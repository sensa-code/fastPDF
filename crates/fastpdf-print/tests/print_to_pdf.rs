//! End-to-end printing through GDI into the virtual printer "Microsoft Print
//! to PDF" (never a physical printer; see `common`). The produced PDF is
//! reopened with the zpdf adapter and checked page by page.

#![cfg(windows)]
// Test helpers, like #[test] bodies (clippy.toml), may panic on failure.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::time::Duration;

use common::*;
use fastpdf_engine_api::{
    CancelToken, DocumentSource, EngineCapabilities, EngineDocument, EngineError, EngineInfo,
    OpenOptions, PageIndex, PageInfo, PageSize, PdfEngine, PixmapMut, RenderOutcome, RenderRequest,
    Rgba8, Rotation, SharedBytes, open_guarded,
};
use fastpdf_print::{
    BAND_ROWS, FitMode, MAX_BAND_WIDTH, PageRange, PrintError, PrintJob, PrintProgress,
    PrintReport, default_printer, list_printers, print,
};

/// Generous: the spooler converts the job after `EndDoc`, and several tests
/// print at the same time.
const SPOOL_TIMEOUT: Duration = Duration::from_secs(180);

fn max_band_bytes() -> usize {
    BAND_ROWS as usize * MAX_BAND_WIDTH as usize * 4
}

/// Progress must walk every sheet in order, each from 0 to all its bands.
fn check_progress(progress: &[PrintProgress], sheets: u32) {
    assert!(!progress.is_empty());
    let mut last = (0, 0);
    for p in progress {
        assert_eq!(p.sheets, sheets);
        assert!(p.bands_done <= p.bands);
        let now = (p.sheet, p.bands_done);
        assert!(now >= last, "progress went backwards: {last:?} -> {now:?}");
        last = now;
    }
    for sheet in 1..=sheets {
        let reports: Vec<_> = progress.iter().filter(|p| p.sheet == sheet).collect();
        assert_eq!(
            reports.first().map(|p| p.bands_done),
            Some(0),
            "sheet {sheet}"
        );
        let end = reports.last().unwrap();
        assert_eq!(end.bands_done, end.bands, "sheet {sheet} incomplete");
    }
}

fn summary(name: &str, report: &PrintReport) {
    eprintln!(
        "{name}: printer `{}` paper {:?} sheets {} device {:?} dpi, render {} dpi, \
         bands drawn {} blank {}, peak band {} KiB, sent {:.1} MiB, failures {}",
        report.printer,
        report.paper,
        report.sheets,
        report.device_dpi,
        report.render_dpi,
        report.bands_drawn,
        report.bands_blank,
        report.peak_band_bytes / 1024,
        report.bytes_sent as f64 / (1024.0 * 1024.0),
        report.failed_pages.len()
    );
}

/// Prints every page of `fixture` and checks the PDF the printer produced.
fn print_fixture(test: &str, fixture_path: &str) {
    let Some(printer) = pdf_printer(test) else {
        return;
    };
    let Some(doc) = fixture(fixture_path) else {
        return;
    };
    let output = output_path(test);
    let pages = doc.page_count();
    let mut progress = Vec::new();
    let report = print(
        &doc,
        &pdf_job(&printer, &output),
        &CancelToken::new(),
        |p| progress.push(p),
    )
    .expect("print");
    summary(test, &report);
    assert_eq!(report.sheets, pages);
    assert!(report.failed_pages.is_empty(), "{:?}", report.failed_pages);
    assert!(report.render_dpi <= 600);
    assert!(report.peak_band_bytes > 0 && report.peak_band_bytes <= max_band_bytes());
    assert!(report.bands_drawn > 0);
    assert_eq!(report.output_file.as_deref(), Some(output.as_path()));
    check_progress(&progress, pages);

    let printed = open_pdf(wait_for_pdf(&output, SPOOL_TIMEOUT));
    assert_eq!(printed.page_count(), pages);
    let paper = report.paper;
    for page in 0..pages {
        let sheet = printed.page_info(PageIndex::new(page)).unwrap().size;
        assert!((sheet.width - paper.width_pt).abs() < 1.0);
        assert!((sheet.height - paper.height_pt).abs() < 1.0);
        let source = render(&doc, page, 0.5);
        let copy = render(&printed, page, 0.5);
        let (source_ink, copy_ink) = (ink_ratio(&source), ink_ratio(&copy));
        eprintln!("{test}: page {page} ink {source_ink:.4} -> printed {copy_ink:.4}");
        assert!(copy_ink > 0.001, "printed page {page} is blank");
        // About the same amount of ink: the printer stores the bands as JPEG,
        // which lightens thin strokes, hence the loose bounds.
        assert!(
            copy_ink > source_ink * 0.3 && copy_ink < source_ink * 3.0,
            "page {page}: ink {source_ink} printed as {copy_ink}"
        );
        // The ink sits where shrink-to-fit puts the page: scaled to the
        // printable area (never enlarged) and centered in it.
        let info = doc.page_info(PageIndex::new(page)).unwrap();
        let size = info.display_size(Rotation::R0);
        assert!(size.width <= size.height, "fixture page is not portrait");
        let [px, py, pw, ph] = paper.printable_pt;
        let s = (pw / size.width).min(ph / size.height).min(1.0);
        let x = px + (pw - size.width * s) / 2.0;
        let y = py + (ph - size.height * s) / 2.0;
        let (sx0, sy0, sx1, sy1) = ink_box(&source).unwrap();
        let expected = [
            (x + sx0 as f32 * size.width * s) / paper.width_pt,
            (y + sy0 as f32 * size.height * s) / paper.height_pt,
            (x + sx1 as f32 * size.width * s) / paper.width_pt,
            (y + sy1 as f32 * size.height * s) / paper.height_pt,
        ];
        let (cx0, cy0, cx1, cy1) = ink_box(&copy).unwrap();
        let actual = [cx0 as f32, cy0 as f32, cx1 as f32, cy1 as f32];
        for (e, a) in expected.iter().zip(actual) {
            assert!(
                (e - a).abs() < 0.015,
                "page {page}: ink box {actual:?}, expected {expected:?}"
            );
        }
    }
}

#[test]
fn prints_small_text_fixture() {
    print_fixture(
        "prints_small_text_fixture",
        "small-text/three-pages-platypus-times.pdf",
    );
}

#[test]
fn prints_traditional_chinese_fixture() {
    print_fixture(
        "prints_traditional_chinese_fixture",
        "traditional-chinese/gov-letter-embedded-ttfsubset.pdf",
    );
}

/// Letter pages with a black bar at the top, middle or bottom, and a
/// landscape page with a bar along its top edge.
fn bars_pdf() -> Vec<u8> {
    pages_pdf(&[
        (612.0, 792.0, "0 0 0 rg 72 684 468 36 re f"),
        (612.0, 792.0, "0 0 0 rg 72 378 468 36 re f"),
        (612.0, 792.0, "0 0 0 rg 72 72 468 36 re f"),
        (792.0, 612.0, "0 0 0 rg 72 504 648 72 re f"),
    ])
}

/// Which bar page a printed portrait page shows, from the ink's top edge.
fn bar_position(doc: &fastpdf_engine_api::GuardedDocument, page: u32) -> &'static str {
    let (_, y0, _, _) = ink_box(&render(doc, page, 0.25)).expect("printed page has ink");
    match y0 {
        y if y < 0.33 => "top",
        y if y < 0.66 => "middle",
        _ => "bottom",
    }
}

#[test]
fn page_range_and_copies_print_in_order() {
    let Some(printer) = pdf_printer("page_range_and_copies_print_in_order") else {
        return;
    };
    let doc = open_pdf(bars_pdf());
    let output = output_path("range-and-copies");
    let job = PrintJob {
        page_range: PageRange::parse("3, 1").unwrap(),
        copies: 2,
        ..pdf_job(&printer, &output)
    };
    let mut progress = Vec::new();
    let report = print(&doc, &job, &CancelToken::new(), |p| progress.push(p)).expect("print");
    summary("range-and-copies", &report);
    assert_eq!(report.sheets, 4);
    check_progress(&progress, 4);
    let order: Vec<(u32, u32)> = progress
        .iter()
        .filter(|p| p.bands_done == 0)
        .map(|p| (p.page.get(), p.copy))
        .collect();
    assert_eq!(order, vec![(2, 1), (0, 1), (2, 2), (0, 2)]);
    // White bands are skipped: a bar page sends only a few bands.
    assert!(report.bands_blank > report.bands_drawn);

    let printed = open_pdf(wait_for_pdf(&output, SPOOL_TIMEOUT));
    assert_eq!(printed.page_count(), 4);
    let positions: Vec<&str> = (0..4).map(|p| bar_position(&printed, p)).collect();
    assert_eq!(positions, ["bottom", "top", "bottom", "top"]);
}

#[test]
fn landscape_pages_turn_counter_clockwise_on_portrait_paper() {
    let Some(printer) = pdf_printer("landscape_pages_turn_counter_clockwise_on_portrait_paper")
    else {
        return;
    };
    let doc = open_pdf(bars_pdf());
    let landscape = PageRange::parse("4").unwrap();
    let rotated_out = output_path("landscape-auto-rotate");
    let job = PrintJob {
        page_range: landscape.clone(),
        ..pdf_job(&printer, &rotated_out)
    };
    print(&doc, &job, &CancelToken::new(), |_| {}).expect("print rotated");
    let upright_out = output_path("landscape-no-rotate");
    let job = PrintJob {
        page_range: landscape,
        auto_rotate: false,
        ..pdf_job(&printer, &upright_out)
    };
    print(&doc, &job, &CancelToken::new(), |_| {}).expect("print upright");

    let rotated = open_pdf(wait_for_pdf(&rotated_out, SPOOL_TIMEOUT));
    let upright = open_pdf(wait_for_pdf(&upright_out, SPOOL_TIMEOUT));
    let paper = rotated.page_info(PageIndex::FIRST).unwrap().size;
    if paper.width > paper.height {
        eprintln!("SKIPPED: the PDF printer's default paper is landscape");
        return;
    }
    // Turned counter-clockwise, the page's top edge runs down the left side
    // of the sheet: a tall bar near the left edge.
    let (x0, y0, x1, y1) = ink_box(&render(&rotated, 0, 0.25)).expect("ink");
    eprintln!("auto-rotated bar at x {x0:.2}..{x1:.2}, y {y0:.2}..{y1:.2}");
    assert!(x1 < 0.3 && y0 < 0.2 && y1 > 0.8, "bar not on the left");
    // Without auto-rotation the page is shrunk upright: a wide bar on top.
    let (x0, y0, x1, y1) = ink_box(&render(&upright, 0, 0.25)).expect("ink");
    eprintln!("upright bar at x {x0:.2}..{x1:.2}, y {y0:.2}..{y1:.2}");
    assert!(x0 < 0.2 && x1 > 0.8 && y1 < 0.5, "bar not on top");
}

#[test]
fn cancelled_jobs_never_start() {
    let Some(printer) = pdf_printer("cancelled_jobs_never_start") else {
        return;
    };
    let doc = open_pdf(bars_pdf());
    let output = output_path("cancelled-before-start");
    let cancel = CancelToken::new();
    cancel.cancel();
    let mut calls = 0;
    let result = print(&doc, &pdf_job(&printer, &output), &cancel, |_| calls += 1);
    assert_eq!(result, Err(PrintError::Cancelled));
    assert_eq!(calls, 0);
    std::thread::sleep(Duration::from_secs(2));
    assert!(!output.exists(), "a cancelled job produced output");
}

#[test]
fn cancelling_between_bands_aborts_the_job() {
    let Some(printer) = pdf_printer("cancelling_between_bands_aborts_the_job") else {
        return;
    };
    let Some(doc) = fixture("small-text/three-pages-platypus-times.pdf") else {
        return;
    };
    let output = output_path("cancelled-mid-job");
    let cancel = CancelToken::new();
    let mut after_cancel = 0;
    let result = print(&doc, &pdf_job(&printer, &output), &cancel, |p| {
        if cancel.is_cancelled() {
            after_cancel += 1;
        } else if p.sheet == 2 && p.bands_done == 2 {
            cancel.cancel();
        }
    });
    assert_eq!(result, Err(PrintError::Cancelled));
    // Checked before the next band: no further progress after cancelling.
    assert_eq!(after_cancel, 0);
    // AbortDoc deletes the spool job, so no (complete) PDF appears.
    std::thread::sleep(Duration::from_secs(5));
    let complete = std::fs::read(&output)
        .map(|b| b.trim_ascii_end().ends_with(b"%%EOF"))
        .unwrap_or(false);
    assert!(!complete, "an aborted job produced a complete PDF");
}

/// Test engine with four Letter pages (zero-based): page 0 renders gray,
/// page 1 renders its first band and then panics, page 2 has no geometry,
/// page 3 fails to render.
struct FlakyEngine;
struct FlakyDoc;

impl PdfEngine for FlakyEngine {
    fn info(&self) -> EngineInfo {
        EngineInfo {
            name: "flaky",
            version: "0",
            capabilities: EngineCapabilities::default(),
        }
    }

    fn open(
        &self,
        _source: DocumentSource,
        _options: &OpenOptions,
    ) -> Result<Box<dyn EngineDocument>, EngineError> {
        Ok(Box::new(FlakyDoc))
    }
}

impl EngineDocument for FlakyDoc {
    fn page_count(&self) -> u32 {
        4
    }

    fn page_info(&self, page: PageIndex) -> Result<PageInfo, EngineError> {
        if page.get() == 2 {
            return Err(EngineError::Malformed("page tree entry is broken".into()));
        }
        Ok(PageInfo {
            size: PageSize::LETTER,
            rotation: Rotation::R0,
        })
    }

    fn render(
        &self,
        request: &RenderRequest,
        target: &mut PixmapMut<'_>,
        _cancel: &CancelToken,
    ) -> Result<RenderOutcome, EngineError> {
        match request.page.get() {
            1 if request.region.y > 0 => panic!("renderer bug on page 2"),
            3 => return Err(EngineError::Malformed("broken content stream".into())),
            _ => {}
        }
        target.fill(Rgba8::new(90, 90, 90, 255));
        Ok(RenderOutcome::default())
    }
}

#[test]
fn pages_that_fail_to_render_print_blank_without_stopping_the_job() {
    let Some(printer) =
        pdf_printer("pages_that_fail_to_render_print_blank_without_stopping_the_job")
    else {
        return;
    };
    let doc = open_guarded(
        &FlakyEngine,
        DocumentSource::from_bytes(SharedBytes::from_vec(Vec::new())),
        &OpenOptions::default(),
    )
    .unwrap();
    let output = output_path("failed-pages");
    // A low resolution keeps the solid gray pages small in the spool file.
    let job = PrintJob {
        max_dpi: 150,
        ..pdf_job(&printer, &output)
    };
    let mut progress = Vec::new();
    let report = print(&doc, &job, &CancelToken::new(), |p| progress.push(p)).expect("print");
    summary("failed-pages", &report);
    assert_eq!(report.sheets, 4);
    check_progress(&progress, 4);
    let failed: Vec<u32> = report.failed_pages.iter().map(|f| f.page.get()).collect();
    assert_eq!(failed, vec![1, 2, 3]);
    assert!(matches!(
        report.failed_pages[0].error,
        EngineError::Panicked(_)
    ));
    assert!(matches!(
        report.failed_pages[1].error,
        EngineError::Malformed(_)
    ));

    let printed = open_pdf(wait_for_pdf(&output, SPOOL_TIMEOUT));
    assert_eq!(printed.page_count(), 4);
    assert!(
        ink_ratio(&render(&printed, 0, 0.25)) > 0.5,
        "page 1 missing"
    );
    for page in 1..4 {
        // Page 2's first band was drawn before the panic; it is erased.
        // (Checked at full size: at small scales a viewer may leave a faint
        // antialiasing seam on the partial pixel at the very page edge.)
        assert_eq!(
            ink_box(&render(&printed, page, 1.0)),
            None,
            "page {} is not blank",
            page + 1
        );
    }
}

#[test]
fn invalid_jobs_are_rejected_before_printing() {
    let doc = open_pdf(bars_pdf());
    let output = output_path("never-written");
    // A queue name that cannot exist: CreateDCW fails, nothing is printed.
    let missing = "FastPDF test printer that does not exist";
    let job = pdf_job(missing, &output);
    assert_eq!(
        print(&doc, &job, &CancelToken::new(), |_| {}),
        Err(PrintError::PrinterNotFound(missing.into()))
    );
    let rejected = [
        PrintJob {
            copies: 0,
            ..job.clone()
        },
        PrintJob {
            page_range: PageRange::parse("9-12").unwrap(),
            ..job.clone()
        },
        PrintJob {
            output_file: Some(output.join("missing-directory").join("out.pdf")),
            ..job.clone()
        },
        PrintJob {
            max_dpi: 0,
            ..job.clone()
        },
    ];
    for job in &rejected {
        let result = print(&doc, job, &CancelToken::new(), |_| {});
        assert!(
            matches!(result, Err(PrintError::InvalidJob(_))),
            "{job:?}: {result:?}"
        );
    }
    assert!(!output.exists());
}

#[test]
fn printers_are_listed_read_only() {
    let printers = list_printers();
    for p in &printers {
        eprintln!(
            "printer `{}` driver `{}` port `{}` virtual {} default {}",
            p.name, p.driver, p.port, p.is_virtual, p.is_default
        );
    }
    if let Some(name) = default_printer() {
        let default: Vec<_> = printers.iter().filter(|p| p.is_default).collect();
        assert_eq!(default.len(), 1);
        assert_eq!(default[0].name, name);
    }
    if let Some(pdf) = printers.iter().find(|p| p.name == PDF_PRINTER) {
        assert!(pdf.is_virtual);
    }
    // Fit mode default matches the print dialog's usual default.
    assert_eq!(PrintJob::default().fit, FitMode::ShrinkToFit);
}
