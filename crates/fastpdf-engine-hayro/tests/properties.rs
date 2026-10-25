//! Document properties (`EngineDocument::properties`): the information
//! dictionary as written, encryption and permissions, form and signature
//! counts, tagging and linearization, from built documents and (when they
//! are generated) the fixtures of `tools/fixtures/fxlib/cat_properties.py`.

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use common::{PageSpec, PdfBuilder, build_full, build_with, fixture, open, open_with};
use fastpdf_engine_api::{
    DocumentProperties, EncryptionMethod, EngineDocument, FormSummary, Permissions,
    PrintPermission, ResourceLimits,
};

/// Object number of the first object a `document` hook adds: catalog, page
/// tree, font, then the page's content and dictionary come first.
const FIRST: u32 = 6;

fn page() -> PageSpec {
    PageSpec::new([0.0, 0.0, 595.28, 841.89], "")
}

/// A one-page document with `catalog_extra` in its catalog and `objects`
/// added in order from object [`FIRST`]; `trailer` goes into the trailer.
fn document(catalog_extra: &str, objects: &[&str], trailer: &str) -> Vec<u8> {
    build_full(&[page()], catalog_extra, |b| {
        for (i, body) in objects.iter().enumerate() {
            assert_eq!(b.add(*body), FIRST + i as u32);
        }
        b.trailer(trailer);
    })
}

fn properties(bytes: Vec<u8>) -> DocumentProperties {
    open(bytes).properties().expect("properties")
}

#[test]
fn information_is_read_in_every_encoding_with_dates_as_written() {
    let info = concat!(
        "<< /Title <FEFF001B7A685457001B7B2C4E007AE0>",
        " /Author (\\215FastPDF\\216 fixture)",
        " /Keywords <EFBBBFE5B1ACE680A7>",
        " /CreationDate (D:20261024153000-00'30')",
        " /ModDate (20200101) >>"
    );
    let bytes = document("", &[info], "/Info 6 0 R");
    let doc = open(bytes);
    let p = doc.properties().unwrap();
    let m = &p.metadata;
    assert_eq!(
        m.title.as_deref(),
        Some("第一章"),
        "language escape dropped"
    );
    assert_eq!(m.author.as_deref(), Some("\u{201C}FastPDF\u{201D} fixture"));
    assert_eq!(m.keywords.as_deref(), Some("屬性"));
    assert_eq!(m.creation_date.as_deref(), Some("D:20261024153000-00'30'"));
    assert_eq!(m.modification_date.as_deref(), Some("20200101"));
    assert!(!m.encrypted);
    assert_eq!(p.security, None);
    // metadata() keeps hayro's reading: the sign of the offset is lost and
    // a date without "D:" is not one.
    let plain = doc.metadata().unwrap();
    assert_eq!(plain.title, m.title);
    assert_eq!(
        plain.creation_date.as_deref(),
        Some("D:20261024153000+00'30'")
    );
    assert_eq!(plain.modification_date, None);
}

#[test]
fn odd_information_dictionaries_say_less() {
    // A stream answers with its dictionary; a number or nothing is no
    // information at all.
    let stream = build_full(&[page()], "", |b| {
        let n = b.add_stream("/Title (Info as a stream)", b"data".to_vec());
        b.trailer(&format!("/Info {n} 0 R"));
    });
    let p = properties(stream);
    assert_eq!(p.metadata.title.as_deref(), Some("Info as a stream"));
    for trailer in ["/Info 6 0 R", "/Info 99 0 R", "/Info [1 2]", ""] {
        let p = properties(document("", &["42"], trailer));
        assert_eq!(p.metadata.title, None, "{trailer}");
        assert_eq!(p.metadata.creation_date, None, "{trailer}");
    }
}

#[test]
fn rc4_security_and_permissions() {
    let bytes = build_with(&[page()], |b| {
        b.encrypt_rc4_with_permissions("", "owner", -3904);
    });
    let p = properties(bytes);
    assert!(p.metadata.encrypted);
    let s = p.security.expect("security");
    assert_eq!(s.handler.as_deref(), Some("Standard"));
    assert_eq!(s.method, EncryptionMethod::Rc4 { key_bits: 40 });
    assert_eq!((s.version, s.revision), (Some(1), Some(2)));
    let permissions = s.permissions.expect("permissions");
    assert_eq!(permissions, Permissions::from_p(-3904, 2));
    assert_eq!(permissions.printing(), PrintPermission::NotAllowed);
    assert!(!permissions.copy() && !permissions.modify());
    assert!(s.encrypt_metadata);
    assert!(!s.user_password, "opened without a password");

    // Opened with the user password.
    let bytes = build_with(&[page()], |b| {
        b.encrypt_rc4_with_permissions("user", "owner", -4);
    });
    let doc = open_with(bytes, Some("user"), ResourceLimits::default()).unwrap();
    let s = doc.properties().unwrap().security.expect("security");
    assert!(s.user_password);
    assert!(s.permissions.is_some_and(Permissions::all_allowed));
}

/// The form of `forms_test`: a text field, an unsigned and a signed
/// signature field, a parent whose two named kids inherit /FT /Sig (one
/// signed with a direct /V, one with /V null), and a text field with two
/// widgets.
const FORM: &[&str] = &[
    "<< /Fields [7 0 R 8 0 R 9 0 R 10 0 R 13 0 R] >>",
    "<< /FT /Tx /T (name) >>",
    "<< /FT /Sig /T (approval) >>",
    "<< /FT /Sig /T (author) /V 16 0 R >>",
    "<< /FT /Sig /T (witnesses) /Kids [11 0 R 12 0 R] >>",
    "<< /T (a) /Parent 10 0 R /V << /Type /Sig >> >>",
    "<< /T (b) /Parent 10 0 R /V null >>",
    "<< /FT /Tx /T (date) /Kids [14 0 R 15 0 R] >>",
    "<< /Subtype /Widget /Parent 13 0 R >>",
    "<< /Subtype /Widget /Parent 13 0 R >>",
    "<< /Type /Sig /Contents <00> >>",
];

#[test]
fn form_fields_and_signatures_are_counted() {
    let p = properties(document("/AcroForm 6 0 R", FORM, ""));
    assert_eq!(
        p.forms,
        FormSummary {
            fields: 6,
            signature_fields: 4,
            signatures: 2,
            xfa: false,
            truncated: false,
        }
    );
    // An XFA part is reported.
    let mut xfa = FORM.to_vec();
    xfa[0] = "<< /Fields [7 0 R] /XFA [] >>";
    let p = properties(document("/AcroForm 6 0 R", &xfa, ""));
    assert!(p.forms.xfa);
    assert_eq!(p.forms.fields, 1);
    // An indirect /FT, shared by two fields; a /V that is a string is no
    // signature.
    let indirect = [
        "<< /Fields [7 0 R 8 0 R] >>",
        "<< /T (x) /FT 9 0 R /V << /Type /Sig >> >>",
        "<< /T (y) /FT 9 0 R /V (not a signature) >>",
        "/Sig",
    ];
    let forms = properties(document("/AcroForm 6 0 R", &indirect, "")).forms;
    assert_eq!(
        (forms.fields, forms.signature_fields, forms.signatures),
        (2, 2, 1)
    );
    // /V is inherited like /FT (null is no value); a kid's own /V wins.
    let inherited = [
        "<< /Fields [7 0 R] >>",
        "<< /FT /Sig /T (p) /V << /Type /Sig >> /Kids [8 0 R 9 0 R 10 0 R] >>",
        "<< /T (a) >>",
        "<< /T (b) /V null >>",
        "<< /T (c) /V (text) >>",
    ];
    let forms = properties(document("/AcroForm 6 0 R", &inherited, "")).forms;
    assert_eq!(
        (forms.fields, forms.signature_fields, forms.signatures),
        (3, 3, 2)
    );
    // No form at all.
    assert_eq!(
        properties(document("", &[], "")).forms,
        FormSummary::default()
    );
    // An /XFA of null is no XFA part.
    let mut no_xfa = FORM.to_vec();
    no_xfa[0] = "<< /Fields [7 0 R] /XFA null >>";
    assert!(
        !properties(document("/AcroForm 6 0 R", &no_xfa, ""))
            .forms
            .xfa
    );
}

#[test]
fn fields_listed_with_their_parent_count_once() {
    let counts = |objects: &[&str]| {
        let p = properties(document("/AcroForm 6 0 R", objects, ""));
        let f = p.forms;
        assert!(!f.truncated, "{objects:?}");
        (f.fields, f.signature_fields, f.signatures)
    };
    // The parent first, kids with /Parent: two fields, both inheriting the
    // parent's /FT and /V (a null /V is no value of its own).
    let parent = "<< /T (addr) /FT /Sig /V << /Type /Sig >> /Kids [8 0 R 9 0 R] >>";
    let a = "<< /T (a) /Parent 7 0 R >>";
    let b = "<< /T (b) /Parent 7 0 R /V null >>";
    assert_eq!(
        counts(&["<< /Fields [7 0 R 8 0 R 9 0 R] >>", parent, a, b]),
        (2, 2, 2)
    );
    // Kids listed before their parent wait for it.
    assert_eq!(
        counts(&["<< /Fields [8 0 R 9 0 R 7 0 R] >>", parent, a, b]),
        (2, 2, 2)
    );
    // Kids without /Parent, listed first: still two fields, and the
    // parent is no phantom signed signature field.
    let a = "<< /T (a) >>";
    let b = "<< /T (b) >>";
    assert_eq!(
        counts(&["<< /Fields [8 0 R 9 0 R 7 0 R] >>", parent, a, b]),
        (2, 0, 0)
    );
    assert_eq!(
        counts(&["<< /Fields [7 0 R 8 0 R 9 0 R] >>", parent, a, b]),
        (2, 2, 2)
    );
    // An entry whose parent never reaches it stands on its own.
    let a = "<< /T (a) /Parent 7 0 R >>";
    let b = "<< /T (b) /Parent 7 0 R >>";
    let orphan = "<< /T (o) /Parent 11 0 R /FT /Tx >>";
    let elsewhere = "<< /T (p2) >>";
    assert_eq!(
        counts(&[
            "<< /Fields [7 0 R 10 0 R] >>",
            parent,
            a,
            b,
            orphan,
            elsewhere
        ]),
        (3, 2, 2)
    );
}

#[test]
fn hostile_field_trees_terminate() {
    // Cycles: a field listed twice, fields that are their own kids and
    // each other's. Neither is a terminal field, and a field that is its
    // own ancestor makes the counts lower bounds.
    let cycle = [
        "<< /Fields [7 0 R 7 0 R] >>",
        "<< /T (a) /Kids [8 0 R 7 0 R] >>",
        "<< /T (b) /Kids [7 0 R 8 0 R] /FT /Sig /V 7 0 R >>",
    ];
    let p = properties(document("/AcroForm 6 0 R", &cycle, ""));
    let counts = (p.forms.fields, p.forms.signature_fields, p.forms.signatures);
    assert_eq!(counts, (0, 0, 0));
    assert!(p.forms.truncated);
    // A /Kids array shared with a field above: also a lower bound.
    let shared = [
        "<< /Fields [7 0 R] >>",
        "<< /T (a) /Kids 8 0 R >>",
        "[9 0 R]",
        "<< /T (b) /Kids 8 0 R >>",
    ];
    let p = properties(document("/AcroForm 6 0 R", &shared, ""));
    assert_eq!(p.forms.fields, 0);

    // A chain deeper than the walk follows.
    let mut chain = vec!["<< /Fields [7 0 R] >>".to_owned()];
    for level in 0..100 {
        let next = FIRST + 2 + level;
        chain.push(format!("<< /T (l{level}) /Kids [{next} 0 R] >>"));
    }
    chain.push("<< /T (bottom) /FT /Tx >>".to_owned());
    let chain: Vec<&str> = chain.iter().map(String::as_str).collect();
    let p = properties(document("/AcroForm 6 0 R", &chain, ""));
    assert!(p.forms.truncated);
    assert_eq!(p.forms.fields, 0);

    // Items of every wrong kind, missing objects, and an unreadable item.
    let junk = [
        "<< /Fields [null 12 (x) 99 0 R 8 0 R 7 0 R << /F 1 >> stream\nabc\nendstream 7 0 R] >>",
        "<< /T (ok) >>",
        "42",
    ];
    let p = properties(document("/AcroForm 6 0 R", &junk, ""));
    assert_eq!(p.forms.fields, 1);
    assert!(p.forms.truncated, "read up to the unreadable item");
}

#[test]
fn huge_forms_are_counted_up_to_the_cap() {
    let bytes = build_full(&[page()], "/AcroForm 6 0 R", |b| {
        let form = b.reserve();
        assert_eq!(form, FIRST);
        let fields: Vec<String> = (0..25_000)
            .map(|i| format!("{} 0 R", b.add(format!("<< /FT /Tx /T (f{i}) >>"))))
            .collect();
        b.set(form, format!("<< /Fields [{}] >>", fields.join(" ")));
    });
    let forms = properties(bytes).forms;
    assert!(forms.truncated);
    assert_eq!(forms.fields, 20_000);
}

#[test]
fn tagged_means_marked() {
    let tagged = |extra: &str| properties(document(extra, &[], "")).tagged;
    assert!(tagged("/MarkInfo << /Marked true >> /StructTreeRoot << >>"));
    assert!(tagged("/MarkInfo << /Marked true >>"));
    assert!(!tagged("/MarkInfo << /Marked false >>"));
    assert!(!tagged("/StructTreeRoot << >>"));
    assert!(!tagged("/MarkInfo 12"));
    assert!(!tagged(""));
}

/// A document whose first object is a linearization dictionary with the
/// file's length, and its last `startxref` offset.
fn linearized_document() -> (Vec<u8>, usize) {
    let mut b = PdfBuilder::new();
    b.add("<< /Linearized 1 /L 7777777777 /O 5 >>");
    let catalog = b.reserve();
    let tree = b.reserve();
    let content = b.add_stream("", Vec::new());
    let page = b.add(format!(
        "<< /Type /Page /Parent {tree} 0 R /MediaBox [0 0 200 200] /Contents {content} 0 R >>"
    ));
    b.set(
        tree,
        format!("<< /Type /Pages /Kids [{page} 0 R] /Count 1 >>"),
    );
    b.set(catalog, format!("<< /Type /Catalog /Pages {tree} 0 R >>"));
    let mut bytes = b.finish(catalog);
    let start: usize = String::from_utf8_lossy(&bytes)
        .rsplit("startxref")
        .next()
        .and_then(|t| t.split_whitespace().next())
        .and_then(|n| n.parse().ok())
        .unwrap();
    let at = bytes.windows(10).position(|w| w == b"7777777777").unwrap();
    let length = format!("{:010}", bytes.len());
    bytes[at..at + 10].copy_from_slice(length.as_bytes());
    (bytes, start)
}

#[test]
fn linearization_turns_false_after_an_update() {
    let (bytes, start) = linearized_document();
    assert!(properties(bytes.clone()).linearized);
    // An incremental update: one more object, a section and a trailer.
    let mut updated = bytes.clone();
    let object = updated.len();
    updated.extend_from_slice(b"6 0 obj\n<< /Updated true >>\nendobj\n");
    let section = updated.len();
    updated.extend_from_slice(
        format!(
            "xref\n6 1\n{object:010} 00000 n \ntrailer\n<< /Size 7 /Root 2 0 R /Prev {start} >>\nstartxref\n{section}\n%%EOF\n"
        )
        .as_bytes(),
    );
    let doc = open(updated);
    assert!(!doc.properties().unwrap().linearized);
    assert_eq!(doc.page_count(), 1);
    // Without a linearization dictionary at all.
    assert!(!properties(document("", &[], "")).linearized);
}

#[test]
fn the_trailer_is_found_after_a_long_table() {
    // 150,000 entries: a 3 MB table between the information and startxref.
    let bytes = build_full(&[page()], "", |b| {
        let info = b.add("<< /Title (Far away) /CreationDate (D:20261024153000-00'30') >>");
        for _ in 0..150_000 {
            b.reserve();
        }
        b.trailer(&format!("/Info {info} 0 R"));
    });
    assert!(bytes.len() > 3 << 20);
    let m = properties(bytes).metadata;
    assert_eq!(m.title.as_deref(), Some("Far away"));
    // As written, so read from the trailer this probe found.
    assert_eq!(m.creation_date.as_deref(), Some("D:20261024153000-00'30'"));
}

#[test]
fn repaired_files_keep_their_information() {
    let bytes = document(
        "",
        &["<< /Title (Repaired) /ModDate (D:20261024153000Z) >>"],
        "/Info 6 0 R",
    );
    // startxref names an offset past the end of the file.
    let at = bytes
        .windows(10)
        .rposition(|w| w == b"startxref\n")
        .unwrap()
        + 10;
    let digits = bytes[at..]
        .iter()
        .take_while(|b| b.is_ascii_digit())
        .count();
    let mut broken = bytes[..at].to_vec();
    broken.extend_from_slice(b"987654321");
    broken.extend_from_slice(&bytes[at + digits..]);
    let doc = open(broken);
    let p = doc.properties();
    assert!(p.is_ok(), "{p:?}");
    let m = p.unwrap().metadata;
    assert_eq!(m.title.as_deref(), Some("Repaired"));
    assert_eq!(m.modification_date.as_deref(), Some("D:20261024153000Z"));
    assert_eq!(doc.panic_count(), 0);
}

/// A one-page document whose last section is a cross-reference stream
/// naming `info` (object 4) as /Info; `startxref` names `wrong` instead of
/// the stream when given.
fn xref_stream_document(info: &str, wrong: Option<&str>) -> Vec<u8> {
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] >>".to_owned(),
        info.to_owned(),
    ];
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (i, body) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{body}\nendobj\n", i + 1).as_bytes());
    }
    let stream_at = out.len();
    offsets.push(stream_at);
    // Rows of /W [1 4 2]: the free head, then every object at its offset.
    let mut rows = vec![0u8, 0, 0, 0, 0, 0xFF, 0xFF];
    for offset in &offsets {
        rows.push(1);
        rows.extend_from_slice(&u32::try_from(*offset).unwrap().to_be_bytes());
        rows.extend_from_slice(&[0, 0]);
    }
    out.extend_from_slice(
        format!(
            "5 0 obj\n<< /Type /XRef /Size 6 /W [1 4 2] /Root 1 0 R /Info 4 0 R /Length {} >>\nstream\n",
            rows.len()
        )
        .as_bytes(),
    );
    out.extend_from_slice(&rows);
    out.extend_from_slice(b"\nendstream\nendobj\n");
    let start = wrong.map_or_else(|| stream_at.to_string(), str::to_owned);
    out.extend_from_slice(format!("startxref\n{start}\n%%EOF\n").as_bytes());
    out
}

#[test]
fn cross_reference_streams_are_found_when_startxref_misses() {
    let info = "<< /Title (Streamed) /CreationDate (D:20261024153000-00'30') >>";
    for wrong in [None, Some("999999999"), Some("9")] {
        let p = properties(xref_stream_document(info, wrong));
        assert_eq!(p.metadata.title.as_deref(), Some("Streamed"), "{wrong:?}");
        // As written: read through the stream's /Info, not hayro's dates.
        assert_eq!(
            p.metadata.creation_date.as_deref(),
            Some("D:20261024153000-00'30'"),
            "{wrong:?}"
        );
    }
}

#[test]
fn dates_without_the_raw_information_drop_offsets_hayro_cannot_know() {
    let bytes = document(
        "",
        &[
            "<< /Title (Padded) /CreationDate (D:20261024153000+08'00') /ModDate (D:20261024153000) >>",
        ],
        "/Info 6 0 R",
    );
    // startxref more than 4 KiB before the end: hayro still finds it, the
    // trailer probe does not, so the dates are hayro's.
    let mut padded = bytes;
    for _ in 0..200 {
        padded.extend_from_slice(b"% padding after the end of the file\n");
    }
    let doc = open(padded);
    let m = doc.properties().unwrap().metadata;
    assert_eq!(m.title.as_deref(), Some("Padded"));
    assert_eq!(m.creation_date.as_deref(), Some("D:20261024153000+08'00'"));
    assert_eq!(m.modification_date.as_deref(), Some("D:20261024153000"));
    // metadata() keeps hayro's reading.
    assert_eq!(
        doc.metadata().unwrap().modification_date.as_deref(),
        Some("D:20261024153000+00'00'")
    );
}

// --- generated fixtures ---------------------------------------------------

#[test]
fn rich_fixture() {
    let Some(bytes) = fixture("small-text/properties-rich.pdf") else {
        return;
    };
    let p = properties(bytes);
    let m = &p.metadata;
    assert_eq!(m.title.as_deref(), Some("文件內容測試 Properties fixture"));
    assert_eq!(
        m.author.as_deref(),
        Some("\u{201C}FastPDF\u{201D} fixture generator")
    );
    assert_eq!(
        m.subject.as_deref(),
        Some("PDFDocEncoding \u{2014} text strings \u{2022} dates")
    );
    assert_eq!(m.keywords.as_deref(), Some("屬性, properties, 測試"));
    assert_eq!(m.creation_date.as_deref(), Some("D:20261024153000+08'00'"));
    assert_eq!(
        m.modification_date.as_deref(),
        Some("D:20261025091500-00'30'")
    );
    assert_eq!(m.pdf_version.as_deref(), Some("1.7"));
    assert!(p.tagged && !p.linearized && p.security.is_none());
    assert_eq!(
        p.forms,
        FormSummary {
            fields: 6,
            signature_fields: 4,
            signatures: 2,
            xfa: false,
            truncated: false,
        }
    );
}

#[test]
fn linearized_fixture() {
    let Some(bytes) = fixture("small-text/linearized-flag.pdf") else {
        return;
    };
    let p = properties(bytes);
    assert!(p.linearized);
    assert_eq!(p.metadata.creation_date.as_deref(), Some("20200101"));
    assert_eq!(
        p.metadata.modification_date.as_deref(),
        Some("D:20261024153000Z")
    );
}

#[test]
fn encrypted_fixtures() {
    use EncryptionMethod::*;
    // Path, method, /V, /R, printing, copying.
    let cases = [
        (
            "aes128-owner-only",
            Aes128,
            4,
            4,
            PrintPermission::HighResolution,
            false,
        ),
        (
            "aes256-r6-owner-only",
            Aes256,
            5,
            6,
            PrintPermission::HighResolution,
            false,
        ),
        (
            "aes256-r5-owner-only",
            Aes256,
            5,
            5,
            PrintPermission::HighResolution,
            false,
        ),
        (
            "rc4-128-owner-only",
            Rc4 { key_bits: 128 },
            2,
            3,
            PrintPermission::HighResolution,
            false,
        ),
        (
            "rc4-40-owner-only",
            Rc4 { key_bits: 40 },
            1,
            2,
            PrintPermission::HighResolution,
            true,
        ),
        (
            "rc4-128-owner-perms",
            Rc4 { key_bits: 128 },
            2,
            3,
            PrintPermission::LowResolution,
            false,
        ),
    ];
    for (name, method, v, r, printing, copy) in cases {
        let Some(bytes) = fixture(&format!("encrypted/{name}.pdf")) else {
            continue;
        };
        let p = properties(bytes);
        assert!(p.metadata.encrypted, "{name}");
        assert_eq!(
            p.metadata.title.as_deref(),
            Some("加密測試文件 Encrypted fixture"),
            "{name}: strings are decrypted"
        );
        let s = p.security.unwrap_or_else(|| panic!("{name}: no security"));
        assert_eq!(s.handler.as_deref(), Some("Standard"), "{name}");
        assert_eq!(s.method, method, "{name}");
        assert_eq!((s.version, s.revision), (Some(v), Some(r)), "{name}");
        let perms = s.permissions.unwrap_or_else(|| panic!("{name}: no /P"));
        assert_eq!(perms.printing(), printing, "{name}");
        assert_eq!(perms.copy(), copy, "{name}");
        assert!(!s.user_password, "{name}");
    }
    if let Some(bytes) = fixture("encrypted/rc4-128-owner-perms.pdf") {
        let perms = properties(bytes)
            .security
            .and_then(|s| s.permissions)
            .unwrap();
        assert!(perms.fill_forms() && perms.copy_for_accessibility() && perms.assemble());
        assert!(!perms.modify() && !perms.annotate() && !perms.all_allowed());
    }
    if let Some(bytes) = fixture("encrypted/rc4-40-owner-only.pdf") {
        let perms = properties(bytes)
            .security
            .and_then(|s| s.permissions)
            .unwrap();
        assert!(perms.all_allowed());
    }
    if let Some(bytes) = fixture("encrypted/aes256-r6-user-password.pdf") {
        let doc = open_with(bytes, Some("fastpdf-user"), ResourceLimits::default()).unwrap();
        let s = doc.properties().unwrap().security.unwrap();
        assert!(s.user_password);
        assert_eq!(s.method, Aes256);
    }
}

#[test]
fn hostile_fixtures() {
    if let Some(bytes) = fixture("malformed/info-hostile-strings.pdf") {
        let m = properties(bytes).metadata;
        // Decoded from at most 16 KiB of the 1 MiB string.
        let title = m.title.unwrap();
        assert!(title.starts_with("Huge title \u{2022}-"));
        assert!(title.len() <= 3 * 16 * 1024, "{}", title.len());
        assert!(
            m.author.unwrap().contains('\u{202E}'),
            "cleaning is the UI's job"
        );
        assert_eq!(m.subject.as_deref(), Some("中"));
        assert_eq!(m.keywords.as_deref(), Some("\u{FFFD}A"));
        assert_eq!(m.producer.as_deref(), Some("產生器"));
        assert_eq!(m.creation_date.as_deref(), Some("D:2026-13-45"));
        assert!(m.modification_date.unwrap().len() > 10_000);
    }
    for (name, title) in [
        ("info-stream", Some("Info as a stream")),
        ("info-wrong-type", None),
        ("info-missing", None),
    ] {
        if let Some(bytes) = fixture(&format!("malformed/{name}.pdf")) {
            assert_eq!(properties(bytes).metadata.title.as_deref(), title, "{name}");
        }
    }
    if let Some(bytes) = fixture("malformed/acroform-kids-cycle.pdf") {
        let forms = properties(bytes).forms;
        assert!(forms.truncated);
        assert!(forms.fields <= 10, "{forms:?}");
    }
    if let Some(bytes) = fixture("malformed/acroform-many-fields.pdf") {
        let forms = properties(bytes).forms;
        assert!(forms.truncated);
        assert_eq!(forms.fields, 20_000);
    }
}
