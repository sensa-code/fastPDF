//! End-to-end tests of the hayro adapter through the guarded engine API.

mod common;

use std::time::{Duration, Instant};

use common::*;
use fastpdf_engine_api::{
    CancelToken, DestinationView, EngineDocument, EngineError, LimitKind, LinkTarget,
    MemoryPressure, PageIndex, PageSize, PixelFormat, PixelRect, ResourceLimits, Rgba8, Rotation,
};

const RED_SQUARE: &str = "1 0 0 rg 100 550 50 50 re f";

fn approx(a: PageSize, w: f32, h: f32) -> bool {
    (a.width - w).abs() < 0.01 && (a.height - h).abs() < 0.01
}

#[test]
fn page_count_size_and_rotation() {
    let mut p2 = PageSpec::new([0.0, 0.0, 595.0, 842.0], "");
    p2.rotate = Some(90);
    let mut p3 = PageSpec::new([0.0, 0.0, 600.0, 800.0], "");
    p3.crop_box = Some([100.0, 200.0, 400.0, 600.0]);
    p3.rotate = Some(-90);
    let mut p4 = PageSpec::new([0.0, 0.0, 100.0, 50.0], "");
    p4.user_unit = Some(2.0);
    let mut p5 = PageSpec::new([0.0, 0.0, 200.0, 100.0], "");
    p5.rotate = Some(45); // invalid; treated as 0 like other readers
    let doc = open(build(&[
        PageSpec::new([0.0, 0.0, 612.0, 792.0], ""),
        p2,
        p3,
        p4,
        p5,
    ]));
    assert_eq!(doc.page_count(), 5);
    let info = |i| doc.page_info(PageIndex::new(i)).unwrap();
    assert!(approx(info(0).size, 612.0, 792.0));
    assert_eq!(info(0).rotation, Rotation::R0);
    assert!(approx(info(1).size, 595.0, 842.0));
    assert_eq!(info(1).rotation, Rotation::R90);
    // CropBox wins over MediaBox; size is reported unrotated.
    assert!(approx(info(2).size, 300.0, 400.0));
    assert_eq!(info(2).rotation, Rotation::R270);
    // /UserUnit scales the size into points.
    assert!(approx(info(3).size, 200.0, 100.0));
    assert_eq!(info(4).rotation, Rotation::R0);
    assert!(matches!(
        doc.page_info(PageIndex::new(5)),
        Err(EngineError::PageOutOfRange { .. })
    ));
}

#[test]
fn rotated_page_with_offset_crop_box() {
    // The red square sits in the top-left corner of the crop box.
    let mut page = PageSpec::new([0.0, 0.0, 600.0, 800.0], RED_SQUARE);
    page.crop_box = Some([100.0, 200.0, 400.0, 600.0]);
    page.rotate = Some(90);
    let doc = open(build(&[page]));
    let is_red = |p: [u8; 4]| p[0] > 200 && p[1] < 60 && p[2] < 60;

    // /Rotate 90: 400 x 300 px; top-left corner moved to the top-right.
    let req = full_request(&doc, 0, 1.0, Rotation::R0);
    assert_eq!(req.region, PixelRect::new(0, 0, 400, 300));
    let pm = render(&doc, &req, PixelFormat::default()).unwrap();
    assert!(is_red(pixel(&pm, 375, 25)), "{:?}", pixel(&pm, 375, 25));
    assert!(!is_red(pixel(&pm, 25, 25)));
    assert_eq!(pixel(&pm, 25, 25), [255, 255, 255, 255]);

    // Plus a user rotation of 90: 180 in total; the corner is bottom-right.
    let req = full_request(&doc, 0, 1.0, Rotation::R90);
    assert_eq!(req.region, PixelRect::new(0, 0, 300, 400));
    let pm = render(&doc, &req, PixelFormat::default()).unwrap();
    assert!(is_red(pixel(&pm, 275, 375)), "{:?}", pixel(&pm, 275, 375));
    assert!(!is_red(pixel(&pm, 25, 25)));

    // Page space is unrotated with the origin at the crop box corner.
    let req = full_request(&doc, 0, 2.0, Rotation::R270);
    let pm = render(&doc, &req, PixelFormat::default()).unwrap();
    // 90 + 270 = 0 total: the square is back at the top-left.
    assert!(is_red(pixel(&pm, 50, 50)));
}

fn busy_page() -> PageSpec {
    let mut content = String::from("0.2 0.4 0.8 RG 1.5 w\n");
    for i in 0..40 {
        let y = 760.0 - f64::from(i) * 18.0;
        content.push_str(&format!(
            "BT /F1 11 Tf 40 {y} Td (Line {i}: The quick brown fox jumps over the lazy dog) Tj ET\n"
        ));
        content.push_str(&format!(
            "{} {} m {} {} c S\n",
            300.0 + f64::from(i),
            y,
            450.0,
            y + 40.0
        ));
    }
    content.push_str("0.9 0.3 0.1 rg 420 100 120 300 re f\n");
    PageSpec::new([0.0, 0.0, 612.0, 792.0], &content)
}

#[test]
fn tiles_match_the_full_page_render() {
    let doc = open(build(&[busy_page()]));
    // 4x: 2448 x 3168 px, larger than one 2048-px block, so tiles come from
    // several blocks rendered with different offsets.
    let req = full_request(&doc, 0, 4.0, Rotation::R0);
    let full = render(&doc, &req, PixelFormat::default()).unwrap();
    let size = req.region.size();
    let tile = 512;
    let mut stitched = vec![0u8; full.data().len()];
    let mut y = 0;
    while y < size.height {
        let mut x = 0;
        while x < size.width {
            let region = PixelRect::new(x, y, tile.min(size.width - x), tile.min(size.height - y));
            let pm = render_region(&doc, &req, region).unwrap();
            for row in 0..region.height as usize {
                let dst = ((y as usize + row) * size.width as usize + x as usize) * 4;
                let src = row * region.width as usize * 4;
                let len = region.width as usize * 4;
                stitched[dst..dst + len].copy_from_slice(&pm.data()[src..src + len]);
            }
            x += tile;
        }
        y += tile;
    }
    let total = (size.width * size.height) as usize;
    let off = differing_pixels(full.data(), &stitched, 24);
    assert!(off * 2000 <= total, "{off} of {total} pixels differ");

    // A region straddling block boundaries is rendered directly.
    let region = PixelRect::new(1900, 1950, 300, 260);
    let direct = render_region(&doc, &req, region).unwrap();
    let mut expected = Vec::new();
    for row in 0..region.height as usize {
        let start = ((region.y as usize + row) * size.width as usize + region.x as usize) * 4;
        expected.extend_from_slice(&full.data()[start..start + region.width as usize * 4]);
    }
    let off = differing_pixels(direct.data(), &expected, 24);
    assert!(
        off * 2000 <= (region.width * region.height) as usize,
        "{off} pixels differ"
    );
}

#[test]
fn parallel_tile_renders_are_consistent() {
    let doc = std::sync::Arc::new(open(build(&[busy_page(), busy_page()])));
    let req = full_request(&doc, 1, 3.0, Rotation::R0);
    let size = req.region.size();
    let regions: Vec<PixelRect> = (0..size.height.div_ceil(256))
        .flat_map(|ty| {
            (0..size.width.div_ceil(256)).map(move |tx| {
                PixelRect::new(
                    tx * 256,
                    ty * 256,
                    256.min(size.width - tx * 256),
                    256.min(size.height - ty * 256),
                )
            })
        })
        .collect();
    let sequential: Vec<Vec<u8>> = regions
        .iter()
        .map(|r| render_region(&doc, &req, *r).unwrap().into_data())
        .collect();
    doc.trim_memory(MemoryPressure::Soft); // drop blocks: render them again
    let parallel: Vec<Vec<u8>> = std::thread::scope(|s| {
        let handles: Vec<_> = regions
            .iter()
            .map(|r| {
                let doc = &doc;
                let req = &req;
                s.spawn(move || render_region(doc, req, *r).unwrap().into_data())
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    assert_eq!(sequential, parallel);
}

#[test]
fn bgra_output_and_background() {
    let doc = open(build(&[PageSpec::new(
        [0.0, 0.0, 200.0, 200.0],
        "1 0 0 rg 0 0 100 200 re f",
    )]));
    let mut req = full_request(&doc, 0, 1.0, Rotation::R0);
    req.background = Rgba8::new(0, 0, 255, 255);
    let rgba = render(&doc, &req, PixelFormat::Rgba8Premultiplied).unwrap();
    let bgra = render(&doc, &req, PixelFormat::Bgra8Premultiplied).unwrap();
    assert_eq!(pixel(&rgba, 10, 10), [255, 0, 0, 255]); // red content
    assert_eq!(pixel(&bgra, 10, 10), [0, 0, 255, 255]);
    assert_eq!(pixel(&rgba, 150, 10), [0, 0, 255, 255]); // blue paper
    assert_eq!(pixel(&bgra, 150, 10), [255, 0, 0, 255]);
}

#[test]
fn cancelled_requests_do_not_render() {
    let doc = open(build(&[busy_page()]));
    let req = full_request(&doc, 0, 1.0, Rotation::R0);
    let mut pm =
        fastpdf_engine_api::Pixmap::new(req.region.size(), PixelFormat::default(), doc.limits())
            .unwrap();
    let cancel = CancelToken::new();
    cancel.cancel();
    assert_eq!(
        doc.render(&req, &mut pm.as_mut(), &cancel),
        Err(EngineError::Cancelled)
    );
}

#[test]
fn memory_trimming_keeps_documents_usable() {
    let doc = open(build(&[busy_page()]));
    let req = full_request(&doc, 0, 1.0, Rotation::R0);
    let before = render(&doc, &req, PixelFormat::default()).unwrap();
    for pressure in [
        MemoryPressure::Soft,
        MemoryPressure::Hard,
        MemoryPressure::Normal,
    ] {
        doc.trim_memory(pressure);
        let after = render(&doc, &req, PixelFormat::default()).unwrap();
        assert_eq!(before, after);
    }
    assert!(
        doc.text_layer(PageIndex::FIRST, &CancelToken::new())
            .is_ok()
    );
}

#[test]
fn memory_counters_follow_blocks_trims_and_reopens() {
    use fastpdf_engine_hayro::diagnostics;

    // Other tests run concurrently and share the process-wide counters, so
    // only lower bounds and monotonic growth are checked here.
    let doc = open(build(&[busy_page()]));
    let req = full_request(&doc, 0, 2.0, Rotation::R0);
    let page = req.region;
    // A tile smaller than its block: the block is rendered and cached.
    render_region(&doc, &req, PixelRect::new(0, 0, 256, 256)).unwrap();
    let block = u64::from(page.width) * u64::from(page.height) * 4;
    let m = diagnostics::memory();
    assert!(m.block_cache_bytes >= block, "{m:?}");
    assert!(m.live_documents >= 1 && m.live_generations >= 1, "{m:?}");
    assert!(m.render_threads >= 1, "{m:?}");
    assert!(m.decoded_content_bytes > 0, "{m:?}");

    doc.trim_memory(MemoryPressure::Soft);
    let soft = diagnostics::memory();
    assert!(soft.soft_trims > m.soft_trims, "{soft:?}");

    doc.trim_memory(MemoryPressure::Hard);
    render_region(&doc, &req, PixelRect::new(256, 256, 256, 256)).unwrap();
    let hard = diagnostics::memory();
    assert!(hard.hard_trims > m.hard_trims, "{hard:?}");
    // The hard trim replaced the `Pdf` generation before the second render.
    assert!(hard.reopens > m.reopens, "{hard:?}");
}

#[test]
fn memory_usage_reports_blocks_and_contexts() {
    let doc = open(build(&[busy_page()]));
    let req = full_request(&doc, 0, 2.0, Rotation::R0);
    let page = req.region;
    render_region(&doc, &req, PixelRect::new(0, 0, 256, 256)).unwrap();
    let block = u64::from(page.width) * u64::from(page.height) * 4;
    let used = doc.memory_usage().unwrap();
    // The cached block plus the render context that drew it.
    assert!(used >= 2 * block, "{used} < 2 x {block}");
    doc.trim_memory(MemoryPressure::Soft);
    let trimmed = doc.memory_usage().unwrap();
    assert!(trimmed <= used - block, "{trimmed} vs {used}");
}

#[test]
fn large_images_go_through_the_decode_budget() {
    use fastpdf_engine_hayro::diagnostics;

    // A 6000 x 6000 RGB image needs ~250 MB to decode: far above the
    // threshold, so its render is admitted by the decode budget. The data
    // is truncated, so hayro gives up quickly after the admission.
    let mut spec = PageSpec::new([0.0, 0.0, 200.0, 200.0], "q 200 0 0 200 0 0 cm /Im0 Do Q");
    spec.resources = "/XObject << /Im0 6 0 R >>".into();
    let bytes = build_with(&[spec], |b| {
        b.add_stream(
            "/Type /XObject /Subtype /Image /Width 6000 /Height 6000 /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /FlateDecode",
            zlib(&[128u8; 3000]),
        );
    });
    let doc = open(bytes);
    let before = diagnostics::memory().decode_admissions;
    let req = full_request(&doc, 0, 0.5, Rotation::R0);
    let _ = render(&doc, &req, PixelFormat::default());
    assert!(diagnostics::memory().decode_admissions > before);
}

#[test]
fn encrypted_document_passwords() {
    let page = PageSpec::new([0.0, 0.0, 200.0, 200.0], RED_SQUARE);
    let mut b = PdfBuilder::new();
    let catalog = b.reserve();
    let tree = b.reserve();
    let content = b.add_stream("", "1 0 0 rg 10 10 50 50 re f".into());
    let p = b.add(format!(
        "<< /Type /Page /Parent {tree} 0 R /MediaBox [0 0 {} {}] /Contents {content} 0 R >>",
        page.media_box[2], page.media_box[3]
    ));
    b.set(tree, format!("<< /Type /Pages /Kids [{p} 0 R] /Count 1 >>"));
    b.set(catalog, format!("<< /Type /Catalog /Pages {tree} 0 R >>"));
    b.encrypt_rc4("user-secret", "owner-secret");
    let bytes = b.finish(catalog);

    let limits = ResourceLimits::default;
    assert_eq!(
        open_with(bytes.clone(), None, limits()).unwrap_err(),
        EngineError::PasswordRequired
    );
    assert_eq!(
        open_with(bytes.clone(), Some("wrong"), limits()).unwrap_err(),
        EngineError::InvalidPassword
    );
    let doc = open_with(bytes, Some("user-secret"), limits()).unwrap();
    assert!(doc.metadata().unwrap().encrypted);
    let req = full_request(&doc, 0, 1.0, Rotation::R0);
    let pm = render(&doc, &req, PixelFormat::default()).unwrap();
    // Decrypted content: red square at the bottom-left.
    assert_eq!(pixel(&pm, 30, 170), [255, 0, 0, 255]);
}

#[test]
fn encrypted_fixtures() {
    for (file, password) in [
        ("encrypted/aes256-r6-user-password.pdf", "fastpdf-user"),
        ("encrypted/aes128-user-password.pdf", "fastpdf-user128"),
        (
            "encrypted/aes256-r6-user-password-unicode.pdf",
            "測試密碼2026",
        ),
    ] {
        let Some(bytes) = fixture(file) else { continue };
        let open = |pw: Option<&str>| open_with(bytes.clone(), pw, ResourceLimits::default());
        assert_eq!(
            open(None).unwrap_err(),
            EngineError::PasswordRequired,
            "{file}"
        );
        assert_eq!(
            open(Some("nope")).unwrap_err(),
            EngineError::InvalidPassword,
            "{file}"
        );
        let doc = open(Some(password)).unwrap_or_else(|e| panic!("{file}: {e}"));
        let req = full_request(&doc, 0, 0.5, Rotation::R0);
        render(&doc, &req, PixelFormat::default()).unwrap();
    }
    for file in [
        "encrypted/aes128-owner-only.pdf",
        "encrypted/aes256-r6-owner-only.pdf",
        "encrypted/rc4-40-owner-only.pdf",
        "encrypted/rc4-128-owner-only.pdf",
    ] {
        let Some(bytes) = fixture(file) else { continue };
        let doc = open_with(bytes, None, ResourceLimits::default())
            .unwrap_or_else(|e| panic!("{file}: {e}"));
        assert!(doc.metadata().unwrap().encrypted, "{file}");
    }
}

#[test]
fn malformed_input_fails_cleanly() {
    let limits = ResourceLimits::default;
    for bytes in [
        Vec::new(),
        b"%PDF-1.7\n".to_vec(),
        (0..20_000u32)
            .map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8)
            .collect(),
    ] {
        match open_with(bytes, None, limits()) {
            Err(EngineError::Malformed(_)) => {}
            other => panic!("expected Malformed, got {other:?}"),
        }
    }
    // A valid file cut in half must not panic, whatever the outcome.
    let whole = build(&[busy_page(), busy_page()]);
    for cut in [whole.len() / 3, whole.len() / 2, whole.len() - 40] {
        if let Ok(doc) = open_with(whole[..cut].to_vec(), None, limits()) {
            for page in 0..doc.page_count() {
                let req = full_request(&doc, page, 0.5, Rotation::R0);
                let r = render(&doc, &req, PixelFormat::default());
                assert!(!matches!(r, Err(EngineError::Panicked(_))), "{r:?}");
            }
        }
    }
}

/// Every generated malformed fixture opens or fails with an error; renders
/// and text extraction never panic and finish in bounded time.
#[test]
fn malformed_fixtures_never_panic() {
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/generated/malformed");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        eprintln!("skipping: {} not generated", dir.display());
        return;
    };
    let limits = ResourceLimits {
        max_render_time: Some(Duration::from_secs(4)),
        ..ResourceLimits::default()
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let bytes = std::fs::read(&path).unwrap();
        let started = Instant::now();
        let Ok(doc) = open_with(bytes, None, limits.clone()) else {
            continue;
        };
        for page in 0..doc.page_count().min(10) {
            let Ok(info) = doc.page_info(PageIndex::new(page)) else {
                continue;
            };
            let s = 0.5f32.min(1000.0 / info.size.width.max(info.size.height));
            let Some(scale) = fastpdf_engine_api::RenderScale::new(s) else {
                continue;
            };
            let req = fastpdf_engine_api::RenderRequest::full_page(
                PageIndex::new(page),
                info.size,
                info.rotation,
                Rotation::R0,
                scale,
            );
            let r = render(&doc, &req, PixelFormat::default());
            assert!(
                !matches!(r, Err(EngineError::Panicked(_))),
                "{}: page {page}: {r:?}",
                path.display()
            );
            let t = doc.text_layer(PageIndex::new(page), &CancelToken::new());
            assert!(
                !matches!(t, Err(EngineError::Panicked(_))),
                "{}",
                path.display()
            );
        }
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "{} took {:?}",
            path.display(),
            started.elapsed()
        );
    }
}

/// `MSung-Light` with `UniCNS-UCS2-H`, not embedded (very common in
/// Taiwanese government PDFs).
fn msung_page(text_hex: &str) -> Vec<u8> {
    let mut page = PageSpec::new(
        [0.0, 0.0, 300.0, 120.0],
        &format!("BT /F2 32 Tf 20 50 Td <{text_hex}> Tj ET"),
    );
    page.resources = "/Font << /F2 6 0 R >>".into();
    build_with(&[page], |b| {
        let font = b.reserve();
        assert_eq!(font, 6);
        let descendant = b.reserve();
        let descriptor = b.add(
            "<< /Type /FontDescriptor /FontName /MSung-Light /Flags 6 /FontBBox [0 -200 1000 900] /ItalicAngle 0 /Ascent 880 /Descent -120 /CapHeight 880 /StemV 93 >>",
        );
        b.set(
            font,
            format!(
                "<< /Type /Font /Subtype /Type0 /BaseFont /MSung-Light /Encoding /UniCNS-UCS2-H /DescendantFonts [{descendant} 0 R] >>"
            ),
        );
        b.set(
            descendant,
            format!(
                "<< /Type /Font /Subtype /CIDFontType0 /BaseFont /MSung-Light /CIDSystemInfo << /Registry (Adobe) /Ordering (CNS1) /Supplement 0 >> /FontDescriptor {descriptor} 0 R /DW 1000 >>"
            ),
        );
    })
}

#[test]
fn traditional_chinese_text_layer() {
    // 中文測試 in UCS-2.
    let doc = open(msung_page("4E2D65876E2C8A66"));
    let layer = doc
        .text_layer(PageIndex::FIRST, &CancelToken::new())
        .unwrap();
    let text = layer.plain_text();
    assert!(text.contains("中文測試"), "{text:?}");
    let span = &layer.spans[0];
    let n = span.text.chars().count();
    // Full-width glyphs split the span evenly, so the per-char rectangles
    // are left out and selection splits the span box (text.rs `compact`).
    assert!(span.char_bounds.is_empty() || span.char_bounds.len() == n);
    let first = span.char_bounds.first().copied().unwrap_or_else(|| {
        let w = span.bounds.width() / n as f32;
        fastpdf_engine_api::PageRect::new(
            span.bounds.x0,
            span.bounds.y0,
            span.bounds.x0 + w,
            span.bounds.y1,
        )
    });
    // Page space: y down from the top of the 120 pt page; baseline at 70.
    assert!(first.x0 >= 15.0 && first.x0 <= 25.0, "{first}");
    assert!(first.y0 < 70.0 && first.y1 > 70.0, "{first}");
    assert!(span.bounds.x1 > 140.0, "{}", span.bounds);
}

#[test]
fn non_embedded_cjk_fonts_render_glyphs() {
    let doc = open(msung_page("4E2D65876E2C8A66"));
    let req = full_request(&doc, 0, 2.0, Rotation::R0);
    let pm = render(&doc, &req, PixelFormat::default()).unwrap();
    let ink = pm
        .data()
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|p| p[0] < 100)
        .count();
    if cfg!(windows) {
        // Four 64-px ideographs from PMingLiU / JhengHei.
        assert!(ink > 1500, "only {ink} dark pixels");
        let (_, cjk_hits, _, _) = fastpdf_engine_hayro::diagnostics::font_counters();
        assert!(cjk_hits > 0);
    }
}

#[test]
fn traditional_chinese_fixtures() {
    for (file, needle) in [
        (
            "traditional-chinese/gov-letter-cid-msung-nonembedded.pdf",
            "虛構市政府環境保護局",
        ),
        (
            "traditional-chinese/gov-letter-embedded-type0.pdf",
            "受文者",
        ),
        ("cjk/cid-nonembedded-4lang.pdf", "繁體中文"),
    ] {
        let Some(bytes) = fixture(file) else { continue };
        let doc = open(bytes);
        let text = doc
            .text_layer(PageIndex::FIRST, &CancelToken::new())
            .unwrap()
            .plain_text();
        assert!(text.contains(needle), "{file}: {text:?}");
    }
}

#[test]
fn outline_links_and_metadata() {
    let pages = [
        PageSpec::new([0.0, 0.0, 612.0, 792.0], ""),
        PageSpec::new([0.0, 0.0, 612.0, 792.0], ""),
    ];
    let p0 = page_object(0);
    let p1 = page_object(1);
    let mut page0 = pages;
    page0[0].extra = "/Annots [100 0 R 101 0 R]".into();
    let bytes = build_full(
        &page0,
        "/Outlines 102 0 R /Names << /Dests 105 0 R >>",
        |b| {
            while b.reserve() < 105 {}
            b.set(
            100,
            "<< /Type /Annot /Subtype /Link /Rect [72 700 172 720] /A << /S /URI /URI (https://example.invalid/) >> >>",
        );
            b.set(
            101,
            format!("<< /Type /Annot /Subtype /Link /Rect [72 600 172 620] /Dest [{p1} 0 R /XYZ 72 792 0] >>"),
        );
            b.set(
                102,
                "<< /Type /Outlines /First 103 0 R /Last 104 0 R /Count 2 >>",
            );
            b.set(
            103,
            format!("<< /Title <FEFF7B2C4E007AE0> /Parent 102 0 R /Next 104 0 R /Dest [{p0} 0 R /Fit] /First 106 0 R /Last 106 0 R /Count 1 >>"),
        );
            b.set(
                104,
                "<< /Title (Named) /Parent 102 0 R /Prev 103 0 R /Dest (chapter2) >>",
            );
            b.set(
                105,
                format!("<< /Names [(chapter2) [{p1} 0 R /FitH 500]] >>"),
            );
            let child = b.reserve();
            assert_eq!(child, 106);
            b.set(child, "<< /Title (Child) /Parent 103 0 R /A << /S /URI /URI (https://example.invalid/c) >> >>");
        },
    );
    let doc = open(bytes);

    let outline = doc.outline().unwrap();
    assert_eq!(outline.len(), 2);
    assert_eq!(outline[0].title, "第一章");
    assert!(outline[0].open);
    assert_eq!(
        outline[0].destination.as_ref().unwrap().page,
        PageIndex::new(0)
    );
    assert_eq!(
        outline[0].children[0].uri.as_deref(),
        Some("https://example.invalid/c")
    );
    let named = outline[1].destination.as_ref().unwrap();
    assert_eq!(named.page, PageIndex::new(1));
    assert_eq!(named.view, DestinationView::FitWidth { top: Some(292.0) });

    let links = doc.links(PageIndex::FIRST).unwrap();
    assert_eq!(links.len(), 2);
    assert_eq!(
        links[0].target,
        LinkTarget::Uri("https://example.invalid/".into())
    );
    // Page space: y down from the top of the 792 pt page.
    assert!((links[0].bounds.y0 - 72.0).abs() < 0.01 && (links[0].bounds.y1 - 92.0).abs() < 0.01);
    match &links[1].target {
        LinkTarget::Internal(d) => {
            assert_eq!(d.page, PageIndex::new(1));
            assert_eq!(
                d.view,
                DestinationView::Xyz {
                    left: Some(72.0),
                    top: Some(0.0),
                    zoom: None
                }
            );
        }
        other => panic!("{other:?}"),
    }
    let meta = doc.metadata().unwrap();
    assert_eq!(meta.pdf_version.as_deref(), Some("1.7"));
    assert!(!meta.encrypted);
}

// --- Guardrails ------------------------------------------------------------

fn expect_limit(r: Result<impl std::fmt::Debug, EngineError>, kind: LimitKind) {
    match r {
        Err(EngineError::LimitExceeded(k)) => assert_eq!(k, kind),
        other => panic!("expected {kind:?}, got {other:?}"),
    }
}

#[test]
fn deep_color_space_chains_are_refused_not_overflowed() {
    // 10 000 /Indexed color spaces, each based on the next: hayro would
    // recurse 10 000 deep (it overflows a 1 MiB stack).
    let mut page = PageSpec::new([0.0, 0.0, 100.0, 100.0], "/CS0 cs 0 sc 10 10 50 50 re f");
    page.resources = "/ColorSpace << /CS0 6 0 R >>".into();
    let bytes = build_with(&[page], |b| {
        let first = b.reserve();
        assert_eq!(first, 6);
        let n = 10_000;
        for i in 0..n {
            let obj = first + i;
            if i > 0 {
                b.reserve();
            }
            let base = if i + 1 < n {
                format!("{} 0 R", obj + 1)
            } else {
                "/DeviceRGB".into()
            };
            b.set(obj, format!("[/Indexed {base} 0 <000000>]"));
        }
    });
    let doc = open(bytes);
    let req = full_request(&doc, 0, 1.0, Rotation::R0);
    expect_limit(
        render(&doc, &req, PixelFormat::default()),
        LimitKind::Nesting,
    );
    expect_limit(
        doc.text_layer(PageIndex::FIRST, &CancelToken::new()),
        LimitKind::Nesting,
    );
}

#[test]
fn exponential_form_fan_out_hits_the_time_budget() {
    // 40 forms, each drawing the next one twice: 2^40 draws.
    let mut page = PageSpec::new([0.0, 0.0, 100.0, 100.0], "/X0 Do");
    page.resources = "/XObject << /X0 6 0 R >>".into();
    let bytes = build_with(&[page], |b| {
        let depth = 40;
        let first = b.reserve();
        assert_eq!(first, 6);
        for _ in 1..depth {
            b.reserve();
        }
        for i in 0..depth {
            let obj = first + i;
            if i + 1 < depth {
                let next = obj + 1;
                b.set_stream(
                    obj,
                    &format!(
                        "/Type /XObject /Subtype /Form /BBox [0 0 100 100] /Resources << /XObject << /X {next} 0 R >> >>"
                    ),
                    b"/X Do /X Do".to_vec(),
                );
            } else {
                b.set_stream(
                    obj,
                    "/Type /XObject /Subtype /Form /BBox [0 0 100 100]",
                    b"0 0 1 1 re f".to_vec(),
                );
            }
        }
    });
    let limits = ResourceLimits {
        max_render_time: Some(Duration::from_secs(2)),
        ..ResourceLimits::default()
    };
    let doc = open_with(bytes, None, limits).unwrap();
    let req = full_request(&doc, 0, 1.0, Rotation::R0);
    let started = Instant::now();
    expect_limit(
        render(&doc, &req, PixelFormat::default()),
        LimitKind::RenderTime,
    );
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "{:?}",
        started.elapsed()
    );
    // The verdict is cached: the second attempt fails immediately.
    let started = Instant::now();
    expect_limit(
        render(&doc, &req, PixelFormat::default()),
        LimitKind::RenderTime,
    );
    assert!(started.elapsed() < Duration::from_millis(500));
}

#[test]
fn decompression_bombs_are_refused() {
    // 64 MiB of spaces compresses to ~64 KB.
    let bomb = zlib(&vec![b' '; 64 * 1024 * 1024]);
    let page = PageSpec::new([0.0, 0.0, 100.0, 100.0], "");
    let bytes = build_with(&[page], |b| {
        // Replace the page's content stream (object 4) with the bomb.
        b.set_stream(4, "/Filter /FlateDecode", bomb);
    });
    let limits = ResourceLimits {
        max_object_bytes: 16 * 1024 * 1024,
        ..ResourceLimits::default()
    };
    let doc = open_with(bytes, None, limits).unwrap();
    let req = full_request(&doc, 0, 1.0, Rotation::R0);
    expect_limit(
        render(&doc, &req, PixelFormat::default()),
        LimitKind::ObjectSize,
    );
}

#[test]
fn huge_declared_images_are_refused() {
    let mut page = PageSpec::new([0.0, 0.0, 100.0, 100.0], "q 100 0 0 100 0 0 cm /Im0 Do Q");
    page.resources = "/XObject << /Im0 6 0 R >>".into();
    let bytes = build_with(&[page], |b| {
        b.add_stream(
            "/Type /XObject /Subtype /Image /Width 100000 /Height 100000 /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /FlateDecode",
            zlib(&[0u8; 300]),
        );
    });
    let doc = open(bytes);
    let req = full_request(&doc, 0, 1.0, Rotation::R0);
    expect_limit(
        render(&doc, &req, PixelFormat::default()),
        LimitKind::DecodedImage,
    );
}

#[test]
fn huge_inline_images_are_refused() {
    // Inline images live in the content stream, not in the resources: the
    // interpretation pre-flight has to catch them.
    let content = "q 100 0 0 100 0 0 cm BI /W 100000 /H 100000 /CS /G /BPC 8 ID \0\0 EI Q";
    let doc = open(build(&[PageSpec::new([0.0, 0.0, 100.0, 100.0], content)]));
    let req = full_request(&doc, 0, 1.0, Rotation::R0);
    expect_limit(
        render(&doc, &req, PixelFormat::default()),
        LimitKind::DecodedImage,
    );
}

#[test]
fn passwords_for_plain_files_are_ignored() {
    let doc = open_with(
        build(&[busy_page()]),
        Some("unused"),
        ResourceLimits::default(),
    )
    .unwrap();
    assert!(!doc.metadata().unwrap().encrypted);
}

#[test]
fn huge_pages_and_targets_are_refused_before_hayro() {
    let mut page = PageSpec::new([0.0, 0.0, 14_400.0, 14_400.0], "");
    page.user_unit = Some(75_000.0);
    let normal = PageSpec::new([0.0, 0.0, 14_400.0, 14_400.0], "0 0 1 rg 0 0 100 100 re f");
    let doc = open(build(&[page, normal]));
    // The guard rejects the page size (1.08e9 pt).
    expect_limit(doc.page_info(PageIndex::FIRST), LimitKind::PageDimension);
    // A 14 400 pt page at 8x is 115 200 px: the guard refuses a full-page
    // target, and the adapter would too (vello_cpu panics past 65 532 px).
    let req = full_request(&doc, 1, 8.0, Rotation::R0);
    let r = render(&doc, &req, PixelFormat::default());
    assert!(matches!(r, Err(EngineError::LimitExceeded(_))), "{r:?}");
    // Tiles of the same page are fine.
    let tile = render_region(&doc, &req, PixelRect::new(0, 115_200 - 512, 512, 512)).unwrap();
    assert_eq!(pixel(&tile, 10, 500), [0, 0, 255, 255]);
}
