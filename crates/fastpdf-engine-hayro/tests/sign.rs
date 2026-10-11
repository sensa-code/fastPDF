//! Signing (ADR 0013 §5, `docs/digital-signatures.md`): the incremental
//! update a signature adds, checked through the bytes, the reopened
//! document and its rendering; and real CMS signatures made with a key that
//! is never stored.

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use common::{PageSpec, build, full_request, open, pixel, render};
use fastpdf_engine_api::write::{
    OutputContent, OutputSpec, PagePick, SignProblem, SignatureAppearance, SignatureSpec, Stamp,
    WriteError, WriteJob,
};
use fastpdf_engine_api::{EngineDocument, PageIndex, PageRect, PixelFormat, Rotation};
use fastpdf_engine_hayro::write::execute_with;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("fastpdf-sign-test-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn page(content: &str) -> PageSpec {
    PageSpec::new([0.0, 0.0, 612.0, 792.0], content)
}

fn spec(appearance: Option<SignatureAppearance>) -> SignatureSpec {
    SignatureSpec {
        signer: "test".into(),
        window: 0,
        reason: "同意".into(),
        location: "Taipei".into(),
        appearance,
    }
}

fn signed_job(source: PathBuf, out: PathBuf, base: OutputContent, spec: SignatureSpec) -> WriteJob {
    WriteJob {
        sources: vec![source],
        outputs: vec![OutputSpec {
            path: out,
            content: OutputContent::Signed {
                base: Box::new(base),
                signature: spec,
            },
        }],
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

fn count(haystack: &[u8], needle: &[u8]) -> usize {
    haystack
        .windows(needle.len())
        .filter(|w| *w == needle)
        .count()
}

/// The numbers of the last `/ByteRange` in `bytes`.
fn byte_range(bytes: &[u8]) -> [usize; 4] {
    let at = bytes
        .windows(12)
        .rposition(|w| w == b"/ByteRange [")
        .unwrap();
    let close = at + bytes[at..].iter().position(|&b| b == b']').unwrap();
    let numbers: Vec<usize> = std::str::from_utf8(&bytes[at + 12..close])
        .unwrap()
        .split_ascii_whitespace()
        .map(|n| n.parse().unwrap())
        .collect();
    numbers.try_into().unwrap()
}

/// Signs `source` (a byte copy as the base) with `signer`; the written
/// bytes, or the error (and then no file).
fn sign_copy(
    dir: &Path,
    source: &[u8],
    spec: SignatureSpec,
    signer: fastpdf_engine_api::write::SignFn<'_>,
) -> Result<Vec<u8>, WriteError> {
    let input = dir.join("in.pdf");
    std::fs::write(&input, source).unwrap();
    let out = dir.join("signed.pdf");
    let job = signed_job(input, out.clone(), OutputContent::Copy { source: 0 }, spec);
    match execute_with(&job, signer) {
        Ok(report) => {
            assert_eq!(report.outputs.len(), 1);
            Ok(std::fs::read(&out).unwrap())
        }
        Err(e) => {
            assert!(!out.exists(), "a failed signature writes nothing");
            Err(e)
        }
    }
}

#[test]
fn invisible_signatures_append_an_update() {
    let dir = scratch("invisible");
    let source = build(&[page("BT /F1 24 Tf 72 700 Td (Signed page) Tj ET")]);
    let seen: RefCell<Vec<Vec<u8>>> = RefCell::new(Vec::new());
    let signer = |s: &SignatureSpec, parts: &[&[u8]]| {
        assert_eq!(s.signer, "test");
        *seen.borrow_mut() = parts.iter().map(|p| p.to_vec()).collect();
        Ok(vec![0x30, 0x82, 0x01, 0x00, 0xAB])
    };
    let out = sign_copy(&dir, &source, spec(None), &signer).unwrap();

    // The document's bytes are untouched; the update follows them.
    assert!(out.starts_with(&source));
    let update = &out[source.len()..];
    assert!(contains(update, b"/SubFilter /ETSI.CAdES.detached"));
    assert!(contains(update, b"/FT /Sig"));
    assert!(contains(update, b"/Rect [0 0 0 0]"));
    assert!(contains(update, b"/SigFlags 3"));
    assert!(contains(update, b"/Reason <FEFF540C610F>"));
    assert!(contains(update, b"/Location (Taipei)"));
    assert!(contains(update, b"/M (D:"));
    assert!(!contains(update, b"/Type /XRef"), "a table after a table");
    let previous = String::from_utf8_lossy(&source);
    let start = previous.rsplit("startxref").next().unwrap().trim();
    let start = start.split_whitespace().next().unwrap();
    assert!(contains(update, format!("/Prev {start}").as_bytes()));
    assert!(update.ends_with(b"%%EOF\n"));

    // The byte range covers everything but the signature, which is in.
    let [zero, gap_start, gap_end, rest] = byte_range(&out);
    assert_eq!(zero, 0);
    assert_eq!(gap_end + rest, out.len());
    let parts = seen.borrow();
    assert_eq!(parts.len(), 2);
    assert_eq!(parts[0], out[..gap_start]);
    assert_eq!(parts[1], out[gap_end..]);
    assert!(out[gap_start..gap_end].starts_with(b"<30820100AB0000"));

    // It opens as the same document, and the page looks the same.
    let before = open(source.clone());
    let after = open(out);
    assert_eq!(after.page_count(), 1);
    let req = full_request(&before, 0, 0.5, Rotation::R0);
    let a = render(&before, &req, PixelFormat::default()).unwrap();
    let b = render(&after, &req, PixelFormat::default()).unwrap();
    assert_eq!(a.data(), b.data());
}

#[test]
fn a_second_signature_keeps_the_first() {
    let dir = scratch("twice");
    let source = build(&[page("BT /F1 24 Tf 72 700 Td (Twice) Tj ET")]);
    let signer = |_: &SignatureSpec, _: &[&[u8]]| Ok(vec![0x30, 0x03, 0x02, 0x01, 0x01]);
    let first = sign_copy(&dir, &source, spec(None), &signer).unwrap();
    let dir = scratch("twice-2");
    let second = sign_copy(&dir, &first, spec(None), &signer).unwrap();
    assert!(
        second.starts_with(&first),
        "the first signature's bytes stay"
    );
    assert_eq!(count(&second, b"/Type /Sig"), 2);
    assert!(contains(&second, b"(Signature1)") && contains(&second, b"(Signature2)"));
    // The second form lists both fields.
    let last_form = second.windows(8).rposition(|w| w == b"/Fields ").unwrap();
    let fields = &second[last_form..last_form + 40];
    assert_eq!(
        count(fields, b" 0 R"),
        2,
        "{}",
        String::from_utf8_lossy(fields)
    );
    assert_eq!(open(second).page_count(), 1);
}

#[test]
fn visible_signatures_show_the_seal() {
    let dir = scratch("visible");
    let seal = dir.join("seal.png");
    image::RgbaImage::from_pixel(40, 40, image::Rgba([220, 0, 0, 255]))
        .save(&seal)
        .unwrap();
    let source = build(&[page("BT /F1 24 Tf 72 700 Td (Seal here) Tj ET")]);
    let appearance = SignatureAppearance {
        page: PageIndex::FIRST,
        stamp: Stamp {
            image: seal,
            rect: PageRect::new(400.0, 100.0, 480.0, 180.0),
            rotate: Rotation::R0,
        },
    };
    let signer = |_: &SignatureSpec, _: &[&[u8]]| Ok(vec![0x30, 0x00]);
    let out = sign_copy(&dir, &source, spec(Some(appearance)), &signer).unwrap();
    assert!(contains(&out, b"/Rect [400 612 480 692]"));
    assert!(contains(&out, b"/AP <</N "));
    // The seal is drawn there (annotations are rendered), not elsewhere.
    let doc = open(out);
    let req = full_request(&doc, 0, 0.5, Rotation::R0);
    let pm = render(&doc, &req, PixelFormat::default()).unwrap();
    let p = pixel(&pm, 220, 70);
    assert!(p[0] > 200 && p[1] < 40 && p[2] < 40, "{p:?}");
    let q = pixel(&pm, 100, 70);
    assert!(q[0] > 200 && q[1] > 200, "{q:?}");
}

#[test]
fn signer_failures_and_oversized_signatures_write_nothing() {
    let dir = scratch("failures");
    let source = build(&[page("BT /F1 24 Tf 72 700 Td (No) Tj ET")]);
    let cancelled = |_: &SignatureSpec, _: &[&[u8]]| Err(SignProblem::Cancelled);
    assert_eq!(
        sign_copy(&dir, &source, spec(None), &cancelled),
        Err(WriteError::Sign(SignProblem::Cancelled))
    );
    let huge = |_: &SignatureSpec, _: &[&[u8]]| Ok(vec![0; 40 * 1024]);
    assert_eq!(
        sign_copy(&dir, &source, spec(None), &huge),
        Err(WriteError::Sign(SignProblem::TooLarge))
    );
    // No signer at all.
    let input = dir.join("in.pdf");
    let job = signed_job(
        input,
        dir.join("signed.pdf"),
        OutputContent::Copy { source: 0 },
        spec(None),
    );
    assert_eq!(
        fastpdf_engine_hayro::write::execute(&job).map(|_| ()),
        Err(WriteError::Sign(SignProblem::Unavailable))
    );
    // A signature page the document does not have.
    let mut far = spec(None);
    far.appearance = Some(SignatureAppearance {
        page: PageIndex::new(5),
        stamp: Stamp {
            image: dir.join("none.png"),
            rect: PageRect::new(0.0, 0.0, 10.0, 10.0),
            rotate: Rotation::R0,
        },
    });
    let ok = |_: &SignatureSpec, _: &[&[u8]]| Ok(vec![0x30, 0x00]);
    assert!(matches!(
        sign_copy(&dir, &source, far, &ok),
        Err(WriteError::NoSuchPage { .. })
    ));
}

#[test]
fn changed_documents_are_signed_after_their_changes() {
    let dir = scratch("pages-then-sign");
    let source = build(&[
        page("BT /F1 24 Tf 72 700 Td (One) Tj ET"),
        page("BT /F1 24 Tf 72 700 Td (Two) Tj ET"),
    ]);
    let input = dir.join("in.pdf");
    std::fs::write(&input, &source).unwrap();
    let out = dir.join("signed.pdf");
    let base = OutputContent::Pages(vec![PagePick::new(0, PageIndex::new(1))]);
    let ok = |_: &SignatureSpec, _: &[&[u8]]| Ok(vec![0x30, 0x00]);
    let report = execute_with(&signed_job(input, out.clone(), base, spec(None)), &ok).unwrap();
    assert_eq!(report.outputs[0].pages, 1);
    let bytes = std::fs::read(&out).unwrap();
    assert!(contains(&bytes, b"(Two)") && !contains(&bytes, b"(One)"));
    assert_eq!(open(bytes).page_count(), 1);
}

#[test]
fn rewriting_a_signed_document_removes_its_signatures() {
    let dir = scratch("rewrite-signed");
    let source = build(&[page("BT /F1 24 Tf 72 700 Td (Signed) Tj ET")]);
    let ok = |_: &SignatureSpec, _: &[&[u8]]| Ok(vec![0x30, 0x00]);
    let signed = sign_copy(&dir, &source, spec(None), &ok).unwrap();
    let input = dir.join("signed-in.pdf");
    std::fs::write(&input, &signed).unwrap();
    let out = dir.join("rewritten.pdf");
    let job = WriteJob {
        sources: vec![input],
        outputs: vec![OutputSpec {
            path: out.clone(),
            content: OutputContent::Pages(vec![PagePick::new(0, PageIndex::FIRST)]),
        }],
    };
    let report = fastpdf_engine_hayro::write::execute(&job).unwrap();
    assert_eq!(report.outputs[0].signatures_removed, 1);
    let bytes = std::fs::read(&out).unwrap();
    assert!(contains(&bytes, b"/FT /Sig"), "the field stays");
    assert!(!contains(&bytes, b"/Type /Sig"), "its value goes");
}

/// Documents from the generated corpus: a cross-reference stream gets a
/// cross-reference stream; a broken table is rewritten first.
#[test]
fn generated_fixtures_are_signed() {
    let ok = |_: &SignatureSpec, _: &[&[u8]]| Ok(vec![0x30, 0x00]);
    if let Some(bytes) = common::fixture("large-page-count/objstm-xrefstream-2000p.pdf") {
        let dir = scratch("xref-stream");
        let out = sign_copy(&dir, &bytes, spec(None), &ok).unwrap();
        assert!(out.starts_with(&bytes));
        assert!(contains(&out[bytes.len()..], b"/Type /XRef"));
        assert_eq!(open(out).page_count(), 2000);
    }
    if let Some(bytes) = common::fixture("malformed/bad-xref-offsets.pdf") {
        let dir = scratch("bad-xref");
        let pages = open(bytes.clone()).page_count();
        let out = sign_copy(&dir, &bytes, spec(None), &ok).unwrap();
        assert!(!out.starts_with(&bytes), "rewritten before signing");
        assert_eq!(open(out).page_count(), pages);
    }
}

/// A real CMS signature by a key that exists for this one test and is never
/// stored (`fastpdf_sign::Signer::Ephemeral`).
#[cfg(windows)]
#[test]
fn ephemeral_keys_make_real_signatures() {
    let dir = scratch("ephemeral");
    let source = build(&[page("BT /F1 24 Tf 72 700 Td (Really signed) Tj ET")]);
    let signer = |_: &SignatureSpec, parts: &[&[u8]]| {
        fastpdf_sign::sign(&fastpdf_sign::Signer::Ephemeral, parts)
            .map_err(|e| SignProblem::Failed(e.to_string()))
    };
    let out = sign_copy(&dir, &source, spec(None), &signer).unwrap();
    let [_, gap_start, gap_end, _] = byte_range(&out);
    let hex = &out[gap_start + 1..gap_end - 1];
    assert!(hex.starts_with(b"30"), "a DER SEQUENCE");
    // Some of the room is used, the rest is zeros.
    let used = hex.iter().rposition(|&b| b != b'0').unwrap();
    assert!(used > 1000 && used < hex.len() - 1000);
    if let Ok(keep) = std::env::var("FASTPDF_KEEP_SIGNED") {
        std::fs::write(keep, &out).unwrap();
    }
}
