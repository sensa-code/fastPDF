//! A small scanner for PDF object syntax, used on the raw bytes of
//! dictionaries and arrays exactly as they are stored in a source file
//! (`Dict::data`, `Array::data`).
//!
//! It finds indirect references ("12 0 R") so they can be renumbered, and
//! splits a dictionary into entries so single entries can be replaced. The
//! syntax was already accepted by hayro's parser; the scanner only has to
//! agree with it on token boundaries. Strings, comments and names are always
//! skipped as whole tokens, so a ")" or an "R" inside them is never taken
//! for syntax. Malformed input yields `None` or fewer matches, never a
//! panic.

use std::ops::Range;

/// An object reference as written: object number and generation.
pub(crate) type RawRef = (u32, u16);

pub(crate) fn is_whitespace(b: u8) -> bool {
    matches!(b, b'\0' | b'\t' | b'\n' | b'\x0c' | b'\r' | b' ')
}

pub(crate) fn is_delimiter(b: u8) -> bool {
    matches!(
        b,
        b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Token {
    DictStart,
    DictEnd,
    ArrayStart,
    ArrayEnd,
    /// A literal or hexadecimal string.
    String,
    Name,
    /// A number or a keyword (`true`, `null`, `R`, ...).
    Regular,
    /// A stray delimiter.
    Other,
}

/// The next token at or after `pos`, skipping whitespace and comments.
fn next_token(data: &[u8], mut pos: usize) -> Option<(Token, Range<usize>)> {
    loop {
        let b = *data.get(pos)?;
        if is_whitespace(b) {
            pos += 1;
        } else if b == b'%' {
            while pos < data.len() && !matches!(data[pos], b'\n' | b'\r') {
                pos += 1;
            }
        } else {
            break;
        }
    }
    let start = pos;
    let regular_end = |mut p: usize| {
        while p < data.len() && !is_whitespace(data[p]) && !is_delimiter(data[p]) {
            p += 1;
        }
        p
    };
    let next = data.get(pos + 1).copied();
    Some(match data[pos] {
        b'<' if next == Some(b'<') => (Token::DictStart, start..pos + 2),
        b'>' if next == Some(b'>') => (Token::DictEnd, start..pos + 2),
        b'[' => (Token::ArrayStart, start..pos + 1),
        b']' => (Token::ArrayEnd, start..pos + 1),
        b'(' => (Token::String, start..literal_string_end(data, pos)),
        b'<' => {
            let end = data[pos..]
                .iter()
                .position(|&b| b == b'>')
                .map_or(data.len(), |i| pos + i + 1);
            (Token::String, start..end)
        }
        b'/' => (Token::Name, start..regular_end(pos + 1)),
        b')' | b'>' | b'{' | b'}' => (Token::Other, start..pos + 1),
        _ => (Token::Regular, start..regular_end(pos)),
    })
}

/// End (exclusive) of the literal string starting at `pos` ("(").
fn literal_string_end(data: &[u8], pos: usize) -> usize {
    let mut depth = 0usize;
    let mut p = pos;
    while p < data.len() {
        match data[p] {
            b'\\' => {
                p += 2;
                continue;
            }
            b'(' => depth += 1,
            b')' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return p + 1;
                }
            }
            _ => {}
        }
        p += 1;
    }
    data.len()
}

/// A token's text as an integer (optional sign, digits only).
fn integer(token: &[u8]) -> Option<i64> {
    let digits = token
        .strip_prefix(b"+")
        .or_else(|| token.strip_prefix(b"-"))
        .unwrap_or(token);
    if digits.is_empty() || digits.len() > 18 || !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    std::str::from_utf8(token).ok()?.parse().ok()
}

fn as_ref(number: i64, generation: i64) -> Option<RawRef> {
    let number = u32::try_from(number).ok().filter(|&n| n > 0)?;
    let generation = u16::try_from(generation).ok()?;
    Some((number, generation))
}

/// Every indirect reference in `data`, with the span of "n g R".
pub(crate) fn references(data: &[u8]) -> Vec<(Range<usize>, RawRef)> {
    let mut out = Vec::new();
    // The two tokens before the current one: span and integer value.
    let mut older: Option<(Range<usize>, Option<i64>)> = None;
    let mut newer: Option<(Range<usize>, Option<i64>)> = None;
    let mut pos = 0;
    while let Some((kind, span)) = next_token(data, pos) {
        pos = span.end;
        if kind == Token::Regular && &data[span.clone()] == b"R" {
            if let (Some((first, Some(n))), Some((_, Some(g)))) = (&older, &newer)
                && let Some(r) = as_ref(*n, *g)
            {
                out.push((first.start..span.end, r));
            }
            older = None;
            newer = None;
            continue;
        }
        let value = (kind == Token::Regular)
            .then(|| integer(&data[span.clone()]))
            .flatten();
        older = newer.take();
        newer = Some((span, value));
    }
    out
}

/// `data` with every reference replaced by `map`'s answer: `Some(n)`
/// writes "n 0 R", `None` writes "null".
pub(crate) fn remap(data: &[u8], mut map: impl FnMut(RawRef) -> Option<u32>) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + 16);
    let mut last = 0;
    for (span, r) in references(data) {
        out.extend_from_slice(&data[last..span.start]);
        match map(r) {
            Some(n) => out.extend_from_slice(format!("{n} 0 R").as_bytes()),
            None => out.extend_from_slice(b"null"),
        }
        last = span.end;
    }
    out.extend_from_slice(&data[last..]);
    out
}

/// End of the object starting at the first token at or after `pos`.
fn object_end(data: &[u8], pos: usize, depth: usize) -> Option<usize> {
    // Deeper nesting than any real document: refuse instead of recursing.
    if depth > 256 {
        return None;
    }
    let (kind, span) = next_token(data, pos)?;
    match kind {
        Token::DictStart | Token::ArrayStart => {
            let close = if kind == Token::DictStart {
                Token::DictEnd
            } else {
                Token::ArrayEnd
            };
            let mut p = span.end;
            loop {
                let (k, s) = next_token(data, p)?;
                if k == close {
                    return Some(s.end);
                }
                if matches!(k, Token::DictEnd | Token::ArrayEnd) {
                    return None;
                }
                p = object_end(data, s.start, depth + 1)?;
            }
        }
        Token::Regular => {
            // "n g R" is one object.
            if integer(&data[span.clone()]).is_some()
                && let Some((Token::Regular, g)) = next_token(data, span.end)
                && integer(&data[g.clone()]).is_some()
                && let Some((Token::Regular, r)) = next_token(data, g.end)
                && &data[r.clone()] == b"R"
            {
                return Some(r.end);
            }
            Some(span.end)
        }
        Token::String | Token::Name | Token::Other => Some(span.end),
        Token::DictEnd | Token::ArrayEnd => None,
    }
}

/// One top-level entry of a dictionary.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Entry {
    /// The key with `#xx` escapes decoded, without the "/".
    pub key: Vec<u8>,
    /// The value's bytes in the dictionary data.
    pub value: Range<usize>,
}

/// The top-level entries of the dictionary that `data` starts with.
pub(crate) fn dict_entries(data: &[u8]) -> Option<Vec<Entry>> {
    let (kind, open) = next_token(data, 0)?;
    if kind != Token::DictStart {
        return None;
    }
    let mut entries = Vec::new();
    let mut pos = open.end;
    loop {
        let (kind, span) = next_token(data, pos)?;
        match kind {
            Token::DictEnd => return Some(entries),
            Token::Name => {
                let value_start = next_token(data, span.end)?.1.start;
                let value_end = object_end(data, span.end, 0)?;
                entries.push(Entry {
                    key: decode_name(&data[span.start + 1..span.end]),
                    value: value_start..value_end,
                });
                pos = value_end;
            }
            _ => return None,
        }
    }
}

/// The raw value of `key` in the dictionary `data`.
pub(crate) fn dict_value<'d>(data: &'d [u8], key: &[u8]) -> Option<&'d [u8]> {
    dict_entries(data)?
        .into_iter()
        .find(|e| e.key == key)
        .map(|e| &data[e.value])
}

/// A name's bytes with `#xx` escapes decoded.
pub(crate) fn decode_name(raw: &[u8]) -> Vec<u8> {
    let hex = |b: u8| (b as char).to_digit(16).map(|d| d as u8);
    let mut out = Vec::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        if raw[i] == b'#'
            && let (Some(h), Some(l)) = (
                raw.get(i + 1).copied().and_then(hex),
                raw.get(i + 2).copied().and_then(hex),
            )
        {
            out.push(h << 4 | l);
            i += 3;
            continue;
        }
        out.push(raw[i]);
        i += 1;
    }
    out
}

/// `name` written as a PDF name ("/" and `#xx` escapes where needed).
pub(crate) fn encode_name(name: &[u8], out: &mut Vec<u8>) {
    out.push(b'/');
    for &b in name {
        if b == b'#' || !(b'!'..=b'~').contains(&b) || is_delimiter(b) {
            out.extend_from_slice(format!("#{b:02X}").as_bytes());
        } else {
            out.push(b);
        }
    }
}

/// A dictionary rebuilt from `data`: entries whose key is in `drop` are left
/// out, every kept value goes through `map_value`, then `add` is appended.
/// `add` values are written as they are: references in them must already
/// be in the output's numbering.
pub(crate) fn rebuild_dict(
    data: &[u8],
    drop: &[&[u8]],
    add: &[(&[u8], Vec<u8>)],
    mut map_value: impl FnMut(&[u8]) -> Vec<u8>,
) -> Option<Vec<u8>> {
    let entries = dict_entries(data)?;
    let mut out = b"<<".to_vec();
    for entry in &entries {
        let replaced = add.iter().any(|(k, _)| *k == entry.key.as_slice());
        if replaced || drop.contains(&entry.key.as_slice()) {
            continue;
        }
        encode_name(&entry.key, &mut out);
        out.push(b' ');
        out.extend_from_slice(&map_value(&data[entry.value.clone()]));
    }
    for (key, value) in add {
        encode_name(key, &mut out);
        out.push(b' ');
        out.extend_from_slice(value);
    }
    out.extend_from_slice(b">>");
    Some(out)
}

/// The elements of the array that `data` starts with, as byte ranges.
pub(crate) fn array_elements(data: &[u8]) -> Option<Vec<Range<usize>>> {
    let (kind, open) = next_token(data, 0)?;
    if kind != Token::ArrayStart {
        return None;
    }
    let mut out = Vec::new();
    let mut pos = open.end;
    loop {
        let (kind, span) = next_token(data, pos)?;
        if kind == Token::ArrayEnd {
            return Some(out);
        }
        let end = object_end(data, span.start, 0)?;
        out.push(span.start..end);
        pos = end;
    }
}

/// `data` as a single reference, if that is all it is.
pub(crate) fn single_reference(data: &[u8]) -> Option<RawRef> {
    let refs = references(data);
    let [(span, r)] = refs.as_slice() else {
        return None;
    };
    let trimmed = data.iter().position(|b| !is_whitespace(*b)).unwrap_or(0)
        ..data
            .iter()
            .rposition(|b| !is_whitespace(*b))
            .map_or(0, |i| i + 1);
    (*span == trimmed).then_some(*r)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn references_skip_strings_names_and_comments() {
        let data = b"<< /A 1 0 R /B [2 0 R 3 5 R] /C (4 0 R \\) 5 0 R) /D /R \
                    % 6 0 R\n /E <7 0 R> /F 12 7 8 0 R /G 0 0 R /H -1 0 R >>";
        let refs: Vec<RawRef> = references(data).into_iter().map(|(_, r)| r).collect();
        assert_eq!(refs, [(1, 0), (2, 0), (3, 5), (8, 0)]);
        let renumbered = remap(data, |(n, _)| (n != 3).then_some(n * 10));
        let text = String::from_utf8(renumbered).unwrap();
        assert!(text.starts_with("<< /A 10 0 R /B [20 0 R null] /C (4 0 R"));
        assert!(text.contains("/F 12 7 80 0 R"));
    }

    #[test]
    fn dictionaries_split_into_entries() {
        let data = b"<</Type/Page/Parent 2 0 R/Res#6Furces<</Font<</F1 5 0 R>>>>\
                     /MediaBox[0 0 612 792]/Contents[6 0 R 7 0 R]/T(a)b)/U true>>";
        // "(a)b)" ends at the first ")": the value is "(a)" and "b)" is
        // garbage, so the dictionary does not parse.
        assert_eq!(dict_entries(data), None);
        let data = b"<</Type/Page/Parent 2 0 R/Res#6Furces<</Font<</F1 5 0 R>>>>\
                     /MediaBox[0 0 612 792]/Contents[6 0 R 7 0 R]/T(a\\)b)/U true>>";
        let entries = dict_entries(data).unwrap();
        let keys: Vec<&[u8]> = entries.iter().map(|e| e.key.as_slice()).collect();
        assert_eq!(
            keys,
            [
                &b"Type"[..],
                b"Parent",
                b"Resources",
                b"MediaBox",
                b"Contents",
                b"T",
                b"U"
            ]
        );
        assert_eq!(dict_value(data, b"Parent"), Some(&b"2 0 R"[..]));
        assert_eq!(dict_value(data, b"Type"), Some(&b"/Page"[..]));
        assert_eq!(dict_value(data, b"T"), Some(&b"(a\\)b)"[..]));
        let contents = dict_value(data, b"Contents").unwrap();
        let items: Vec<&[u8]> = array_elements(contents)
            .unwrap()
            .into_iter()
            .map(|r| &contents[r])
            .collect();
        assert_eq!(items, [&b"6 0 R"[..], b"7 0 R"]);
        assert_eq!(single_reference(b" 2 0 R "), Some((2, 0)));
        assert_eq!(single_reference(b"[2 0 R]"), None);
    }

    #[test]
    fn rebuilt_dictionaries_replace_drop_and_remap() {
        let data = b"<< /Type /Page /Parent 2 0 R /Rotate 90 /Annots [9 0 R] >>";
        let out = rebuild_dict(
            data,
            &[b"Rotate"],
            &[(b"Parent", b"1 0 R".to_vec()), (b"A B", b"1".to_vec())],
            |v| remap(v, |(n, _)| Some(n + 100)),
        )
        .unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "<</Type /Page/Annots [109 0 R]/Parent 1 0 R/A#20B 1>>"
        );
        assert_eq!(rebuild_dict(b"[1 2]", &[], &[], |v| v.to_vec()), None);
    }

    #[test]
    fn hostile_input_does_not_panic() {
        let deep = format!("<<{}", "/K [".repeat(10_000));
        assert_eq!(dict_entries(deep.as_bytes()), None);
        for data in [
            &b""[..],
            b"<<",
            b"<< /A",
            b"<< /A (unterminated",
            b"<< /A <abc",
            b"<< 1 2 R >>",
            b"[1 0 R",
            b"99999999999999999999 0 R",
            b"4294967296 0 R 1 65536 R",
        ] {
            let _ = dict_entries(data);
            let _ = array_elements(data);
            let _ = references(data);
        }
        // Numbers that do not fit an object number or generation.
        assert!(references(b"99999999999999999999 0 R").is_empty());
        assert!(references(b"4294967296 0 R 1 65536 R").is_empty());
        assert_eq!(decode_name(b"A#2"), b"A#2");
        assert_eq!(decode_name(b"A#20B"), b"A B");
    }
}
