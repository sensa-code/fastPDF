//! Shared helpers for the printing tests.
//!
//! Safety rule for every test in this crate: jobs go **only** to the virtual
//! printer "Microsoft Print to PDF", **always** with `output_file` set to a
//! file under the cargo target directory. [`pdf_printer`] verifies the queue
//! really is that virtual printer (driver and port) and otherwise returns
//! `None`, and the test is reported as skipped. No dialog is shown and no
//! printer setting is touched: jobs use the printer's defaults.

// Helpers shared by several test binaries; each uses a subset. Like #[test]
// bodies (clippy.toml allow-expect-in-tests), they may panic on failure.
#![allow(dead_code, clippy::expect_used, clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use fastpdf_engine_api::{
    CancelToken, DocumentSource, EngineDocument, GuardedDocument, OpenOptions, PageIndex,
    PixelFormat, Pixmap, RenderRequest, RenderScale, Rotation, SharedBytes, open_guarded,
};
use fastpdf_engine_zpdf::ZpdfEngine;
use fastpdf_print::{PrintJob, list_printers};

/// The only printer tests may use.
pub(crate) const PDF_PRINTER: &str = "Microsoft Print to PDF";

/// Name of the virtual PDF printer, or `None` (after printing a SKIPPED
/// notice) when it is missing or does not look exactly like the built-in
/// Windows queue: driver "Microsoft Print To PDF" on port `PORTPROMPT:`.
pub(crate) fn pdf_printer(test: &str) -> Option<String> {
    let found = list_printers().into_iter().find(|p| p.name == PDF_PRINTER);
    match found {
        Some(p)
            if p.is_virtual
                && p.driver.eq_ignore_ascii_case("Microsoft Print To PDF")
                && p.port.eq_ignore_ascii_case("PORTPROMPT:") =>
        {
            Some(p.name)
        }
        other => {
            eprintln!("SKIPPED {test}: virtual printer `{PDF_PRINTER}` not available ({other:?})");
            None
        }
    }
}

/// A job for the verified virtual printer that writes `output`.
pub(crate) fn pdf_job(printer: &str, output: &Path) -> PrintJob {
    PrintJob {
        printer_name: Some(printer.to_owned()),
        output_file: Some(output.to_owned()),
        document_name: format!(
            "FastPDF test {}",
            output.file_stem().unwrap_or_default().display()
        ),
        ..PrintJob::default()
    }
}

/// A fresh output path under the target directory (`<target>/tmp`). A file
/// left there by a previous run of the same test is removed first, so the
/// test can tell when the spooler has written the new one.
pub(crate) fn output_path(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("fastpdf-print");
    std::fs::create_dir_all(&dir).expect("create output directory");
    let path = dir.join(format!("{name}.pdf"));
    if path.exists() {
        std::fs::remove_file(&path).expect("remove previous test output");
    }
    path
}

/// The spooler writes the PDF asynchronously after `EndDoc`; waits until the
/// file is complete (ends with `%%EOF` and stopped growing).
pub(crate) fn wait_for_pdf(path: &Path, timeout: Duration) -> Vec<u8> {
    let start = Instant::now();
    let mut last_len = None;
    loop {
        // Reading can fail while the spooler still holds the file open.
        if let Ok(bytes) = std::fs::read(path) {
            let complete = bytes.trim_ascii_end().ends_with(b"%%EOF");
            if complete && last_len == Some(bytes.len()) {
                return bytes;
            }
            last_len = Some(bytes.len());
        }
        assert!(
            start.elapsed() < timeout,
            "spooler did not write {} within {timeout:?}",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Generated fixture (fixtures are not committed); `None` with a notice when
/// it has not been generated.
pub(crate) fn fixture(relative: &str) -> Option<GuardedDocument> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/generated")
        .join(relative);
    match std::fs::read(&path) {
        Ok(bytes) => Some(open_pdf(bytes)),
        Err(e) => {
            eprintln!("SKIPPED: fixture {} unavailable ({e})", path.display());
            None
        }
    }
}

pub(crate) fn open_pdf(bytes: Vec<u8>) -> GuardedDocument {
    open_guarded(
        &ZpdfEngine::new(),
        DocumentSource::from_bytes(SharedBytes::from_vec(bytes)),
        &OpenOptions::default(),
    )
    .expect("open PDF")
}

/// Renders a whole page at `scale` (RGBA, white background).
pub(crate) fn render(doc: &GuardedDocument, page: u32, scale: f32) -> Pixmap {
    let page = PageIndex::new(page);
    let info = doc.page_info(page).expect("page info");
    let request = RenderRequest::full_page(
        page,
        info.size,
        info.rotation,
        Rotation::R0,
        RenderScale::new(scale).expect("scale"),
    );
    let mut pixmap = Pixmap::new(
        request.region.size(),
        PixelFormat::Rgba8Premultiplied,
        &Default::default(),
    )
    .expect("pixmap");
    doc.render(&request, &mut pixmap.as_mut(), &CancelToken::new())
        .expect("render");
    pixmap
}

/// Inked (clearly non-white) pixels of a rendered page as (x, y) pairs.
/// The threshold keeps light-gray text but ignores JPEG ringing, which the
/// PDF printer adds around strokes.
fn inked(pixmap: &Pixmap) -> impl Iterator<Item = (u32, u32)> + '_ {
    let width = pixmap.size().width;
    let (px, _) = pixmap.data().as_chunks::<4>();
    px.iter()
        .enumerate()
        .filter(|(_, p)| p[..3].iter().any(|&c| c < 225))
        .map(move |(i, _)| ((i as u32) % width, (i as u32) / width))
}

/// Fraction of the page's pixels that are inked.
pub(crate) fn ink_ratio(pixmap: &Pixmap) -> f64 {
    inked(pixmap).count() as f64 / pixmap.size().area() as f64
}

/// Bounding box of the ink as fractions of the page: (x0, y0, x1, y1).
pub(crate) fn ink_box(pixmap: &Pixmap) -> Option<(f64, f64, f64, f64)> {
    let (w, h) = (
        f64::from(pixmap.size().width),
        f64::from(pixmap.size().height),
    );
    inked(pixmap)
        .fold(None, |acc: Option<(u32, u32, u32, u32)>, (x, y)| {
            Some(match acc {
                None => (x, y, x + 1, y + 1),
                Some((x0, y0, x1, y1)) => (x0.min(x), y0.min(y), x1.max(x + 1), y1.max(y + 1)),
            })
        })
        .map(|(x0, y0, x1, y1)| {
            (
                f64::from(x0) / w,
                f64::from(y0) / h,
                f64::from(x1) / w,
                f64::from(y1) / h,
            )
        })
}

/// Builds a PDF from numbered object bodies: `objects[i]` becomes object
/// `i + 1`; object 1 must be the catalog.
fn pdf(objects: &[Vec<u8>]) -> Vec<u8> {
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

/// One page per `(width, height, content stream)`, in points.
pub(crate) fn pages_pdf(pages: &[(f32, f32, &str)]) -> Vec<u8> {
    let mut objects = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        Vec::new(), // the page tree, filled below
    ];
    let mut kids = Vec::new();
    for (width, height, content) in pages {
        let page = objects.len() + 1;
        kids.push(format!("{page} 0 R"));
        objects.push(
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {width} {height}] \
                 /Contents {} 0 R /Resources << >> >>",
                page + 1
            )
            .into_bytes(),
        );
        objects.push(
            format!(
                "<< /Length {} >>\nstream\n{content}\nendstream",
                content.len()
            )
            .into_bytes(),
        );
    }
    objects[1] = format!(
        "<< /Type /Pages /Kids [{}] /Count {} >>",
        kids.join(" "),
        pages.len()
    )
    .into_bytes();
    pdf(&objects)
}
