//! Opening, encryption, malformed input, metadata, text, outline and links.

mod common;

use std::panic::catch_unwind;

use common::*;
use fastpdf_engine_api::{
    CancelToken, Destination, DestinationView, DocumentSource, EngineDocument, EngineError,
    LinkTarget, OpenOptions, PageIndex, PageRect, PdfEngine, PixelFormat, Pixmap, RenderRequest,
    Rotation, SharedBytes,
};
use fastpdf_engine_zpdf::ZpdfEngine;

fn simple_pdf() -> Vec<u8> {
    pages_pdf(&[
        (
            "/MediaBox [0 0 300 400]",
            b"0 0 1 rg 20 20 100 100 re f BT /F1 24 Tf 40 300 Td (Hello FastPDF) Tj ET".as_slice(),
        ),
        (
            "/MediaBox [0 0 500 200] /Rotate 270",
            b"1 0 0 rg 0 0 50 50 re f".as_slice(),
        ),
    ])
}

#[test]
fn opens_and_reports_pages_and_metadata() {
    let doc = open(simple_pdf()).expect("open");
    assert_eq!(doc.page_count(), 2);
    let p1 = doc.page_info(PageIndex::new(1)).expect("info");
    assert_eq!(p1.rotation, Rotation::R270);
    assert_eq!((p1.size.width, p1.size.height), (500.0, 200.0));
    let meta = doc.metadata().expect("metadata");
    assert_eq!(meta.pdf_version.as_deref(), Some("1.7"));
    assert!(!meta.encrypted);
    let info = ZpdfEngine::new().info();
    assert_eq!(info.version, "0.14.0+fe0ed23");
    assert!(info.capabilities.parallel_render && info.capabilities.cooperative_cancel);
}

#[test]
fn malformed_input_is_an_error_not_a_panic() {
    for bytes in [
        Vec::new(),
        b"not a pdf at all".to_vec(),
        b"%PDF-1.7\n%%EOF".to_vec(),
        b"%PDF-1.7\n1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj\ntrailer << /Root 1 0 R >>"
            .to_vec(),
    ] {
        let result = catch_unwind(|| open(bytes.clone()));
        assert!(
            result.is_ok(),
            "panicked on {:?}",
            String::from_utf8_lossy(&bytes)
        );
        let opened = result.unwrap_or_else(|_| Err(EngineError::Panicked(String::new())));
        assert!(
            matches!(opened, Err(EngineError::Malformed(_))),
            "{:?} -> {opened:?}",
            String::from_utf8_lossy(&bytes)
        );
    }
}

/// Deterministic byte mutations of a valid file: open, inspect and render
/// must never panic (the guard would report `Panicked`).
#[test]
fn mutated_files_never_panic() {
    let base = pages_pdf(&[
        (
            "/MediaBox [0 0 200 200]",
            b"q 0.5 0 0 0.5 10 10 cm 0 0 1 rg 0 0 200 200 re f Q 1 w 0 0 m 200 200 l S \
              BT /F1 12 Tf 10 100 Td (abc) Tj ET"
                .as_slice(),
        ),
        (
            "/MediaBox [0 0 100 100] /Rotate 90",
            b"1 0 0 rg 0 0 50 50 re f".as_slice(),
        ),
    ]);
    let mut seed = 0x2545_f491u32;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed
    };
    let mut opened = 0;
    for _ in 0..400 {
        let mut bytes = base.clone();
        let flips = 1 + next() % 8;
        for _ in 0..flips {
            let at = next() as usize % bytes.len();
            bytes[at] = (next() & 0xff) as u8;
        }
        if next() % 5 == 0 {
            let cut = next() as usize % bytes.len();
            bytes.truncate(cut);
        }
        let Ok(doc) = open(bytes) else { continue };
        opened += 1;
        for page in 0..doc.page_count().min(3) {
            let index = PageIndex::new(page);
            let Ok(info) = doc.page_info(index) else {
                continue;
            };
            let request = RenderRequest::full_page(
                index,
                info.size,
                info.rotation,
                Rotation::R0,
                scale(0.25),
            );
            if let Ok(mut pm) = Pixmap::new(
                request.region.size(),
                PixelFormat::default(),
                &Default::default(),
            ) {
                let result = doc.render(&request, &mut pm.as_mut(), &CancelToken::new());
                assert!(
                    !matches!(result, Err(EngineError::Panicked(_))),
                    "{result:?}"
                );
            }
            let text = doc.text_layer(index, &CancelToken::new());
            assert!(!matches!(text, Err(EngineError::Panicked(_))), "{text:?}");
            let links = doc.links(index);
            assert!(!matches!(links, Err(EngineError::Panicked(_))), "{links:?}");
        }
        assert!(!matches!(doc.outline(), Err(EngineError::Panicked(_))));
        assert!(!matches!(doc.metadata(), Err(EngineError::Panicked(_))));
        assert_eq!(doc.panic_count(), 0);
    }
    assert!(opened > 50, "only {opened} mutated files opened");
}

#[allow(clippy::expect_used)] // test fixture builder
fn encrypted(plain: &[u8], config: zpdf_writer::EncryptionConfig) -> Vec<u8> {
    let file = zpdf_parser::PdfFile::parse(plain.to_vec()).expect("parse");
    zpdf_writer::rewrite_pdf(
        &file,
        &zpdf_writer::RewriteOptions {
            encrypt: Some(config),
            ..Default::default()
        },
    )
    .expect("encrypt")
}

#[test]
fn passwords_are_required_checked_and_used() {
    let plain = simple_pdf();
    let reference = render_full(&open(plain.clone()).expect("open"), 0, 1.0, Rotation::R0);
    for (name, config) in [
        (
            "rc4",
            zpdf_writer::EncryptionConfig::rc4_128("user", "owner"),
        ),
        (
            "aes256",
            zpdf_writer::EncryptionConfig::aes256("user", "owner"),
        ),
    ] {
        let bytes = encrypted(&plain, config);
        assert_eq!(
            open(bytes.clone()).err(),
            Some(EngineError::PasswordRequired),
            "{name}: no password"
        );
        assert_eq!(
            open_with(bytes.clone(), Some("")).err(),
            Some(EngineError::PasswordRequired),
            "{name}: empty password"
        );
        assert_eq!(
            open_with(bytes.clone(), Some("wrong")).err(),
            Some(EngineError::InvalidPassword),
            "{name}: wrong password"
        );
        for password in ["user", "owner"] {
            let doc = open_with(bytes.clone(), Some(password)).expect("correct password");
            assert!(doc.metadata().expect("meta").encrypted);
            let pm = render_full(&doc, 0, 1.0, Rotation::R0);
            assert_eq!(pm, reference, "{name}/{password}: decrypted render differs");
        }
    }
}

#[test]
fn owner_only_encryption_opens_without_a_password() {
    let plain = simple_pdf();
    for config in [
        zpdf_writer::EncryptionConfig::rc4_128("", "owner"),
        zpdf_writer::EncryptionConfig::aes256("", "owner"),
    ] {
        let doc = open(encrypted(&plain, config)).expect("empty user password");
        assert_eq!(doc.page_count(), 2);
        let pm = render_full(&doc, 0, 0.5, Rotation::R0);
        assert!(is_color(pixel(&pm, 20, 180), [0, 0, 255]));
    }
}

#[test]
fn text_layer_is_in_page_space() {
    // Visible box with a non-zero origin: page space starts at its top-left.
    let doc = open(pages_pdf(&[(
        "/MediaBox [0 0 400 400] /CropBox [50 60 350 360]",
        b"BT /F1 20 Tf 100 300 Td (Hello FastPDF) Tj ET".as_slice(),
    )]))
    .expect("open");
    let layer = doc
        .text_layer(PageIndex::FIRST, &CancelToken::new())
        .expect("text");
    assert!(layer.plain_text().contains("Hello FastPDF"), "{layer:?}");
    let span = layer
        .spans
        .iter()
        .find(|s| s.text.contains("Hello"))
        .expect("span");
    // Baseline origin (100, 300) in user space -> (50, 60) from the top-left.
    assert!((span.bounds.x0 - 50.0).abs() < 0.5, "{:?}", span.bounds);
    assert!(
        span.bounds.y1 > 60.0 && span.bounds.y0 < 60.0,
        "{:?}",
        span.bounds
    );
    assert!(span.bounds.x1 > span.bounds.x0 + 50.0, "{:?}", span.bounds);
    assert!(span.char_bounds.is_empty());
}

fn navigation_pdf() -> Vec<u8> {
    pdf(&[
        obj("<< /Type /Catalog /Pages 2 0 R /Outlines 7 0 R >>"),
        obj("<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>"),
        obj(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 5 0 R \
             /Annots [10 0 R 11 0 R 12 0 R] >>",
        ),
        obj(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /CropBox [12 12 600 780] \
             /Contents 6 0 R >>",
        ),
        stream("", b"0 g 0 0 10 10 re f"),
        stream("", b"0 g 0 0 10 10 re f"),
        obj("<< /Type /Outlines /First 8 0 R /Last 9 0 R /Count 2 >>"),
        obj("<< /Title (Chapter 1) /Parent 7 0 R /Next 9 0 R \
             /Dest [3 0 R /XYZ 72 720 0] >>"),
        obj("<< /Title (Chapter 2) /Parent 7 0 R /Prev 8 0 R \
             /A << /S /GoTo /D [4 0 R /FitR 112 112 312 412] >> >>"),
        obj(
            "<< /Type /Annot /Subtype /Link /Rect [72 600 200 620] /Border [0 0 0] \
             /Dest [4 0 R /FitH 700] >>",
        ),
        obj("<< /Type /Annot /Subtype /Link /Rect [72 500 200 520] \
             /A << /S /URI /URI (https://example.invalid/) >> >>"),
        obj("<< /Type /Annot /Subtype /Link /Rect [72 400 200 420] \
             /A << /S /JavaScript /JS (app.alert(1)) >> >>"),
    ])
}

#[test]
fn outline_destinations_are_in_page_space() {
    let doc = open(navigation_pdf()).expect("open");
    let outline = doc.outline().expect("outline");
    assert_eq!(outline.len(), 2);
    assert_eq!(outline[0].title, "Chapter 1");
    assert_eq!(
        outline[0].destination,
        Some(Destination {
            page: PageIndex::new(0),
            view: DestinationView::Xyz {
                left: Some(72.0),
                top: Some(72.0),
                zoom: None,
            },
        })
    );
    // Page 2's CropBox starts at (12, 12) and ends at y = 780.
    assert_eq!(
        outline[1].destination,
        Some(Destination {
            page: PageIndex::new(1),
            view: DestinationView::FitRect(PageRect::new(100.0, 368.0, 300.0, 668.0)),
        })
    );
}

#[test]
fn links_are_in_page_space() {
    let doc = open(navigation_pdf()).expect("open");
    let links = doc.links(PageIndex::FIRST).expect("links");
    assert_eq!(links.len(), 3);
    assert_eq!(links[0].bounds, PageRect::new(72.0, 172.0, 200.0, 192.0));
    assert_eq!(
        links[0].target,
        LinkTarget::Internal(Destination {
            page: PageIndex::new(1),
            view: DestinationView::FitWidth { top: Some(80.0) },
        })
    );
    assert_eq!(
        links[1].target,
        LinkTarget::Uri("https://example.invalid/".into())
    );
    assert_eq!(links[2].target, LinkTarget::Unsupported);
    assert!(doc.links(PageIndex::new(1)).expect("links").is_empty());
}

#[test]
fn source_bytes_are_copied_once_and_can_be_dropped() {
    let bytes = SharedBytes::from_vec(simple_pdf());
    let doc = ZpdfEngine::new()
        .open(
            DocumentSource::from_bytes(bytes.clone()),
            &OpenOptions::default(),
        )
        .expect("open");
    drop(bytes);
    let request = full_request(doc.as_ref(), 0, 0.5, Rotation::R0);
    assert!(render_request(doc.as_ref(), &request, PixelFormat::default()).is_ok());
}
