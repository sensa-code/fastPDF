//! The document's last trailer dictionary, which hayro reads but does not
//! expose: it names the information dictionary and the encryption
//! dictionary. Found from `startxref` near the end of the file without
//! walking the cross-reference sections (never `/Prev`, never every entry):
//!
//! * a table: its subsections are skipped by their entry counts (20 bytes
//!   per entry, as hayro reads them) up to `trailer`;
//! * a cross-reference stream: its dictionary;
//! * otherwise (no section at the offset `startxref` names, or a table
//!   that does not read): the last `trailer` within 64 KiB before
//!   `startxref`.
//!
//! Every slice is checked and every window has a fixed size; the dictionary
//! is read with hayro's own parser, without a document (references stay
//! unresolved).

use hayro::hayro_syntax::object::{Dict, FromBytes, Name};

/// How far from the end of the file `startxref` is looked for.
const TAIL: usize = 4096;
/// Bytes a trailer dictionary is read from at most, and how far before
/// `startxref` a `trailer` is looked for.
const WINDOW: usize = 64 * 1024;
/// Subsections of one cross-reference table skipped at most.
const MAX_SUBSECTIONS: usize = 1 << 16;
/// Bytes of one entry of a cross-reference table.
const ENTRY_LEN: usize = 20;

/// The last trailer dictionary of `bytes` (the dictionary of the last
/// cross-reference stream), if it can be found.
pub(super) fn last_trailer(bytes: &[u8]) -> Option<Dict<'_>> {
    let (keyword, offset) = startxref(bytes)?;
    offset
        .and_then(|offset| section_at(bytes, offset))
        .or_else(|| trailer_before(bytes, keyword))
}

/// Where `startxref` is in the last [`TAIL`] bytes, and the offset it
/// names when that is inside the file.
fn startxref(bytes: &[u8]) -> Option<(usize, Option<usize>)> {
    let from = bytes.len().saturating_sub(TAIL);
    let tail = bytes.get(from..)?;
    let at = tail.windows(9).rposition(|w| w == b"startxref")?;
    let rest = tail.get(at + 9..)?;
    let rest = &rest[whitespace(rest)..];
    let digits = rest.iter().take_while(|b| b.is_ascii_digit()).count();
    let offset = std::str::from_utf8(rest.get(..digits.min(20))?)
        .ok()
        .and_then(|d| d.parse::<usize>().ok())
        .filter(|&o| o < bytes.len());
    Some((from + at, offset))
}

/// The trailer of the cross-reference section at `offset`.
fn section_at(bytes: &[u8], offset: usize) -> Option<Dict<'_>> {
    let section = bytes.get(offset..)?;
    let section = &section[whitespace(section)..];
    match section.strip_prefix(b"xref") {
        Some(table) => table_trailer(table),
        None => stream_dict(section),
    }
}

/// The trailer after a cross-reference table (`table` starts after
/// `xref`).
fn table_trailer(table: &[u8]) -> Option<Dict<'_>> {
    let mut pos = 0;
    for _ in 0..MAX_SUBSECTIONS {
        let Some((count, after)) = subsection_header(table, pos) else {
            break;
        };
        pos = ENTRY_LEN
            .checked_mul(count)
            .and_then(|len| after.checked_add(len))
            .filter(|&end| end <= table.len())?;
    }
    let rest = table.get(pos..)?;
    let rest = rest[whitespace(rest)..].strip_prefix(b"trailer")?;
    dict(&rest[whitespace(rest)..])
}

/// `first count` at `pos` (whitespace around it skipped): the count and
/// where its entries start.
fn subsection_header(table: &[u8], pos: usize) -> Option<(usize, usize)> {
    let (_, pos) = number(table, pos)?;
    let (count, pos) = number(table, pos)?;
    let rest = table.get(pos..)?;
    Some((count, pos + whitespace(rest)))
}

/// A non-negative integer at `pos` after whitespace, and where it ends.
fn number(data: &[u8], pos: usize) -> Option<(usize, usize)> {
    let rest = data.get(pos..)?;
    let start = pos + whitespace(rest);
    let digits = data
        .get(start..)?
        .iter()
        .take_while(|b| b.is_ascii_digit())
        .count();
    if digits == 0 || digits > 10 {
        return None;
    }
    let text = std::str::from_utf8(data.get(start..start + digits)?).ok()?;
    Some((text.parse().ok()?, start + digits))
}

/// The dictionary of the cross-reference stream (`n g obj << ... >>`) at
/// the start of `section`.
fn stream_dict(section: &[u8]) -> Option<Dict<'_>> {
    let (_, pos) = number(section, 0)?;
    let (_, pos) = number(section, pos)?;
    let rest = section.get(pos..)?;
    let rest = rest[whitespace(rest)..].strip_prefix(b"obj")?;
    let dict = dict(&rest[whitespace(rest)..])?;
    dict.get::<Name<'_>>(b"Type")
        .is_some_and(|t| t.as_ref() == b"XRef")
        .then_some(dict)
}

/// The last `trailer` dictionary within [`WINDOW`] bytes before `end`.
fn trailer_before(bytes: &[u8], end: usize) -> Option<Dict<'_>> {
    let start = end.saturating_sub(WINDOW);
    let window = bytes.get(start..end)?;
    let at = window.windows(7).rposition(|w| w == b"trailer")?;
    let rest = &window[at + 7..];
    dict(&rest[whitespace(rest)..])
}

/// The dictionary `data` starts with, read from at most [`WINDOW`] bytes.
fn dict(data: &[u8]) -> Option<Dict<'_>> {
    if !data.starts_with(b"<<") {
        return None;
    }
    Dict::from_bytes(data.get(..data.len().min(WINDOW))?)
}

/// Length of the PDF whitespace `data` starts with.
fn whitespace(data: &[u8]) -> usize {
    data.iter()
        .take_while(|b| matches!(b, b'\0' | b'\t' | b'\n' | b'\x0c' | b'\r' | b' '))
        .count()
}

#[cfg(test)]
mod tests {
    use hayro::hayro_syntax::object::{MaybeRef, ObjectIdentifier};

    use super::*;

    /// The object number `/Root` names in the trailer found.
    fn root(bytes: &[u8]) -> Option<i32> {
        match last_trailer(bytes)?.get_raw::<Dict<'_>>(b"Root")? {
            MaybeRef::Ref(r) => Some(ObjectIdentifier::from(r).obj_number),
            MaybeRef::NotRef(_) => None,
        }
    }

    fn table(entries: usize, trailer: &str) -> Vec<u8> {
        let mut out = b"%PDF-1.7\n1 0 obj\n<< >>\nendobj\n".to_vec();
        let start = out.len();
        out.extend_from_slice(format!("xref\n0 {entries}\n").as_bytes());
        for _ in 0..entries {
            out.extend_from_slice(b"0000000000 65535 f\r\n");
        }
        out.extend_from_slice(
            format!("trailer\n{trailer}\nstartxref\n{start}\n%%EOF\n").as_bytes(),
        );
        out
    }

    #[test]
    fn classic_tables_are_skipped_by_their_counts() {
        assert_eq!(root(&table(3, "<< /Size 3 /Root 7 0 R >>")), Some(7));
        // Far more than the window before startxref.
        assert_eq!(
            root(&table(200_000, "<< /Size 200000 /Root 9 0 R >>")),
            Some(9)
        );
        // Two subsections.
        let mut doc = b"%PDF-1.7\n".to_vec();
        let start = doc.len();
        doc.extend_from_slice(b"xref\n0 1\n0000000000 65535 f\r\n5 2\n");
        doc.extend_from_slice(b"0000000009 00000 n\r\n0000000009 00000 n\r\n");
        doc.extend_from_slice(
            format!("trailer <</Root 5 0 R>>\nstartxref {start}\n%%EOF").as_bytes(),
        );
        assert_eq!(root(&doc), Some(5));
    }

    #[test]
    fn cross_reference_streams_give_their_dictionary() {
        let mut doc = b"%PDF-1.7\n".to_vec();
        let start = doc.len();
        doc.extend_from_slice(
            b"12 0 obj\n<< /Type /XRef /Size 13 /Root 3 0 R /W [1 4 2] /Length 0 >>\nstream\n\nendstream\nendobj\n",
        );
        doc.extend_from_slice(format!("startxref\n{start}\n%%EOF\n").as_bytes());
        assert_eq!(root(&doc), Some(3));
        // An object that is not a cross-reference stream.
        let wrong = String::from_utf8_lossy(&doc).replace("/Type /XRef", "/Type /Page");
        assert_eq!(root(wrong.as_bytes()), None);
    }

    #[test]
    fn broken_offsets_fall_back_to_the_last_trailer() {
        let doc = String::from_utf8_lossy(&table(2, "<< /Root 4 0 R >>")).into_owned();
        let (head, offset) = doc.rsplit_once("startxref\n").unwrap_or_default();
        let rest = offset.trim_start_matches(|c: char| c.is_ascii_digit());
        // startxref past the end of the file, or at no section.
        for wrong in ["999999999", "3"] {
            let broken = format!("{head}startxref\n{wrong}{rest}");
            assert_eq!(root(broken.as_bytes()), Some(4), "{wrong}");
        }
        // An entry count that overruns the file.
        let lying = doc.replace("xref\n0 2\n", "xref\n0 9999\n");
        assert_eq!(root(lying.as_bytes()), Some(4));
    }

    #[test]
    fn nothing_is_found_without_its_keywords() {
        assert_eq!(root(b""), None);
        assert_eq!(root(b"%PDF-1.7\n1 0 obj << >> endobj\n%%EOF"), None);
        // No trailer keyword after the table.
        let doc = table(2, "<< /Root 4 0 R >>");
        let no_trailer = String::from_utf8_lossy(&doc).replace("trailer", "tailer!");
        assert_eq!(root(no_trailer.as_bytes()), None);
        // A trailer that is not a dictionary.
        let not_dict = String::from_utf8_lossy(&doc).replace("<< /Root 4 0 R >>", "[/Root 4 0 R]");
        assert_eq!(root(not_dict.as_bytes()), None);
        // startxref without a number, and no trailer.
        assert_eq!(root(b"%PDF-1.7\nstartxref\n%%EOF"), None);
    }
}
