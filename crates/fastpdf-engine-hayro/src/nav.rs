//! Document outline, link annotations and metadata, read directly with
//! hayro-syntax's object API (hayro itself does not interpret them).
//! Everything is iterative and bounded: outlines and name trees in hostile
//! files can be cyclic or absurdly large.

use std::collections::HashSet;

use fastpdf_engine_api::{
    Destination, DestinationView, DocumentMetadata, EngineError, Link, LinkTarget, OutlineItem,
    PageIndex,
};
use hayro::hayro_syntax::object::{
    Array, DateTime, Dict, MaybeRef, Name, Object, ObjectIdentifier, Rect as PdfRect,
    String as PdfString,
};
use hayro::hayro_syntax::{Pdf, PdfVersion};
use hayro::kurbo::Rect;

use crate::document::{DocInner, Generation};
use crate::geometry::PageGeom;

/// Outline entries returned at most.
const MAX_OUTLINE_ITEMS: usize = 50_000;
/// Deepest outline nesting followed.
const MAX_OUTLINE_DEPTH: usize = 64;
/// Name-tree nodes visited for one named destination.
const MAX_NAME_TREE_NODES: usize = 10_000;
/// Link annotations returned per page.
const MAX_LINKS: usize = 10_000;

pub(crate) fn metadata(pdf: &Pdf, encrypted: bool) -> DocumentMetadata {
    let m = pdf.metadata();
    let text = |v: &Option<Vec<u8>>| {
        v.as_deref()
            .map(decode_text_string)
            .map(|s| s.trim_matches('\0').to_owned())
            .filter(|s| !s.is_empty())
    };
    DocumentMetadata {
        title: text(&m.title),
        author: text(&m.author),
        subject: text(&m.subject),
        keywords: text(&m.keywords),
        creator: text(&m.creator),
        producer: text(&m.producer),
        creation_date: m.creation_date.map(format_date),
        modification_date: m.modification_date.map(format_date),
        pdf_version: Some(version_string(pdf.version()).to_owned()),
        encrypted,
    }
}

fn version_string(v: PdfVersion) -> &'static str {
    match v {
        PdfVersion::Pdf10 => "1.0",
        PdfVersion::Pdf11 => "1.1",
        PdfVersion::Pdf12 => "1.2",
        PdfVersion::Pdf13 => "1.3",
        PdfVersion::Pdf14 => "1.4",
        PdfVersion::Pdf15 => "1.5",
        PdfVersion::Pdf16 => "1.6",
        PdfVersion::Pdf17 => "1.7",
        PdfVersion::Pdf20 => "2.0",
    }
}

/// hayro parses dates; the API wants PDF date strings back.
fn format_date(d: DateTime) -> String {
    let sign = if d.utc_offset_hour < 0 { '-' } else { '+' };
    format!(
        "D:{:04}{:02}{:02}{:02}{:02}{:02}{}{:02}'{:02}'",
        d.year,
        d.month,
        d.day,
        d.hour,
        d.minute,
        d.second,
        sign,
        d.utc_offset_hour.unsigned_abs(),
        d.utc_offset_minute
    )
}

/// Decodes a PDF text string: UTF-16BE or UTF-8 with BOM, otherwise
/// PDFDocEncoding.
pub(crate) fn decode_text_string(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        let (pairs, _) = rest.as_chunks::<2>();
        let units: Vec<u16> = pairs.iter().map(|c| u16::from_be_bytes(*c)).collect();
        return String::from_utf16_lossy(&units);
    }
    if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8_lossy(rest).into_owned();
    }
    bytes.iter().map(|&b| pdf_doc_char(b)).collect()
}

/// PDFDocEncoding (ISO 32000-2, Annex D) → Unicode.
fn pdf_doc_char(b: u8) -> char {
    const HIGH: [char; 33] = [
        '\u{2022}', '\u{2020}', '\u{2021}', '\u{2026}', '\u{2014}', '\u{2013}', '\u{0192}',
        '\u{2044}', '\u{2039}', '\u{203A}', '\u{2212}', '\u{2030}', '\u{201E}', '\u{201C}',
        '\u{201D}', '\u{2018}', '\u{2019}', '\u{201A}', '\u{2122}', '\u{FB01}', '\u{FB02}',
        '\u{0141}', '\u{0152}', '\u{0160}', '\u{0178}', '\u{017D}', '\u{0131}', '\u{0142}',
        '\u{0153}', '\u{0161}', '\u{017E}', '\u{FFFD}', '\u{20AC}',
    ];
    const LOW: [char; 8] = [
        '\u{02D8}', '\u{02C7}', '\u{02C6}', '\u{02D9}', '\u{02DD}', '\u{02DB}', '\u{02DA}',
        '\u{02DC}',
    ];
    match b {
        0x18..=0x1F => LOW[usize::from(b - 0x18)],
        0x80..=0xA0 => HIGH[usize::from(b - 0x80)],
        0xAD => '\u{FFFD}',
        _ => char::from(b),
    }
}

/// Converts PDF destinations to FastPDF destinations.
struct DestResolver<'a, 'p> {
    doc: &'a DocInner,
    generation: &'p Generation,
    catalog: Option<Dict<'p>>,
}

impl<'a, 'p> DestResolver<'a, 'p> {
    fn new(doc: &'a DocInner, generation: &'p Generation) -> Self {
        let xref = generation.pdf.xref();
        Self {
            doc,
            generation,
            catalog: xref.get::<Dict<'p>>(xref.root_id()),
        }
    }

    fn page_geom(&self, page: u32) -> Option<PageGeom> {
        let pages = self.generation.pdf.pages();
        pages.get(page as usize).map(PageGeom::from_page)
    }

    /// A destination value: an explicit array, or a name/string to look up.
    fn resolve(&self, value: &Object<'p>) -> Option<Destination> {
        match value {
            Object::Array(a) => self.explicit(a),
            Object::Name(n) => self.named(n.as_ref()),
            Object::String(s) => self.named(s.as_bytes()),
            // `<< /D [...] >>` form found in /Dests dictionaries.
            Object::Dict(d) => d.get::<Array<'p>>(b"D").and_then(|a| self.explicit(&a)),
            _ => None,
        }
    }

    fn explicit(&self, array: &Array<'p>) -> Option<Destination> {
        let mut items = array.raw_iter();
        let page = match items.next()? {
            MaybeRef::Ref(r) => {
                let id: ObjectIdentifier = r.into();
                *self.doc.page_ids(self.generation).get(&id)?
            }
            // Remote-style destinations use a page number.
            MaybeRef::NotRef(Object::Number(n)) => {
                let i = n.as_f64();
                if !(0.0..f64::from(self.doc.page_count())).contains(&i) {
                    return None;
                }
                i as u32
            }
            MaybeRef::NotRef(_) => return None,
        };
        let geom = self.page_geom(page)?;
        let kind = match items.next() {
            Some(MaybeRef::NotRef(Object::Name(n))) => n.as_str().to_owned(),
            _ => "Fit".to_owned(),
        };
        let nums: Vec<Option<f64>> = items
            .map(|item| match item {
                MaybeRef::NotRef(Object::Number(n)) => Some(n.as_f64()),
                _ => None,
            })
            .collect();
        let num = |i: usize| nums.get(i).copied().flatten().filter(|v| v.is_finite());
        let x = |v: f64| geom.page_point(v, 0.0).0;
        let y = |v: f64| geom.page_point(0.0, v).1;
        let view = match kind.as_str() {
            "XYZ" => DestinationView::Xyz {
                left: num(0).map(x),
                top: num(1).map(y),
                zoom: num(2).filter(|z| *z > 0.0).map(|z| z as f32),
            },
            "FitH" | "FitBH" => DestinationView::FitWidth { top: num(0).map(y) },
            "FitV" | "FitBV" => DestinationView::FitHeight {
                left: num(0).map(x),
            },
            "FitR" => match (num(0), num(1), num(2), num(3)) {
                (Some(l), Some(b), Some(r), Some(t)) => {
                    DestinationView::FitRect(geom.page_rect(Rect::new(l, b, r, t)))
                }
                _ => DestinationView::Fit,
            },
            _ => DestinationView::Fit,
        };
        Some(Destination {
            page: PageIndex::new(page),
            view,
        })
    }

    fn named(&self, name: &[u8]) -> Option<Destination> {
        let catalog = self.catalog.as_ref()?;
        // PDF 1.1 style: /Dests dictionary in the catalog.
        if let Some(dests) = catalog.get::<Dict<'p>>(b"Dests")
            && let Some(value) = dests.get::<Object<'p>>(name)
            && let Some(d) = self.resolve_direct(&value)
        {
            return Some(d);
        }
        // PDF 1.2+: /Names /Dests name tree.
        let tree = catalog
            .get::<Dict<'p>>(b"Names")?
            .get::<Dict<'p>>(b"Dests")?;
        let value = name_tree_lookup(&tree, name)?;
        self.resolve_direct(&value)
    }

    /// Like `resolve` but never follows another name (no lookup loops).
    fn resolve_direct(&self, value: &Object<'p>) -> Option<Destination> {
        match value {
            Object::Array(a) => self.explicit(a),
            Object::Dict(d) => d.get::<Array<'p>>(b"D").and_then(|a| self.explicit(&a)),
            _ => None,
        }
    }

    /// Target of an action dictionary.
    fn action(&self, action: &Dict<'p>) -> Option<LinkTarget> {
        let kind = action.get::<Name<'p>>(b"S")?;
        match kind.as_str() {
            "GoTo" => {
                let d = action.get::<Object<'p>>(b"D")?;
                self.resolve(&d).map(LinkTarget::Internal)
            }
            "URI" => action
                .get::<PdfString<'p>>(b"URI")
                .map(|u| LinkTarget::Uri(String::from_utf8_lossy(u.as_bytes()).into_owned())),
            _ => Some(LinkTarget::Unsupported),
        }
    }
}

/// Looks `key` up in a name tree, iteratively and with a node budget.
fn name_tree_lookup<'p>(root: &Dict<'p>, key: &[u8]) -> Option<Object<'p>> {
    let mut stack = vec![root.clone()];
    let mut seen: HashSet<ObjectIdentifier> = HashSet::new();
    let mut visited = 0;
    while let Some(node) = stack.pop() {
        visited += 1;
        if visited > MAX_NAME_TREE_NODES {
            return None;
        }
        if let Some(id) = node.obj_id()
            && !seen.insert(id)
        {
            continue;
        }
        if let Some(limits) = node.get::<Array<'p>>(b"Limits") {
            let mut it = limits.iter::<PdfString<'p>>();
            if let (Some(lo), Some(hi)) = (it.next(), it.next())
                && (key < lo.as_bytes() || key > hi.as_bytes())
            {
                continue;
            }
        }
        if let Some(names) = node.get::<Array<'p>>(b"Names") {
            let mut it = names.flex_iter();
            while let Some(k) = it.next::<PdfString<'p>>() {
                let value = it.next::<Object<'p>>();
                if k.as_bytes() == key {
                    return value;
                }
            }
        }
        if let Some(kids) = node.get::<Array<'p>>(b"Kids") {
            stack.extend(kids.iter::<Dict<'p>>());
        }
    }
    None
}

pub(crate) fn outline(doc: &DocInner, generation: &Generation) -> Vec<OutlineItem> {
    let resolver = DestResolver::new(doc, generation);
    let Some(first) = resolver
        .catalog
        .as_ref()
        .and_then(|c| c.get::<Dict<'_>>(b"Outlines"))
        .and_then(|o| o.get::<Dict<'_>>(b"First"))
    else {
        return Vec::new();
    };

    // Iterative walk: each level is (next sibling to visit, finished items).
    struct Level<'p> {
        next: Option<Dict<'p>>,
        items: Vec<OutlineItem>,
        pending: Option<OutlineItem>,
    }
    let mut seen: HashSet<ObjectIdentifier> = HashSet::new();
    let mut count = 0usize;
    let mut levels = vec![Level {
        next: Some(first),
        items: Vec::new(),
        pending: None,
    }];
    loop {
        let depth = levels.len();
        let Some(level) = levels.last_mut() else {
            break;
        };
        let next = level.next.take().filter(|d| match d.obj_id() {
            Some(id) => seen.insert(id),
            None => true,
        });
        match next {
            Some(node) if count < MAX_OUTLINE_ITEMS => {
                count += 1;
                level.next = node.get::<Dict<'_>>(b"Next");
                let item = outline_item(&resolver, &node);
                match node.get::<Dict<'_>>(b"First") {
                    Some(child) if depth < MAX_OUTLINE_DEPTH => {
                        levels.push(Level {
                            next: Some(child),
                            items: Vec::new(),
                            pending: Some(item),
                        });
                    }
                    _ => level.items.push(item),
                }
            }
            _ => {
                let Some(done) = levels.pop() else { break };
                match levels.last_mut() {
                    Some(parent) => {
                        if let Some(mut item) = done.pending {
                            item.children = done.items;
                            parent.items.push(item);
                        }
                    }
                    None => return done.items,
                }
            }
        }
    }
    Vec::new()
}

fn outline_item(resolver: &DestResolver<'_, '_>, node: &Dict<'_>) -> OutlineItem {
    let title = node
        .get::<PdfString<'_>>(b"Title")
        .map(|t| decode_text_string(t.as_bytes()))
        .unwrap_or_default();
    let mut item = OutlineItem {
        title,
        open: node.get::<i64>(b"Count").is_some_and(|c| c > 0),
        ..OutlineItem::default()
    };
    if let Some(dest) = node.get::<Object<'_>>(b"Dest") {
        item.destination = resolver.resolve(&dest);
    } else if let Some(action) = node.get::<Dict<'_>>(b"A") {
        match resolver.action(&action) {
            Some(LinkTarget::Internal(d)) => item.destination = Some(d),
            Some(LinkTarget::Uri(u)) => item.uri = Some(u),
            _ => {}
        }
    }
    item
}

pub(crate) fn links(
    doc: &DocInner,
    generation: &Generation,
    page: PageIndex,
) -> Result<Vec<Link>, EngineError> {
    let pages = generation.pdf.pages();
    let p = pages
        .get(page.as_usize())
        .ok_or(EngineError::PageOutOfRange {
            page,
            page_count: doc.page_count(),
        })?;
    let geom = PageGeom::from_page(p);
    let resolver = DestResolver::new(doc, generation);
    let mut out = Vec::new();
    let Some(annots) = p.raw().get::<Array<'_>>(b"Annots") else {
        return Ok(out);
    };
    for annot in annots.iter::<Dict<'_>>().take(MAX_LINKS) {
        if annot
            .get::<Name<'_>>(b"Subtype")
            .is_none_or(|s| s.as_str() != "Link")
        {
            continue;
        }
        let Some(rect) = annot.get::<PdfRect>(b"Rect") else {
            continue;
        };
        let target = if let Some(dest) = annot.get::<Object<'_>>(b"Dest") {
            resolver.resolve(&dest).map(LinkTarget::Internal)
        } else if let Some(action) = annot.get::<Dict<'_>>(b"A") {
            resolver.action(&action)
        } else {
            None
        };
        out.push(Link {
            bounds: geom.page_rect(Rect::new(rect.x0, rect.y0, rect.x1, rect.y1)),
            target: target.unwrap_or(LinkTarget::Unsupported),
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_strings_decode() {
        assert_eq!(decode_text_string(b"Hello"), "Hello");
        assert_eq!(
            decode_text_string(&[0xFE, 0xFF, 0x4E, 0x2D, 0x65, 0x87]),
            "中文"
        );
        assert_eq!(decode_text_string(&[0xEF, 0xBB, 0xBF, b'a']), "a");
        assert_eq!(
            decode_text_string(&[0x80, 0x92, 0xA0]),
            "\u{2022}\u{2122}\u{20AC}"
        );
    }

    #[test]
    fn dates_format_like_pdf() {
        let d = DateTime {
            year: 2026,
            month: 10,
            day: 4,
            hour: 8,
            minute: 5,
            second: 9,
            utc_offset_hour: -3,
            utc_offset_minute: 30,
        };
        assert_eq!(format_date(d), "D:20261004080509-03'30'");
    }
}
