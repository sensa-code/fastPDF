//! PDF text strings (ISO 32000-2 §7.9.2.2): UTF-16BE or UTF-8 with a byte
//! order mark, otherwise PDFDocEncoding. Used for metadata, outline titles,
//! page-label prefixes and the properties panel, all hostile document text:
//! decoding reads at most [`MAX_TEXT_BYTES`] of a string, never panics, and
//! drops the language escapes a string may carry.

/// Bytes of one text string decoded at most; longer strings are cut (at a
/// character boundary when the encoding allows).
pub(crate) const MAX_TEXT_BYTES: usize = 16 * 1024;

/// The escape that opens and closes a language mark in a Unicode text
/// string.
const ESCAPE: char = '\u{1B}';
/// Characters between the two escapes of a language mark at most: a
/// language and a country code, two bytes each (one UTF-16 unit or two
/// UTF-8 characters per code).
const MAX_MARK_CHARS: usize = 4;

/// Decodes a PDF text string: UTF-16BE or UTF-8 with BOM, otherwise
/// PDFDocEncoding. Only the first [`MAX_TEXT_BYTES`] bytes are read; an odd
/// last byte of UTF-16 is ignored.
pub(crate) fn decode_text_string(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        let (pairs, _) = rest.as_chunks::<2>();
        let mut units: Vec<u16> = pairs
            .iter()
            .take(MAX_TEXT_BYTES / 2)
            .map(|c| u16::from_be_bytes(*c))
            .collect();
        // Never keep half of a surrogate pair that the cut split.
        if pairs.len() > units.len() && units.last().is_some_and(|u| (0xD800..0xDC00).contains(u)) {
            units.pop();
        }
        return without_language_marks(String::from_utf16_lossy(&units));
    }
    if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        let mut end = rest.len().min(MAX_TEXT_BYTES);
        // Cut at the start of a character, not inside one.
        while end < rest.len() && end > 0 && rest[end] & 0xC0 == 0x80 {
            end -= 1;
        }
        return without_language_marks(String::from_utf8_lossy(&rest[..end]).into_owned());
    }
    bytes
        .iter()
        .take(MAX_TEXT_BYTES)
        .map(|&b| pdf_doc_char(b))
        .collect()
}

/// `text` without its language marks (escape, one to four characters,
/// escape); an escape that opens no mark is dropped on its own.
fn without_language_marks(text: String) -> String {
    if !text.contains(ESCAPE) {
        return text;
    }
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while let Some(&c) = chars.get(i) {
        if c != ESCAPE {
            out.push(c);
            i += 1;
            continue;
        }
        let close = chars
            .iter()
            .skip(i + 1)
            .take(MAX_MARK_CHARS + 1)
            .position(|&c| c == ESCAPE);
        i += match close {
            Some(n) if n > 0 => n + 2,
            _ => 1,
        };
    }
    out
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

#[cfg(test)]
mod tests {
    use super::*;

    fn utf16(text: &str) -> Vec<u8> {
        let mut out = vec![0xFE, 0xFF];
        out.extend(text.encode_utf16().flat_map(u16::to_be_bytes));
        out
    }

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
        // An odd last byte of UTF-16 is ignored.
        assert_eq!(decode_text_string(&[0xFE, 0xFF, 0x4E, 0x2D, 0x65]), "中");
        assert_eq!(decode_text_string(&[0xFE, 0xFF]), "");
    }

    #[test]
    fn language_marks_are_dropped() {
        // UTF-16: escape, "zh" and "TW" packed into one unit each, escape.
        let mut bytes = vec![0xFE, 0xFF, 0x00, 0x1B, b'z', b'h', b'T', b'W', 0x00, 0x1B];
        bytes.extend_from_slice(&utf16("第一章")[2..]);
        assert_eq!(decode_text_string(&bytes), "第一章");
        // Language only, in the middle of the text.
        let mut bytes = utf16("a");
        bytes.extend_from_slice(&[0x00, 0x1B, b'e', b'n', 0x00, 0x1B]);
        bytes.extend_from_slice(&utf16("b")[2..]);
        assert_eq!(decode_text_string(&bytes), "ab");
        // UTF-8 (PDF 2.0): two bytes per code.
        assert_eq!(
            decode_text_string(b"\xEF\xBB\xBF\x1Bja\x1B\xE6\x97\xA5\xE6\x9C\xAC"),
            "日本"
        );
        // An escape that opens no mark goes on its own.
        assert_eq!(decode_text_string(&utf16("a\u{1B}bcdefg")), "abcdefg");
        assert_eq!(decode_text_string(&utf16("\u{1B}\u{1B}x")), "x");
    }

    #[test]
    fn long_strings_are_cut_whole() {
        let long = "附".repeat(MAX_TEXT_BYTES);
        let decoded = decode_text_string(&utf16(&long));
        assert_eq!(decoded.chars().count(), MAX_TEXT_BYTES / 2);
        // A surrogate pair split by the cut is dropped, not replaced.
        let emoji = format!("{}\u{1F600}", "a".repeat(MAX_TEXT_BYTES / 2 - 1));
        let decoded = decode_text_string(&utf16(&emoji));
        assert!(!decoded.contains('\u{FFFD}'));
        assert_eq!(decoded.len(), MAX_TEXT_BYTES / 2 - 1);
        // UTF-8 is cut before a character, not inside it.
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice("x".repeat(MAX_TEXT_BYTES - 1).as_bytes());
        bytes.extend_from_slice("附附".as_bytes());
        let decoded = decode_text_string(&bytes);
        assert!(!decoded.contains('\u{FFFD}'));
        assert_eq!(decoded.len(), MAX_TEXT_BYTES - 1);
        // PDFDocEncoding: one character per byte read.
        let decoded = decode_text_string(&vec![b'q'; 3 * MAX_TEXT_BYTES]);
        assert_eq!(decoded.len(), MAX_TEXT_BYTES);
    }
}
