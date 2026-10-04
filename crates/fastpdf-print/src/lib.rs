//! Headless printing for FastPDF (spec §8 Print, §33 Windows Print).
//!
//! GPUI has no printing API, so pages go to the printer through Win32 GDI:
//! `CreateDCW` → `StartDocW` → per sheet `StartPage`, the page rendered by
//! the engine in horizontal **bands** of at most [`BAND_ROWS`] x
//! [`MAX_BAND_WIDTH`] pixels (one reused buffer of at most 8 MiB, so an A0
//! page at 600 dpi never needs a page-sized bitmap), each band cropped to its
//! inked part, packed as 8-bit gray or 24-bit color and sent with
//! `StretchDIBits` → `EndPage` → `EndDoc`. Every GDI handle is owned by an
//! RAII guard, so errors, panics and cancellation delete the DC and abort
//! the spool job.
//!
//! All rendering goes through [`GuardedDocument`], so engine panics stay
//! contained: a page that fails to render prints blank and is listed in
//! [`PrintReport::failed_pages`] instead of stopping the job (spec §24).
//!
//! Jobs use the printer's current defaults (paper, orientation, duplex,
//! color): this crate never changes printer settings and passes no DEVMODE.
//!
//! Not in this crate (yet): the print dialog. The UI is expected to show
//! `PrintDlgEx` (or its own dialog), then fill a [`PrintJob`] from the result
//! (a future version may also take the dialog's DEVMODE for paper, duplex
//! and color) and call [`print()`] on a worker thread with a [`CancelToken`]
//! wired to its cancel button and the progress callback to its progress bar.
//!
//! Other platforms: [`print()`] returns [`PrintError::Unsupported`] and the
//! printer queries return nothing.

mod job;
#[cfg_attr(not(windows), allow(dead_code))]
mod layout;
#[cfg_attr(not(windows), allow(dead_code))]
mod raster;
#[cfg(windows)]
mod win;

pub use fastpdf_engine_api::{CancelToken, GuardedDocument};
pub use job::{
    FitMode, PageFailure, PageRange, PaperInfo, PrintError, PrintJob, PrintProgress, PrintReport,
    PrinterInfo,
};
pub use layout::{BAND_ROWS, MAX_BAND_WIDTH};

/// Installed printers and printer connections (empty when printing is not
/// supported). Read-only. It asks print servers about their queues, so an
/// unreachable network printer can make it slow: call it off the UI thread.
pub fn list_printers() -> Vec<PrinterInfo> {
    #[cfg(windows)]
    {
        win::printers()
    }
    #[cfg(not(windows))]
    {
        Vec::new()
    }
}

/// Name of the user's default printer.
pub fn default_printer() -> Option<String> {
    #[cfg(windows)]
    {
        win::default_printer()
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// Prints `doc` as described by `job`, blocking until the job is spooled
/// (the spooler then prints it, or writes `job.output_file`, asynchronously).
///
/// Call it from a worker thread. `cancel` is checked before the job starts,
/// between sheets, between bands and inside rendering; a cancelled job is
/// aborted in the spooler (`AbortDoc`) and returns [`PrintError::Cancelled`].
/// `progress` is called when a sheet starts and after each of its bands.
///
/// A queue that writes files (such as "Microsoft Print to PDF") asks for a
/// file name in a dialog unless `job.output_file` is set.
pub fn print(
    doc: &GuardedDocument,
    job: &PrintJob,
    cancel: &CancelToken,
    mut progress: impl FnMut(PrintProgress),
) -> Result<PrintReport, PrintError> {
    #[cfg(windows)]
    {
        win::print(doc, job, cancel, &mut progress)
    }
    #[cfg(not(windows))]
    {
        let _ = (doc, job, cancel, &mut progress);
        Err(PrintError::Unsupported(
            "printing is implemented for Windows only".into(),
        ))
    }
}
