//! Test helpers: tiny PDF writer (fixtures are generated, never committed)
//! and render/compare utilities.

// Helpers shared by several test crates; each uses a subset. Like #[test]
// bodies (clippy.toml allow-expect-in-tests), they may panic on failure.
#![allow(dead_code, clippy::expect_used)]

use fastpdf_engine_api::{
    CancelToken, DocumentSource, EngineDocument, EngineError, GuardedDocument, OpenOptions,
    PageIndex, PixelFormat, PixelRect, Pixmap, RenderRequest, RenderScale, Rgba8, Rotation,
    SharedBytes, open_guarded,
};
use fastpdf_engine_zpdf::ZpdfEngine;

/// Builds a PDF from numbered object bodies: `objects[i]` becomes object
/// `i + 1`, object 1 must be the catalog.
pub(crate) fn pdf(objects: &[Vec<u8>]) -> Vec<u8> {
    let mut out = b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n".to_vec();
    let mut offsets = Vec::with_capacity(objects.len());
    for (i, body) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref = out.len();
    out.extend_from_slice(
        format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
    );
    for offset in offsets {
        out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    out
}

pub(crate) fn obj(s: &str) -> Vec<u8> {
    s.as_bytes().to_vec()
}

pub(crate) fn stream(dict: &str, data: &[u8]) -> Vec<u8> {
    let mut out = format!("<< {dict} /Length {} >>\nstream\n", data.len()).into_bytes();
    out.extend_from_slice(data);
    out.extend_from_slice(b"\nendstream");
    out
}

/// One page per `(page dictionary extras, content)`; object layout:
/// 1 catalog, 2 pages, 3 Helvetica, then page/content pairs.
/// `extra` is spliced into each page dictionary (MediaBox, Rotate, ...).
pub(crate) fn pages_pdf(pages: &[(&str, &[u8])]) -> Vec<u8> {
    let mut objects = vec![
        obj("<< /Type /Catalog /Pages 2 0 R >>"),
        Vec::new(), // filled below
        obj("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>"),
    ];
    let mut kids = Vec::new();
    for (extra, content) in pages {
        let page_num = objects.len() + 1;
        let content_num = page_num + 1;
        kids.push(format!("{page_num} 0 R"));
        objects.push(obj(&format!(
            "<< /Type /Page /Parent 2 0 R {extra} /Contents {content_num} 0 R \
             /Resources << /Font << /F1 3 0 R >> >> >>"
        )));
        objects.push(stream("", content));
    }
    objects[1] = obj(&format!(
        "<< /Type /Pages /Kids [{}] /Count {} >>",
        kids.join(" "),
        pages.len()
    ));
    pdf(&objects)
}

pub(crate) fn open(bytes: Vec<u8>) -> Result<GuardedDocument, EngineError> {
    open_with(bytes, None)
}

pub(crate) fn open_with(
    bytes: Vec<u8>,
    password: Option<&str>,
) -> Result<GuardedDocument, EngineError> {
    let options = OpenOptions {
        password: password.map(str::to_owned),
        ..OpenOptions::default()
    };
    open_guarded(
        &ZpdfEngine::new(),
        DocumentSource::from_bytes(SharedBytes::from_vec(bytes)),
        &options,
    )
}

pub(crate) fn scale(s: f32) -> RenderScale {
    RenderScale::new(s).expect("valid scale")
}

pub(crate) fn full_request(
    doc: &dyn EngineDocument,
    page: u32,
    s: f32,
    user: Rotation,
) -> RenderRequest {
    let info = doc.page_info(PageIndex::new(page)).expect("page info");
    RenderRequest::full_page(
        PageIndex::new(page),
        info.size,
        info.rotation,
        user,
        scale(s),
    )
}

pub(crate) fn render_request(
    doc: &dyn EngineDocument,
    request: &RenderRequest,
    format: PixelFormat,
) -> Result<Pixmap, EngineError> {
    let mut pixmap = Pixmap::new(request.region.size(), format, &Default::default())?;
    doc.render(request, &mut pixmap.as_mut(), &CancelToken::new())?;
    Ok(pixmap)
}

pub(crate) fn render_full(doc: &dyn EngineDocument, page: u32, s: f32, user: Rotation) -> Pixmap {
    let request = full_request(doc, page, s, user);
    render_request(doc, &request, PixelFormat::Rgba8Premultiplied).expect("render")
}

pub(crate) fn render_region(
    doc: &dyn EngineDocument,
    page: u32,
    s: f32,
    region: PixelRect,
) -> Pixmap {
    let request = full_request(doc, page, s, Rotation::R0).with_region(region);
    render_request(doc, &request, PixelFormat::Rgba8Premultiplied).expect("render region")
}

pub(crate) fn pixel(pm: &Pixmap, x: u32, y: u32) -> [u8; 4] {
    let i = (y as usize * pm.size().width as usize + x as usize) * 4;
    let d = pm.data();
    [d[i], d[i + 1], d[i + 2], d[i + 3]]
}

/// Compares `tile` with the same-sized crop of `full` at `region`:
/// returns (pixels differing by more than `tol` in any channel, max delta).
pub(crate) fn compare_crop(
    full: &Pixmap,
    tile: &Pixmap,
    region: PixelRect,
    tol: u8,
) -> (usize, u8) {
    let mut bad = 0;
    let mut max = 0;
    for y in 0..region.height {
        for x in 0..region.width {
            let a = pixel(full, region.x + x, region.y + y);
            let b = pixel(tile, x, y);
            let d = a
                .iter()
                .zip(b)
                .map(|(p, q)| p.abs_diff(q))
                .max()
                .unwrap_or(0);
            max = max.max(d);
            if d > tol {
                bad += 1;
            }
        }
    }
    (bad, max)
}

/// Like [`compare_crop`], but a tile pixel only counts as wrong when no
/// pixel within `radius` of its position in `full` is within `tol`. This
/// absorbs anti-aliasing differences (tiny-skia clips hairlines to the
/// raster before stepping, so a line entering a tile mid-way can shift its
/// coverage by a fraction of a pixel) while still catching content that is
/// missing or misplaced by more than `radius` pixels.
pub(crate) fn misplaced_pixels(
    full: &Pixmap,
    tile: &Pixmap,
    region: PixelRect,
    tol: u8,
    radius: u32,
) -> usize {
    let (fw, fh) = (full.size().width, full.size().height);
    let mut bad = 0;
    for y in 0..region.height {
        for x in 0..region.width {
            let b = pixel(tile, x, y);
            let (cx, cy) = (region.x + x, region.y + y);
            let ok = (cy.saturating_sub(radius)..=(cy + radius).min(fh - 1)).any(|fy| {
                (cx.saturating_sub(radius)..=(cx + radius).min(fw - 1)).any(|fx| {
                    let a = pixel(full, fx, fy);
                    a.iter().zip(b).all(|(p, q)| p.abs_diff(q) <= tol)
                })
            });
            if !ok {
                bad += 1;
            }
        }
    }
    bad
}

pub(crate) const RED: Rgba8 = Rgba8::new(255, 0, 0, 255);

/// True when `px` (premultiplied RGBA) is close to the opaque color `c`.
pub(crate) fn is_color(px: [u8; 4], c: [u8; 3]) -> bool {
    px[3] > 250
        && px[0].abs_diff(c[0]) < 24
        && px[1].abs_diff(c[1]) < 24
        && px[2].abs_diff(c[2]) < 24
}

/// A Letter page exercising fills, strokes of several widths, curves, a
/// clip, an inline image, a transparent fill and (system-font) text.
pub(crate) fn busy_content() -> Vec<u8> {
    let mut c = Vec::new();
    c.extend_from_slice(b"0.9 0.95 1 rg 0 0 612 792 re f\n");
    // Concentric circles made of Bezier curves.
    for i in 1..12 {
        let r = 20.0 * f64::from(i);
        let k = r * 0.5523;
        let (cx, cy) = (306.0, 420.0);
        c.extend_from_slice(
            format!(
                "{w} w {g} G {x0} {cy} m {x0} {y1} {x1} {y2} {cx} {y2} c \
                 {x2} {y2} {x3} {y1} {x3} {cy} c {x3} {y3} {x2} {y4} {cx} {y4} c \
                 {x1} {y4} {x0} {y3} {x0} {cy} c S\n",
                w = 0.25 * f64::from(i),
                g = f64::from(i) / 14.0,
                x0 = cx - r,
                x1 = cx - k,
                x2 = cx + k,
                x3 = cx + r,
                y1 = cy + k,
                y2 = cy + r,
                y3 = cy - k,
                y4 = cy - r,
            )
            .as_bytes(),
        );
    }
    // Diagonal hairlines crossing many tile borders.
    for i in 0..40 {
        let x = 15.0 * f64::from(i);
        c.extend_from_slice(format!("0 w 0 0 0 RG {x} 0 m {} 792 l S\n", x + 200.0).as_bytes());
    }
    // Clipped red/green checker.
    c.extend_from_slice(b"q 60 60 200 120 re W n\n");
    for i in 0..8 {
        for j in 0..4 {
            let color = if (i + j) % 2 == 0 { "1 0 0" } else { "0 0.6 0" };
            c.extend_from_slice(
                format!("{color} rg {} {} 30 30 re f\n", 50 + i * 30, 50 + j * 30).as_bytes(),
            );
        }
    }
    c.extend_from_slice(b"Q\n");
    // Semi-transparent overlap via ExtGState-free alpha: use an inline image instead.
    c.extend_from_slice(b"q 120 0 0 120 420 80 cm BI /W 2 /H 2 /CS /RGB /BPC 8 ID ");
    c.extend_from_slice(&[255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 0]);
    c.extend_from_slice(b" EI Q\n");
    c.extend_from_slice(
        b"BT /F1 36 Tf 40 700 Td (Tile seams?) Tj 0 -44 Td (0123456789 AWVA) Tj ET\n",
    );
    c
}

/// MediaBox and CropBox with non-zero origins; blue fill of the visible box,
/// a red square in its top-left corner and a green one bottom-right.
pub(crate) fn marker_content() -> Vec<u8> {
    b"0 0 1 rg 120 140 560 720 re f\n\
      1 0 0 rg 120 820 40 40 re f\n\
      0 1 0 rg 640 140 40 40 re f\n"
        .to_vec()
}

/// About 60k short strokes: slow enough to observe cancellation.
pub(crate) fn heavy_content() -> Vec<u8> {
    let mut c = b"0.2 w 0 0 0 RG\n".to_vec();
    let mut seed = 12345u32;
    let mut next = || {
        seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
        f64::from(seed >> 16) / 65536.0
    };
    for _ in 0..60_000 {
        let (x, y) = (next() * 2384.0, next() * 3370.0);
        c.extend_from_slice(
            format!(
                "{x:.1} {y:.1} m {:.1} {:.1} l S\n",
                x + next() * 40.0 - 20.0,
                y + next() * 40.0 - 20.0
            )
            .as_bytes(),
        );
    }
    c
}
