//! Print job description, progress, report and errors.

use std::fmt;
use std::ops::RangeInclusive;
use std::path::PathBuf;

use fastpdf_engine_api::{EngineError, PageIndex};

/// An installed printer (or print queue connection).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrinterInfo {
    /// Queue name, as passed to [`PrintJob::printer_name`].
    pub name: String,
    pub is_default: bool,
    /// Heuristic: the queue produces a file or a document instead of paper
    /// ("Microsoft Print to PDF", XPS writer, OneNote, fax, `FILE:` ports...).
    pub is_virtual: bool,
    pub driver: String,
    pub port: String,
}

/// How a page is sized on the paper.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FitMode {
    /// Shrink pages larger than the printable area to fit it; never enlarge.
    #[default]
    ShrinkToFit,
    /// Print at 100 %, centered on the sheet; whatever exceeds the printable
    /// area is cut off.
    ActualSize,
}

/// Which pages to print, in the order given.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum PageRange {
    #[default]
    All,
    /// One-based, inclusive page numbers as a user types them (`1-3, 5`).
    Pages(Vec<RangeInclusive<u32>>),
}

impl PageRange {
    /// Parses a user range such as `"1-3, 5, 8-"` (an open end runs to the
    /// last page). Page numbers are one-based.
    pub fn parse(spec: &str) -> Result<Self, PrintError> {
        let invalid = |part: &str| PrintError::InvalidJob(format!("invalid page range `{part}`"));
        let mut ranges = Vec::new();
        for part in spec.split(',').map(str::trim).filter(|p| !p.is_empty()) {
            let number = |s: &str| -> Result<u32, PrintError> {
                match s.trim().parse::<u32>() {
                    Ok(n) if n >= 1 => Ok(n),
                    _ => Err(invalid(part)),
                }
            };
            let range = match part.split_once('-') {
                Some((first, "")) => number(first)?..=u32::MAX,
                Some((first, last)) => number(first)?..=number(last)?,
                None => {
                    let n = number(part)?;
                    n..=n
                }
            };
            if range.start() > range.end() {
                return Err(invalid(part));
            }
            ranges.push(range);
        }
        if ranges.is_empty() {
            return Err(PrintError::InvalidJob("empty page range".into()));
        }
        Ok(Self::Pages(ranges))
    }

    /// The zero-based pages to print in order. Parts beyond the document end
    /// are clipped; an empty result is an error.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub(crate) fn resolve(&self, page_count: u32) -> Result<Vec<PageIndex>, PrintError> {
        let pages: Vec<PageIndex> = match self {
            Self::All => (0..page_count).map(PageIndex::new).collect(),
            Self::Pages(ranges) => ranges
                .iter()
                .flat_map(|r| {
                    let first = (*r.start()).max(1);
                    let last = (*r.end()).min(page_count);
                    (first..=last).map(|n| PageIndex::new(n - 1))
                })
                .collect(),
        };
        if pages.is_empty() {
            return Err(PrintError::InvalidJob(format!(
                "page range selects none of the document's {page_count} pages"
            )));
        }
        Ok(pages)
    }
}

/// One print job.
///
/// The print dialog (`PrintDlgEx`) is not part of this crate yet: the UI will
/// collect these settings (printer, range, copies, fit, orientation) and fill
/// a `PrintJob`, so the dialog stays a thin layer over this API.
#[derive(Debug, Clone, PartialEq)]
pub struct PrintJob {
    /// Printer queue name; `None` prints to the default printer.
    pub printer_name: Option<String>,
    /// Print into this file instead of sending the job to the device
    /// (`DOCINFOW::lpszOutput`). Required for file-producing queues such as
    /// "Microsoft Print to PDF" to avoid their save-as prompt.
    pub output_file: Option<PathBuf>,
    pub page_range: PageRange,
    /// Collated copies, produced by sending the pages again (`1..`).
    pub copies: u32,
    pub fit: FitMode,
    /// Turn pages whose orientation differs from the paper's by 90° (counter-
    /// clockwise, the usual landscape convention).
    pub auto_rotate: bool,
    /// Upper bound on the render resolution. Printers with a higher DPI get
    /// the image stretched by the device, which keeps memory and spool size
    /// bounded.
    pub max_dpi: u32,
    /// Name shown in the print queue.
    pub document_name: String,
}

impl Default for PrintJob {
    fn default() -> Self {
        Self {
            printer_name: None,
            output_file: None,
            page_range: PageRange::All,
            copies: 1,
            fit: FitMode::ShrinkToFit,
            auto_rotate: true,
            max_dpi: 600,
            document_name: "FastPDF document".into(),
        }
    }
}

/// Progress of a running job, reported after every band.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PrintProgress {
    /// One-based sheet being printed and the job's total (pages x copies).
    pub sheet: u32,
    pub sheets: u32,
    /// Document page on that sheet and its copy number (one-based).
    pub page: PageIndex,
    pub copy: u32,
    /// Bands of this sheet finished so far, out of `bands`.
    pub bands_done: u32,
    pub bands: u32,
}

/// A page that could not be rendered; its sheet was printed blank (bands
/// drawn before the failure are painted over with white).
#[derive(Debug, Clone, PartialEq)]
pub struct PageFailure {
    pub page: PageIndex,
    pub copy: u32,
    pub error: EngineError,
}

/// The sheet the printer uses (its current default paper), in points.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PaperInfo {
    pub width_pt: f32,
    pub height_pt: f32,
    /// Printable area within the sheet: x, y, width, height.
    pub printable_pt: [f32; 4],
}

/// Summary of a finished job.
#[derive(Debug, Clone, PartialEq)]
pub struct PrintReport {
    pub printer: String,
    pub output_file: Option<PathBuf>,
    pub paper: PaperInfo,
    /// Sheets sent to the spooler (pages x copies), failed ones included.
    pub sheets: u32,
    pub failed_pages: Vec<PageFailure>,
    /// Pages the engine rendered only partially (content skipped because of
    /// a budget or a broken object); they were printed as rendered.
    pub partial_pages: Vec<PageIndex>,
    /// Device resolution (dots per inch, x and y).
    pub device_dpi: (u32, u32),
    /// Resolution pages were rendered at (before fit scaling).
    pub render_dpi: u32,
    /// Rows per band and the largest band buffer used, in bytes.
    pub band_rows: u32,
    pub peak_band_bytes: usize,
    /// Bands sent to the device (cropped to their inked part) and bands
    /// skipped because they were entirely paper-white.
    pub bands_drawn: u64,
    pub bands_blank: u64,
    /// Bitmap bytes sent to the device: inked parts only, as 8-bit gray or
    /// 24-bit color.
    pub bytes_sent: u64,
}

/// Why a job did not complete.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum PrintError {
    /// Printing is not implemented on this platform.
    Unsupported(String),
    PrinterNotFound(String),
    NoDefaultPrinter,
    InvalidJob(String),
    /// The job was cancelled; nothing was printed (the spool job is aborted).
    Cancelled,
    /// A Win32 call failed; `code` is `GetLastError()`.
    Win32 {
        call: &'static str,
        code: u32,
    },
    /// The document could not be inspected at all (e.g. no page geometry for
    /// any page); single-page render failures are reported in [`PrintReport`].
    Engine(EngineError),
}

impl fmt::Display for PrintError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported(m) => write!(f, "printing unsupported: {m}"),
            Self::PrinterNotFound(name) => write!(f, "printer `{name}` not found"),
            Self::NoDefaultPrinter => f.write_str("no default printer"),
            Self::InvalidJob(m) => write!(f, "invalid print job: {m}"),
            Self::Cancelled => f.write_str("print job cancelled"),
            Self::Win32 { call, code } => write!(f, "{call} failed (error {code})"),
            Self::Engine(e) => write!(f, "document error: {e}"),
        }
    }
}

impl std::error::Error for PrintError {}

impl PrintJob {
    #[cfg_attr(not(windows), allow(dead_code))]
    pub(crate) fn validate(&self) -> Result<(), PrintError> {
        if self.copies == 0 || self.copies > 999 {
            return Err(PrintError::InvalidJob(format!(
                "copies must be 1..=999, got {}",
                self.copies
            )));
        }
        if !(72..=4800).contains(&self.max_dpi) {
            return Err(PrintError::InvalidJob(format!(
                "max_dpi must be 72..=4800, got {}",
                self.max_dpi
            )));
        }
        if self.document_name.contains('\0')
            || self
                .printer_name
                .as_deref()
                .is_some_and(|n| n.contains('\0'))
        {
            return Err(PrintError::InvalidJob("names must not contain NUL".into()));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_ranges_parse_like_print_dialogs() {
        assert_eq!(
            PageRange::parse("1-3, 5,8-").unwrap(),
            PageRange::Pages(vec![1..=3, 5..=5, 8..=u32::MAX])
        );
        for bad in ["", "0", "3-1", "a", "1-x", "-2"] {
            assert!(PageRange::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn ranges_resolve_in_order_and_clip_to_the_document() {
        let r = PageRange::parse("4-6, 2, 9-").unwrap();
        let pages: Vec<u32> = r.resolve(5).unwrap().iter().map(|p| p.get()).collect();
        assert_eq!(pages, vec![3, 4, 1]);
        assert_eq!(PageRange::All.resolve(3).unwrap().len(), 3);
        assert!(PageRange::parse("7-9").unwrap().resolve(5).is_err());
    }

    #[test]
    fn jobs_are_validated() {
        assert!(PrintJob::default().validate().is_ok());
        let zero = PrintJob {
            copies: 0,
            ..PrintJob::default()
        };
        assert!(zero.validate().is_err());
        let dpi = PrintJob {
            max_dpi: 10,
            ..PrintJob::default()
        };
        assert!(dpi.validate().is_err());
    }
}
