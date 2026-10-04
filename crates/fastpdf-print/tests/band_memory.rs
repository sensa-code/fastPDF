//! Banding keeps memory bounded: printing an A0 page at 600 dpi never
//! allocates a page- or sheet-sized bitmap. This is its own test binary
//! because it installs a counting global allocator and measures the process.
//!
//! Like every test here it prints only to "Microsoft Print to PDF" with an
//! output file under the target directory (see `common`).

#![cfg(windows)]
// The counting allocator and the process memory query need unsafe code; each
// block states why it is sound.
#![allow(unsafe_code)]

mod common;

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use common::*;
use fastpdf_engine_api::{CancelToken, EngineDocument, PageIndex, Rotation};
use fastpdf_print::{BAND_ROWS, FitMode, MAX_BAND_WIDTH, PrintJob, print};
use windows_sys::Win32::System::ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
use windows_sys::Win32::System::Threading::GetCurrentProcess;

/// Live and peak bytes allocated through the Rust allocator.
static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

struct Counting;

fn grew(bytes: usize) {
    let live = LIVE.fetch_add(bytes, Ordering::Relaxed) + bytes;
    PEAK.fetch_max(live, Ordering::Relaxed);
}

fn shrank(bytes: usize) {
    LIVE.fetch_sub(bytes, Ordering::Relaxed);
}

// SAFETY: every method forwards to the system allocator with the caller's
// arguments unchanged, so `System`'s guarantees carry over; the counters are
// bookkeeping on the side and never touch the memory.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller upholds `GlobalAlloc::alloc`'s contract.
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            grew(layout.size());
        }
        ptr
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller upholds `GlobalAlloc::alloc_zeroed`'s contract.
        let ptr = unsafe { System.alloc_zeroed(layout) };
        if !ptr.is_null() {
            grew(layout.size());
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: `ptr` was allocated by this allocator (i.e. by `System`)
        // with `layout`, as the caller guarantees.
        unsafe { System.dealloc(ptr, layout) };
        shrank(layout.size());
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: the caller upholds `GlobalAlloc::realloc`'s contract.
        let new = unsafe { System.realloc(ptr, layout, new_size) };
        if !new.is_null() {
            // Counted as alloc-then-free: both blocks may exist at once.
            grew(new_size);
            shrank(layout.size());
        }
        new
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// Restarts peak tracking; returns the bytes live right now.
fn reset_peak() -> usize {
    let live = LIVE.load(Ordering::Relaxed);
    PEAK.store(live, Ordering::Relaxed);
    live
}

/// (current, peak) private bytes of the whole process, including memory
/// that GDI and the printer driver allocate outside the Rust allocator.
fn process_private_bytes() -> (usize, usize) {
    let mut counters = PROCESS_MEMORY_COUNTERS {
        cb: size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
        ..Default::default()
    };
    // SAFETY: the pseudo handle of the current process needs no closing,
    // and `counters` is a writable PROCESS_MEMORY_COUNTERS whose `cb` holds
    // its size.
    let ok = unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, counters.cb) };
    assert_ne!(ok, 0, "K32GetProcessMemoryInfo failed");
    (counters.PagefileUsage, counters.PeakPagefileUsage)
}

const MIB: usize = 1024 * 1024;

#[test]
fn a0_pages_print_with_bounded_memory() {
    let Some(printer) = pdf_printer("a0_pages_print_with_bounded_memory") else {
        return;
    };
    let Some(doc) = fixture("cad/a0-floorplan-layers.pdf") else {
        return;
    };
    let size = doc
        .page_info(PageIndex::FIRST)
        .unwrap()
        .display_size(Rotation::R0);
    assert!(
        size.width.min(size.height) > 2000.0,
        "not an A0 page: {size:?}"
    );
    let page_bitmap = |dpi: f32| {
        (size.width / 72.0 * dpi).ceil() as usize * (size.height / 72.0 * dpi).ceil() as usize * 4
    };
    eprintln!(
        "A0 page {size:?}: a 600 dpi bitmap would take {} MiB",
        page_bitmap(600.0) / MIB
    );
    // Let the engine parse the page first: its display list is cached for
    // the document's lifetime and is not part of the per-band budget.
    render(&doc, 0, 0.05);

    for (fit, name) in [
        (FitMode::ActualSize, "a0-actual-size"),
        (FitMode::ShrinkToFit, "a0-shrink-to-fit"),
    ] {
        let output = output_path(name);
        let job = PrintJob {
            fit,
            ..pdf_job(&printer, &output)
        };
        let (process_before, _) = process_private_bytes();
        let base = reset_peak();
        let report = print(&doc, &job, &CancelToken::new(), |_| {}).expect("print");
        let heap_peak = PEAK.load(Ordering::Relaxed) - base;
        let (_, process_peak) = process_private_bytes();
        // The whole printable area as one bitmap at the render resolution.
        let dpi = report.render_dpi as f32;
        let [_, _, pw, ph] = report.paper.printable_pt;
        let sheet_bitmap =
            (pw / 72.0 * dpi).ceil() as usize * (ph / 72.0 * dpi).ceil() as usize * 4;
        eprintln!(
            "{name}: render {dpi} dpi, bands {} drawn + {} blank, band buffer {} KiB, \
             sent {:.1} MiB, heap peak +{:.1} MiB (sheet bitmap {} MiB), process private peak +{:.1} MiB",
            report.bands_drawn,
            report.bands_blank,
            report.peak_band_bytes / 1024,
            report.bytes_sent as f64 / MIB as f64,
            heap_peak as f64 / MIB as f64,
            sheet_bitmap / MIB,
            process_peak.saturating_sub(process_before) as f64 / MIB as f64,
        );
        assert_eq!(report.sheets, 1);
        assert!(report.failed_pages.is_empty(), "{:?}", report.failed_pages);
        assert!(report.peak_band_bytes <= BAND_ROWS as usize * MAX_BAND_WIDTH as usize * 4);
        // Everything the job allocated at once (band buffer, the engine's
        // band raster, spool bookkeeping) stays far below one sheet bitmap,
        // let alone the A0 page.
        assert!(
            heap_peak < sheet_bitmap / 4 && heap_peak < 64 * MIB,
            "{name}: heap grew by {heap_peak} bytes (sheet bitmap {sheet_bitmap})"
        );

        let printed = open_pdf(wait_for_pdf(&output, Duration::from_secs(180)));
        assert_eq!(printed.page_count(), 1);
        assert!(
            ink_ratio(&render(&printed, 0, 0.25)) > 0.001,
            "{name}: blank"
        );
    }
}
