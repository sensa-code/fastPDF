//! Rendering behavior of the zpdf adapter: tiles, rotation, formats,
//! cancellation, threading and memory trimming.

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use common::*;
use fastpdf_engine_api::{
    CancelToken, EngineDocument, EngineError, MemoryPressure, PageIndex, PageSize, PdfEngine,
    PixelFormat, PixelRect, Pixmap, Rgba8, Rotation,
};
use fastpdf_engine_zpdf::ZpdfEngine;

/// Largest per-channel difference accepted as anti-aliasing noise between a
/// tile and the full-page render. tiny-skia chops curves at the raster edge,
/// so curve edges near a tile border are approximated by slightly different
/// line segments (measured maximum: 50/255). Missing or misplaced content
/// differs by far more.
const AA_TOLERANCE: u8 = 64;

#[test]
fn tiles_match_the_full_page_crop() {
    let bytes = pages_pdf(&[("/MediaBox [0 0 612 792]", &busy_content())]);
    let doc = open(bytes).expect("open");
    let s = 2.0;
    let full = render_full(&doc, 0, s, Rotation::R0);
    assert_eq!(full.size().width, 1224);
    assert_eq!(full.size().height, 1584);
    let tile = 256;
    let mut worst_fraction = 0.0f64;
    let mut checked = 0;
    for ty in (0..full.size().height).step_by(tile) {
        for tx in (0..full.size().width).step_by(tile) {
            let region = PixelRect::new(
                tx,
                ty,
                (tile as u32).min(full.size().width - tx),
                (tile as u32).min(full.size().height - ty),
            );
            let part = render_region(&doc, 0, s, region);
            assert_eq!(part.size(), region.size());
            let (bad, max) = compare_crop(&full, &part, region, 16);
            let fraction = bad as f64 / f64::from(region.width * region.height);
            worst_fraction = worst_fraction.max(fraction);
            // Exact apart from anti-aliasing: nothing missing or shifted.
            assert_eq!(
                misplaced_pixels(&full, &part, region, AA_TOLERANCE, 1),
                0,
                "tile {region:?}: {bad} pixels differ (max delta {max})"
            );
            assert!(
                fraction <= 0.02,
                "tile {region:?}: {bad} pixels differ (max delta {max})"
            );
            checked += 1;
        }
    }
    assert_eq!(checked, 35);
    eprintln!("worst tile mismatch: {:.4}%", worst_fraction * 100.0);
}

#[test]
fn odd_sized_regions_and_small_scales_have_exact_sizes() {
    let bytes = pages_pdf(&[("/MediaBox [0 0 595.3 841.9]", &busy_content())]);
    let doc = open(bytes).expect("open");
    for s in [0.07, 0.33, 1.0, 1.37, 3.1] {
        let full = render_full(&doc, 0, s, Rotation::R0);
        let w = full.size().width;
        let h = full.size().height;
        let region = PixelRect::new(w / 3, h / 5, (w / 2).max(1), (h / 3).max(1));
        let part = render_region(&doc, 0, s, region);
        assert_eq!(part.size(), region.size(), "scale {s}");
        let (bad, max) = compare_crop(&full, &part, region, 24);
        assert_eq!(
            misplaced_pixels(&full, &part, region, AA_TOLERANCE, 1),
            0,
            "scale {s}: {bad} pixels differ (max delta {max})"
        );
    }
}

const BLUE: [u8; 3] = [0, 0, 255];
const RED_RGB: [u8; 3] = [255, 0, 0];
const GREEN: [u8; 3] = [0, 255, 0];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Corner {
    TopLeft,
    TopRight,
    BottomRight,
    BottomLeft,
}

/// Where the original top-left corner ends up after a clockwise rotation.
fn rotated(corner: Corner, rotation: Rotation) -> Corner {
    let order = [
        Corner::TopLeft,
        Corner::TopRight,
        Corner::BottomRight,
        Corner::BottomLeft,
    ];
    let start = order.iter().position(|c| *c == corner).unwrap_or(0);
    order[(start + rotation.quarter_turns() as usize) % 4]
}

fn corner_pixel(pm: &Pixmap, corner: Corner) -> [u8; 4] {
    let (w, h) = (pm.size().width, pm.size().height);
    let inset = 6;
    let (x, y) = match corner {
        Corner::TopLeft => (inset, inset),
        Corner::TopRight => (w - 1 - inset, inset),
        Corner::BottomRight => (w - 1 - inset, h - 1 - inset),
        Corner::BottomLeft => (inset, h - 1 - inset),
    };
    pixel(pm, x, y)
}

#[test]
fn rotated_pages_with_offset_boxes_render_in_place() {
    let content = marker_content();
    let boxes = "/MediaBox [100 100 712 892] /CropBox [120 140 680 860]";
    let extras: Vec<String> = [0, 90, 180, 270, -90, 450]
        .iter()
        .map(|r| format!("{boxes} /Rotate {r}"))
        .collect();
    let pages: Vec<(&str, &[u8])> = extras
        .iter()
        .map(|e| (e.as_str(), content.as_slice()))
        .collect();
    let doc = open(pages_pdf(&pages)).expect("open");
    assert_eq!(doc.page_count(), 6);
    let expected = [
        Rotation::R0,
        Rotation::R90,
        Rotation::R180,
        Rotation::R270,
        Rotation::R270,
        Rotation::R90,
    ];
    for (page, intrinsic) in expected.iter().enumerate() {
        let info = doc.page_info(PageIndex::new(page as u32)).expect("info");
        assert_eq!(info.size, PageSize::new(560.0, 720.0), "page {page}");
        assert_eq!(info.rotation, *intrinsic, "page {page}");
        for user in [Rotation::R0, Rotation::R90, Rotation::R270] {
            let total = intrinsic.then(user);
            let pm = render_full(&doc, page as u32, 0.5, user);
            let expected_size = scale(0.5).page_pixels(info.size, total);
            assert_eq!(pm.size(), expected_size, "page {page} user {user:?}");
            let red_at = rotated(Corner::TopLeft, total);
            let green_at = rotated(Corner::BottomRight, total);
            assert!(
                is_color(corner_pixel(&pm, red_at), RED_RGB),
                "page {page} user {user:?}: red expected at {red_at:?}"
            );
            assert!(
                is_color(corner_pixel(&pm, green_at), GREEN),
                "page {page} user {user:?}: green expected at {green_at:?}"
            );
            // No white band anywhere along the edges: content is not shifted.
            let (w, h) = (pm.size().width, pm.size().height);
            for (x, y) in [(w / 2, 1), (w / 2, h - 2), (1, h / 2), (w - 2, h / 2)] {
                assert!(
                    is_color(pixel(&pm, x, y), BLUE),
                    "page {page} user {user:?}: edge pixel ({x},{y}) = {:?}",
                    pixel(&pm, x, y)
                );
            }
        }
    }
}

#[test]
fn rotated_tiles_match_the_rotated_full_page() {
    let content = busy_content();
    let doc = open(pages_pdf(&[(
        "/MediaBox [30 40 642 832] /Rotate 90",
        content.as_slice(),
    )]))
    .expect("open");
    let full = render_full(&doc, 0, 1.5, Rotation::R180);
    let (w, h) = (full.size().width, full.size().height);
    assert_eq!((w, h), (1188, 918)); // 792 x 612 points after R90 + R180 = R270
    for region in [
        PixelRect::new(0, 0, 300, 200),
        PixelRect::new(w - 300, h - 200, 300, 200),
        PixelRect::new(450, 300, 256, 256),
    ] {
        let request = full_request(&doc, 0, 1.5, Rotation::R180).with_region(region);
        let part = render_request(&doc, &request, PixelFormat::Rgba8Premultiplied).expect("tile");
        let (bad, max) = compare_crop(&full, &part, region, 16);
        assert_eq!(
            misplaced_pixels(&full, &part, region, AA_TOLERANCE, 1),
            0,
            "{region:?}: {bad} differ, max {max}"
        );
    }
}

#[test]
fn bgra_and_rgba_targets_differ_only_in_channel_order() {
    let doc = open(pages_pdf(&[(
        "/MediaBox [0 0 200 200]",
        b"1 0 0 rg 0 0 100 200 re f".as_slice(),
    )]))
    .expect("open");
    let request = full_request(&doc, 0, 1.0, Rotation::R0);
    let rgba = render_request(&doc, &request, PixelFormat::Rgba8Premultiplied).expect("rgba");
    let bgra = render_request(&doc, &request, PixelFormat::Bgra8Premultiplied).expect("bgra");
    assert_eq!(pixel(&rgba, 10, 10), [255, 0, 0, 255]);
    assert_eq!(pixel(&bgra, 10, 10), [0, 0, 255, 255]);
    let (rgba_px, _) = rgba.data().as_chunks::<4>();
    let (bgra_px, _) = bgra.data().as_chunks::<4>();
    for (a, b) in rgba_px.iter().zip(bgra_px) {
        assert_eq!([a[2], a[1], a[0], a[3]], *b);
    }
}

#[test]
fn background_is_painted_first() {
    let doc = open(pages_pdf(&[(
        "/MediaBox [0 0 200 200]",
        b"0 0 1 rg 0 0 50 50 re f".as_slice(),
    )]))
    .expect("open");
    let mut request = full_request(&doc, 0, 1.0, Rotation::R0);
    request.background = Rgba8::TRANSPARENT;
    let clear = render_request(&doc, &request, PixelFormat::Rgba8Premultiplied).expect("render");
    assert_eq!(pixel(&clear, 150, 20), [0, 0, 0, 0]);
    assert_eq!(pixel(&clear, 10, 190), [0, 0, 255, 255]);
    request.background = Rgba8::new(255, 255, 0, 255);
    let yellow = render_request(&doc, &request, PixelFormat::Rgba8Premultiplied).expect("render");
    assert_eq!(pixel(&yellow, 150, 20), [255, 255, 0, 255]);
}

/// Red bounding box (min x, min y, max x, max y) of `pm`, if any.
fn red_bounds(pm: &Pixmap) -> Option<(u32, u32, u32, u32)> {
    let mut found: Option<(u32, u32, u32, u32)> = None;
    for y in 0..pm.size().height {
        for x in 0..pm.size().width {
            if is_color(pixel(pm, x, y), RED_RGB) {
                let b = found.get_or_insert((x, y, x, y));
                *b = (b.0.min(x), b.1.min(y), b.2.max(x), b.3.max(y));
            }
        }
    }
    found
}

#[test]
fn annotations_stay_in_place_on_rotated_offset_pages() {
    // A 50 pt square annotation in the top-left corner of a MediaBox that
    // does not start at the origin.
    let expected = [
        (0, (0, 0, 49, 49)),
        (90, (150, 0, 199, 49)),
        (180, (150, 150, 199, 199)),
        (270, (0, 150, 49, 199)),
    ];
    for (rotate, bounds) in expected {
        let objects = vec![
            obj("<< /Type /Catalog /Pages 2 0 R >>"),
            obj("<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            obj(&format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [100 100 300 300] /Rotate {rotate}                  /Contents 4 0 R /Annots [5 0 R] >>"
            )),
            stream("", b"0 0 1 rg 100 100 200 200 re f"),
            obj("<< /Type /Annot /Subtype /Square /Rect [100 250 150 300] /AP << /N 6 0 R >> >>"),
            stream(
                "/Type /XObject /Subtype /Form /BBox [0 0 50 50]",
                b"1 0 0 rg 0 0 50 50 re f",
            ),
        ];
        let doc = open(pdf(&objects)).expect("open");
        let pm = render_full(&doc, 0, 1.0, Rotation::R0);
        assert_eq!(red_bounds(&pm), Some(bounds), "/Rotate {rotate}");
    }
}

#[test]
fn annotation_flag_controls_appearance_streams() {
    let content = stream("", b"0 0 1 rg 0 0 10 10 re f");
    let objects = vec![
        obj("<< /Type /Catalog /Pages 2 0 R >>"),
        obj("<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        obj(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R /Annots [5 0 R] >>",
        ),
        content,
        obj("<< /Type /Annot /Subtype /Square /Rect [100 100 180 180] /AP << /N 6 0 R >> >>"),
        stream(
            "/Type /XObject /Subtype /Form /BBox [0 0 80 80]",
            b"1 0 0 rg 0 0 80 80 re f",
        ),
    ];
    let doc = open(pdf(&objects)).expect("open");
    let mut request = full_request(&doc, 0, 1.0, Rotation::R0);
    let with = render_request(&doc, &request, PixelFormat::Rgba8Premultiplied).expect("render");
    assert!(is_color(pixel(&with, 140, 60), RED_RGB));
    request.annotations = false;
    let without = render_request(&doc, &request, PixelFormat::Rgba8Premultiplied).expect("render");
    assert!(is_color(pixel(&without, 140, 60), [255, 255, 255]));
}

#[test]
fn cancellation_stops_rendering_between_commands() {
    let doc = Arc::new(
        open(pages_pdf(&[(
            "/MediaBox [0 0 2384 3370]",
            &heavy_content(),
        )]))
        .expect("open"),
    );
    // Warm up: the page is interpreted once and cached.
    let _ = render_region(doc.as_ref(), 0, 0.25, PixelRect::new(0, 0, 64, 64));

    let request = full_request(doc.as_ref(), 0, 1.5, Rotation::R0);
    let started = Instant::now();
    let mut full = Pixmap::new(
        request.region.size(),
        PixelFormat::default(),
        &Default::default(),
    )
    .expect("alloc");
    doc.render(&request, &mut full.as_mut(), &CancelToken::new())
        .expect("uncancelled render");
    let uncancelled = started.elapsed();

    let cancel = CancelToken::new();
    let worker = {
        let doc = Arc::clone(&doc);
        let request = request.clone();
        let cancel = cancel.clone();
        std::thread::spawn(move || {
            let mut pm = Pixmap::new(
                request.region.size(),
                PixelFormat::default(),
                &Default::default(),
            )
            .expect("alloc");
            let started = Instant::now();
            let result = doc.render(&request, &mut pm.as_mut(), &cancel);
            (result, started.elapsed())
        })
    };
    std::thread::sleep(uncancelled / 10);
    cancel.cancel();
    let (result, elapsed) = worker.join().expect("join");
    assert_eq!(result, Err(EngineError::Cancelled));
    eprintln!("uncancelled {uncancelled:?}, cancelled after {elapsed:?}");
    assert!(elapsed < uncancelled, "cancel did not shorten the render");
}

#[test]
fn a_cancelled_token_never_starts_work() {
    let doc = ZpdfEngine::new()
        .open(
            fastpdf_engine_api::DocumentSource::from_bytes(
                fastpdf_engine_api::SharedBytes::from_vec(pages_pdf(&[(
                    "/MediaBox [0 0 100 100]",
                    b"0 g 0 0 10 10 re f".as_slice(),
                )])),
            ),
            &Default::default(),
        )
        .expect("open");
    let request = full_request(doc.as_ref(), 0, 1.0, Rotation::R0);
    let mut pm = Pixmap::new(
        request.region.size(),
        PixelFormat::default(),
        &Default::default(),
    )
    .expect("alloc");
    let cancel = CancelToken::new();
    cancel.cancel();
    assert_eq!(
        doc.render(&request, &mut pm.as_mut(), &cancel),
        Err(EngineError::Cancelled)
    );
}

#[test]
fn tiles_render_in_parallel_with_identical_results() {
    let doc =
        Arc::new(open(pages_pdf(&[("/MediaBox [0 0 612 792]", &busy_content())])).expect("open"));
    let s = 1.5;
    let size = scale(s).page_pixels(PageSize::new(612.0, 792.0), Rotation::R0);
    let mut regions = Vec::new();
    for y in (0..size.height).step_by(200) {
        for x in (0..size.width).step_by(200) {
            regions.push(PixelRect::new(
                x,
                y,
                200.min(size.width - x),
                200.min(size.height - y),
            ));
        }
    }
    let sequential: Vec<Vec<u8>> = regions
        .iter()
        .map(|r| render_region(doc.as_ref(), 0, s, *r).into_data())
        .collect();
    doc.trim_memory(MemoryPressure::Soft); // force re-interpretation under contention
    let parallel: Vec<Vec<u8>> = std::thread::scope(|scope| {
        let handles: Vec<_> = regions
            .iter()
            .map(|r| {
                let doc = Arc::clone(&doc);
                let r = *r;
                scope.spawn(move || render_region(doc.as_ref(), 0, s, r).into_data())
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().expect("join"))
            .collect()
    });
    assert_eq!(sequential.len(), parallel.len());
    for (i, (a, b)) in sequential.iter().zip(&parallel).enumerate() {
        assert!(a == b, "tile {i} differs between sequential and parallel");
    }
}

#[test]
fn trimming_memory_keeps_results_identical() {
    let doc = open(pages_pdf(&[
        ("/MediaBox [0 0 612 792]", &busy_content()),
        ("/MediaBox [0 0 300 300] /Rotate 90", &marker_content()),
    ]))
    .expect("open");
    let before = render_full(&doc, 0, 0.75, Rotation::R0);
    let before_2 = render_full(&doc, 1, 0.75, Rotation::R0);
    doc.trim_memory(MemoryPressure::Soft);
    assert_eq!(render_full(&doc, 0, 0.75, Rotation::R0), before);
    doc.trim_memory(MemoryPressure::Hard);
    assert_eq!(render_full(&doc, 0, 0.75, Rotation::R0), before);
    assert_eq!(render_full(&doc, 1, 0.75, Rotation::R0), before_2);
    doc.trim_memory(MemoryPressure::Normal);
}

#[test]
fn regions_outside_the_page_are_rejected_by_the_guard() {
    let doc = open(pages_pdf(&[("/MediaBox [0 0 100 100]", b"".as_slice())])).expect("open");
    let request =
        full_request(&doc, 0, 1.0, Rotation::R0).with_region(PixelRect::new(90, 90, 20, 20));
    assert!(matches!(
        render_request(&doc, &request, PixelFormat::default()),
        Err(EngineError::InvalidRequest(_))
    ));
}

#[test]
fn large_pages_render_tiles_without_full_page_cost() {
    let doc = open(pages_pdf(&[(
        "/MediaBox [0 0 2384 3370]",
        &heavy_content(),
    )]))
    .expect("open");
    // First call interprets; time the second, cached one.
    let region = PixelRect::new(4096, 4096, 512, 512);
    let _ = render_region(&doc, 0, 4.0, region);
    let started = Instant::now();
    let tile = render_region(&doc, 0, 4.0, region);
    let elapsed = started.elapsed();
    assert_eq!(tile.size(), region.size());
    eprintln!("512px tile of a 60k-stroke A0 page at 4x: {elapsed:?}");
    assert!(elapsed < Duration::from_secs(5));
}
