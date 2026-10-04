//! Windows printing: a GDI print job whose pages are rendered band by band.

mod gdi;
mod spool;

use std::path::PathBuf;

use fastpdf_engine_api::{
    CancelToken, EngineDocument, EngineError, GuardedDocument, PageIndex, PixelFormat, PixelRect,
    PixmapMut, RenderRequest,
};

pub(crate) use spool::{default_printer, printers};

use crate::job::{PageFailure, PrintError, PrintJob, PrintProgress, PrintReport};
use crate::layout::{self, BAND_ROWS, DeviceRect, MAX_BAND_WIDTH, Paper};
use crate::raster;

/// Where a sheet sits in the job, for progress reports.
#[derive(Clone, Copy)]
struct Sheet {
    number: u32,
    total: u32,
    page: PageIndex,
    copy: u32,
}

/// Mutable state shared by all sheets of one job.
struct JobState<'p> {
    report: PrintReport,
    /// One band buffer, reused for every band of the job.
    band: Vec<u8>,
    progress: &'p mut dyn FnMut(PrintProgress),
}

impl JobState<'_> {
    fn report_progress(&mut self, sheet: Sheet, bands_done: u32, bands: u32) {
        (self.progress)(PrintProgress {
            sheet: sheet.number,
            sheets: sheet.total,
            page: sheet.page,
            copy: sheet.copy,
            bands_done,
            bands,
        });
    }

    fn fail(&mut self, sheet: Sheet, error: EngineError) {
        self.report.failed_pages.push(PageFailure {
            page: sheet.page,
            copy: sheet.copy,
            error,
        });
    }

    /// The band buffer resized to `bytes`. Bands shrink after the first one
    /// of a page, so the buffer is allocated once at its exact size.
    fn band_buffer(&mut self, bytes: usize) -> &mut [u8] {
        if self.band.len() < bytes {
            self.band.reserve_exact(bytes - self.band.len());
            self.band.resize(bytes, 0);
        }
        self.report.peak_band_bytes = self.report.peak_band_bytes.max(self.band.capacity());
        &mut self.band[..bytes]
    }
}

pub(crate) fn print(
    doc: &GuardedDocument,
    job: &PrintJob,
    cancel: &CancelToken,
    progress: &mut dyn FnMut(PrintProgress),
) -> Result<PrintReport, PrintError> {
    job.validate()?;
    let pages = job.page_range.resolve(doc.page_count())?;
    let total = u32::try_from(pages.len())
        .ok()
        .and_then(|n| n.checked_mul(job.copies))
        .ok_or_else(|| PrintError::InvalidJob("too many sheets".into()))?;
    let output = job.output_file.as_deref().map(output_path).transpose()?;
    if cancel.is_cancelled() {
        return Err(PrintError::Cancelled);
    }
    let printer = match &job.printer_name {
        Some(name) => name.clone(),
        None => default_printer().ok_or(PrintError::NoDefaultPrinter)?,
    };

    let dc = gdi::PrinterDc::open(&printer)?;
    let paper = dc.paper()?;
    let mut state = JobState {
        report: PrintReport {
            printer,
            output_file: output.clone(),
            paper: paper.info(),
            sheets: 0,
            failed_pages: Vec::new(),
            partial_pages: Vec::new(),
            device_dpi: (paper.dpi_x, paper.dpi_y),
            render_dpi: layout::render_dpi(&paper, job.max_dpi),
            band_rows: BAND_ROWS,
            peak_band_bytes: 0,
            bands_drawn: 0,
            bands_blank: 0,
            bytes_sent: 0,
        },
        band: Vec::new(),
        progress,
    };
    // From here on the document guard aborts the spool job on every early
    // return (cancellation, GDI failure) and the DC guard deletes the DC.
    let mut document = dc.start_doc(&job.document_name, output.as_deref())?;
    let mut number = 0;
    for copy in 1..=job.copies {
        for &page in &pages {
            if cancel.is_cancelled() {
                return Err(PrintError::Cancelled);
            }
            number += 1;
            let sheet = Sheet {
                number,
                total,
                page,
                copy,
            };
            document.start_page()?;
            print_sheet(doc, job, cancel, &mut document, &paper, sheet, &mut state)?;
            document.end_page()?;
            state.report.sheets += 1;
        }
    }
    document.finish()?;
    Ok(state.report)
}

/// The spooler needs an absolute path, and it reports a bad one only
/// asynchronously (as a failed job), so check what can be checked now.
fn output_path(path: &std::path::Path) -> Result<PathBuf, PrintError> {
    let absolute = std::path::absolute(path)
        .map_err(|e| PrintError::InvalidJob(format!("output file {}: {e}", path.display())))?;
    if absolute.is_dir() || absolute.parent().is_none_or(|dir| !dir.is_dir()) {
        return Err(PrintError::InvalidJob(format!(
            "output file {} is not in an existing directory",
            absolute.display()
        )));
    }
    Ok(absolute)
}

/// Renders one sheet band by band. A page the engine cannot render leaves a
/// blank sheet and is recorded; only cancellation and GDI errors abort.
fn print_sheet(
    doc: &GuardedDocument,
    job: &PrintJob,
    cancel: &CancelToken,
    document: &mut gdi::Document<'_>,
    paper: &Paper,
    sheet: Sheet,
    state: &mut JobState<'_>,
) -> Result<(), PrintError> {
    let placed = doc.page_info(sheet.page).and_then(|info| {
        layout::place(&info, paper, job.fit, job.auto_rotate, job.max_dpi)
            .map(|placement| (info, placement))
            .map_err(|e| EngineError::InvalidRequest(e.to_string()))
    });
    let (info, placement) = match placed {
        Ok(placed) => placed,
        Err(error) => {
            state.fail(sheet, error);
            state.report_progress(sheet, 0, 0);
            return Ok(());
        }
    };
    let bands = layout::bands(&placement, BAND_ROWS, MAX_BAND_WIDTH);
    let count = u32::try_from(bands.len()).unwrap_or(u32::MAX);
    state.report_progress(sheet, 0, count);
    // Everything drawn on this sheet so far, to erase it if a band fails.
    let mut inked: Option<DeviceRect> = None;
    for (done, band) in (1u32..).zip(&bands) {
        if cancel.is_cancelled() {
            return Err(PrintError::Cancelled);
        }
        let size = band.source.size();
        let bytes = size.width as usize * size.height as usize * PixelFormat::BYTES_PER_PIXEL;
        let pixels = state.band_buffer(bytes);
        let mut target = PixmapMut::from_slice(pixels, size, PixelFormat::Bgra8Premultiplied)
            .map_err(PrintError::Engine)?;
        let request = RenderRequest::full_page(
            sheet.page,
            info.size,
            info.rotation,
            placement.rotation,
            placement.scale,
        )
        .with_region(band.source);
        match doc.render(&request, &mut target, cancel) {
            Ok(outcome) => {
                if outcome.partial && !state.report.partial_pages.contains(&sheet.page) {
                    state.report.partial_pages.push(sheet.page);
                }
            }
            Err(EngineError::Cancelled) => return Err(PrintError::Cancelled),
            Err(error) => {
                // spec §24: one broken page must not stop the job; its sheet
                // prints blank. Bands already drawn are painted over, using
                // the whole printable area (the sheet holds only this page)
                // so that in vector output such as Print to PDF no
                // antialiased edge of a band survives inside the page. Best
                // effort: a device that cannot erase keeps the partial page,
                // which is still reported as failed.
                if let Some(rect) = inked {
                    let _ = document.erase(rect.union(&paper.printable_rect()));
                }
                state.fail(sheet, error);
                state.report_progress(sheet, count, count);
                return Ok(());
            }
        }
        let pixels = &mut state.band[..bytes];
        match raster::ink_bounds(pixels, size) {
            // Paper is white already: nothing to send.
            None => state.report.bands_blank += 1,
            Some(ink) => {
                let source = PixelRect::new(
                    band.source.x + ink.x,
                    band.source.y + ink.y,
                    ink.width,
                    ink.height,
                );
                let dest = placement.device_rect(source);
                if dest.is_empty() {
                    // Shrunk to nothing on the device.
                    state.report.bands_blank += 1;
                } else {
                    let packed = raster::pack(pixels, size.width, ink);
                    let bits = &pixels[..packed.len];
                    document.draw_dib(dest, bits, ink.width, ink.height, packed)?;
                    inked = Some(inked.map_or(dest, |r| r.union(&dest)));
                    state.report.bands_drawn += 1;
                    state.report.bytes_sent += packed.len as u64;
                }
            }
        }
        state.report_progress(sheet, done, count);
    }
    Ok(())
}
