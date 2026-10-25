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
    out.extend_from_slice(format!("trailer\n{trailer}\nstartxref\n{start}\n%%EOF\n").as_bytes());
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
    doc.extend_from_slice(format!("trailer <</Root 5 0 R>>\nstartxref {start}\n%%EOF").as_bytes());
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
fn comments_between_tokens_are_skipped() {
    let mut doc = b"%PDF-1.7\n".to_vec();
    let start = doc.len();
    doc.extend_from_slice(
        b"12 % number\n0 obj % a comment\r\n% another\n<< /Type /XRef /Root 3 0 R >>\nstream\n\nendstream\nendobj\n",
    );
    doc.extend_from_slice(format!("startxref\n{start}\n%%EOF\n").as_bytes());
    assert_eq!(root(&doc), Some(3));
    let table = String::from_utf8_lossy(&table(1, "<< /Root 4 0 R >>"))
        .replace("trailer\n", "trailer % the trailer\n");
    assert_eq!(root(table.as_bytes()), Some(4));
    assert_eq!(whitespace(b" % open"), 7);
    assert_eq!(whitespace(b"%\n%\r x"), 5);
}

/// A file whose last section is the cross-reference stream `12 0 obj`
/// (with `/Root 3 0 R`), and where `startxref` points.
fn stream_file(startxref: &str) -> Vec<u8> {
    let mut doc = b"%PDF-1.7\n1 0 obj\n<< /Type /XRef /Root 1 0 R >>\nendobj\n".to_vec();
    doc.extend_from_slice(b"7 0 obj\n<< /Length 3 >>\nstream\nobj\nendstream\nendobj\n");
    doc.extend_from_slice(
        b"12 0 obj\n<< /Type /XRef /Size 13 /Root 3 0 R /W [1 4 2] /Length 0 >>\nstream\n\nendstream\nendobj\n",
    );
    doc.extend_from_slice(format!("startxref\n{startxref}\n%%EOF\n").as_bytes());
    doc
}

#[test]
fn broken_offsets_fall_back_to_the_last_stream() {
    // Past the end of the file, at no section, or missing.
    for wrong in ["999999999", "3", ""] {
        assert_eq!(root(&stream_file(wrong)), Some(3), "{wrong:?}");
    }
    // A table section after the stream wins over it, and the other way.
    let mut both = stream_file("999999999");
    both.truncate(both.len() - "startxref\n999999999\n%%EOF\n".len());
    both.extend_from_slice(b"trailer\n<< /Root 4 0 R >>\nstartxref\n999999999\n%%EOF\n");
    assert_eq!(root(&both), Some(4));
    let mut both = b"%PDF-1.7\ntrailer\n<< /Root 4 0 R >>\n".to_vec();
    both.extend_from_slice(&stream_file("1")[9..]);
    assert_eq!(root(&both), Some(3));
    assert_eq!(object_header_start(b"x 12 0 obj", 7), Some(2));
    assert_eq!(object_header_start(b"12 0obj", 4), None);
    assert_eq!(object_header_start(b"a 0 obj", 4), None);
    assert_eq!(object_header_start(b"obj", 0), None);
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
