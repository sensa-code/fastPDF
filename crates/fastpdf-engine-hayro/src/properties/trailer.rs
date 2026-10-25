//! The document's last trailer dictionary, which hayro reads but does not
//! expose: it names the information dictionary and the encryption
//! dictionary. Found from `startxref` near the end of the file without
//! walking the cross-reference sections (never `/Prev`, never every entry):
//!
//! * a table: its subsections are skipped by their entry counts (20 bytes
//!   per entry, as hayro reads them) up to `trailer`;
//! * a cross-reference stream: its dictionary;
//! * otherwise (no section at the offset `startxref` names, or a table
//!   that does not read): the last `trailer` or cross-reference stream
//!   object within 64 KiB before `startxref`, whichever is later.
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
        .or_else(|| last_section_before(bytes, keyword))
}

/// The last trailer or cross-reference stream within [`WINDOW`] bytes
/// before `end`.
fn last_section_before(bytes: &[u8], end: usize) -> Option<Dict<'_>> {
    let start = end.saturating_sub(WINDOW);
    let window = bytes.get(start..end)?;
    match (trailer_before(window), stream_before(window)) {
        (Some((t, table)), Some((s, stream))) => Some(if t > s { table } else { stream }),
        (table, stream) => table.or(stream).map(|(_, dict)| dict),
    }
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

/// The last `trailer` dictionary in `window`, and where it is.
fn trailer_before(window: &[u8]) -> Option<(usize, Dict<'_>)> {
    let at = window.windows(7).rposition(|w| w == b"trailer")?;
    let rest = &window[at + 7..];
    Some((at, dict(&rest[whitespace(rest)..])?))
}

/// The last cross-reference stream object (`n g obj << /Type /XRef ... >>`)
/// in `window`, and where it is. Only the last few `obj` keywords are
/// tried: the stream is the last object a writer puts before `startxref`.
fn stream_before(window: &[u8]) -> Option<(usize, Dict<'_>)> {
    const TRIES: usize = 8;
    let mut end = window.len();
    for _ in 0..TRIES {
        let at = window.get(..end)?.windows(3).rposition(|w| w == b"obj")?;
        end = at;
        let Some(start) = object_header_start(window, at) else {
            continue;
        };
        if let Some(dict) = window.get(start..).and_then(stream_dict) {
            return Some((start, dict));
        }
    }
    None
}

/// Where `n g ` starts when `obj` at `at` follows an object number and a
/// generation.
fn object_header_start(data: &[u8], at: usize) -> Option<usize> {
    let mut pos = at;
    for _ in 0..2 {
        let space = data[..pos]
            .iter()
            .rev()
            .take_while(|b| matches!(b, b'\0' | b'\t' | b'\n' | b'\x0c' | b'\r' | b' '))
            .count();
        let digits_end = pos - space;
        let digits = data[..digits_end]
            .iter()
            .rev()
            .take_while(|b| b.is_ascii_digit())
            .count();
        if space == 0 || digits == 0 || digits > 10 {
            return None;
        }
        pos = digits_end - digits;
    }
    Some(pos)
}

/// The dictionary `data` starts with, read from at most [`WINDOW`] bytes.
fn dict(data: &[u8]) -> Option<Dict<'_>> {
    if !data.starts_with(b"<<") {
        return None;
    }
    Dict::from_bytes(data.get(..data.len().min(WINDOW))?)
}

/// Length of the PDF whitespace and comments `data` starts with.
fn whitespace(data: &[u8]) -> usize {
    let mut at = 0;
    loop {
        at += data
            .get(at..)
            .unwrap_or_default()
            .iter()
            .take_while(|b| matches!(b, b'\0' | b'\t' | b'\n' | b'\x0c' | b'\r' | b' '))
            .count();
        if data.get(at) != Some(&b'%') {
            return at;
        }
        at += data[at..]
            .iter()
            .take_while(|b| !matches!(b, b'\n' | b'\r'))
            .count();
    }
}

#[cfg(test)]
mod tests;
