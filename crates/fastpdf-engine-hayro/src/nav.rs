//! Document outline and metadata, read directly with hayro-syntax's object
//! API (hayro itself does not interpret them). Everything is iterative and
//! bounded: outlines in hostile files can be cyclic or absurdly large.
//! Link annotations are read in `crate::links`, the targets of both in
//! `crate::dest`.

use std::collections::HashSet;

use fastpdf_engine_api::{DocumentMetadata, LinkTarget, OutlineItem};
use hayro::hayro_syntax::object::{DateTime, Dict, ObjectIdentifier, String as PdfString};
use hayro::hayro_syntax::{Pdf, PdfVersion};

use crate::dest::DestResolver;
use crate::document::{DocInner, Generation};
use crate::text_string::decode_text_string;

/// Outline entries returned at most.
const MAX_OUTLINE_ITEMS: usize = 50_000;
/// Deepest outline nesting followed.
const MAX_OUTLINE_DEPTH: usize = 64;

pub(crate) fn metadata(pdf: &Pdf, encrypted: bool) -> DocumentMetadata {
    let m = pdf.metadata();
    let text = |v: &Option<Vec<u8>>| v.as_deref().and_then(info_text);
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

/// A text string of the document information dictionary: decoded, without
/// surrounding NULs, `None` when nothing is left.
pub(crate) fn info_text(raw: &[u8]) -> Option<String> {
    let text = decode_text_string(raw);
    let trimmed = text.trim_matches('\0');
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
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

pub(crate) fn outline(doc: &DocInner, generation: &Generation) -> Vec<OutlineItem> {
    let mut resolver = DestResolver::new(doc, generation);
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
                let item = outline_item(&mut resolver, &node);
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

fn outline_item<'p>(resolver: &mut DestResolver<'_, 'p>, node: &Dict<'p>) -> OutlineItem {
    let title = node
        .get::<PdfString<'_>>(b"Title")
        .map(|t| decode_text_string(t.as_bytes()))
        .unwrap_or_default();
    let mut item = OutlineItem {
        title,
        open: node.get::<i64>(b"Count").is_some_and(|c| c > 0),
        ..OutlineItem::default()
    };
    match resolver.target(node) {
        Some(LinkTarget::Internal(d)) => item.destination = Some(d),
        Some(LinkTarget::Uri(u)) => item.uri = Some(u),
        _ => {}
    }
    item
}

#[cfg(test)]
mod tests {
    use super::*;

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
