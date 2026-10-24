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

/// Outline entries returned at most.
const MAX_OUTLINE_ITEMS: usize = 50_000;
/// Deepest outline nesting followed.
const MAX_OUTLINE_DEPTH: usize = 64;

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
