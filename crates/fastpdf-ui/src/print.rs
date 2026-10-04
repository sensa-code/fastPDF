//! Printing (Ctrl+P, spec §8 and §33): a small panel over the document —
//! printer, pages, copies, scaling — that runs `fastpdf_print::print` on a
//! background thread, shows its progress, can cancel it and reports the
//! result, including pages that could not be rendered (printed blank).
//!
//! No system print dialog is shown. For development and tests,
//! `ReaderOptions::print_to_file` makes every job write into a file instead
//! of reaching the printer (the app sets it from `FASTPDF_PRINT_TO_FILE`).

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};

use fastpdf_engine_api::{CancelToken, PageIndex};
use fastpdf_print::{
    FitMode, PageRange, PrintError, PrintJob, PrintProgress, PrintReport, PrinterInfo,
};
use gpui::{
    AppContext, ClickEvent, Context, Div, Entity, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, Subscription, Task, Window, div, prelude::FluentBuilder,
    px, relative,
};

use crate::reader::ReaderView;
use crate::text_input::{TextChanged, TextInput};
use crate::theme::Theme;
use crate::toolbar::styled_button;

/// Key context of the panel's page-range field: the editing keys, but none
/// of the find bar's bindings (Enter must not search).
const RANGE_FIELD_CONTEXT: &str = "TextInput PrintPanel";
const MAX_COPIES: u32 = 99;
/// Panel width in logical pixels.
const WIDTH: f32 = 440.0;
/// Failed pages listed by number in the result; the rest are counted.
const LISTED_FAILURES: usize = 12;

/// Which pages to print.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum PageChoice {
    #[default]
    All,
    Current,
    Custom,
}

/// The installed printers, read on a background thread (enumeration can
/// be slow when a network printer does not answer).
pub(crate) enum Printers {
    NotLoaded,
    Loading(#[allow(dead_code)] Task<()>),
    Ready(Vec<PrinterInfo>),
}

/// Messages from the print thread.
enum JobEvent {
    Progress(PrintProgress),
    Finished(Result<PrintReport, PrintError>),
}

/// A job running on the print thread.
pub(crate) struct RunningJob {
    cancel: CancelToken,
    rx: Receiver<JobEvent>,
    pub progress: Option<PrintProgress>,
    printer: String,
    cancelling: bool,
}

/// How the last job ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Outcome {
    Done(String),
    /// Printed, but these pages failed to render and came out blank.
    Partial(String),
    Failed(String),
    Cancelled,
}

/// The print panel and its job.
pub(crate) struct PrintPanel {
    pub open: bool,
    pub printers: Printers,
    /// Chosen printer queue.
    pub selected: Option<String>,
    /// The printer list is unfolded.
    pub list_open: bool,
    pub pages: PageChoice,
    pub range: Entity<TextInput>,
    pub copies: u32,
    pub fit: FitMode,
    pub job: Option<RunningJob>,
    pub outcome: Option<Outcome>,
    /// A setting that keeps the job from starting (shown in the panel).
    pub problem: Option<String>,
    /// Development override: every job writes into this file.
    pub to_file: Option<PathBuf>,
    _range_changes: Subscription,
}

impl std::fmt::Debug for PrintPanel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PrintPanel")
            .field("open", &self.open)
            .field("selected", &self.selected)
            .field("pages", &self.pages)
            .field("copies", &self.copies)
            .field("printing", &self.job.is_some())
            .field("outcome", &self.outcome)
            .finish_non_exhaustive()
    }
}

impl PrintPanel {
    pub(crate) fn new(to_file: Option<PathBuf>, cx: &mut Context<'_, ReaderView>) -> Self {
        let range = cx.new(|cx| TextInput::with_context("e.g. 1-3, 5", RANGE_FIELD_CONTEXT, cx));
        // Typing a range selects "Custom" and clears an old range error.
        let subscription = cx.subscribe(&range, |view: &mut ReaderView, _, _: &TextChanged, cx| {
            view.print.pages = PageChoice::Custom;
            view.print.problem = None;
            cx.notify();
        });
        Self {
            open: false,
            printers: Printers::NotLoaded,
            selected: None,
            list_open: false,
            pages: PageChoice::All,
            range,
            copies: 1,
            fit: FitMode::ShrinkToFit,
            job: None,
            outcome: None,
            problem: None,
            to_file,
            _range_changes: subscription,
        }
    }

    pub(crate) fn printing(&self) -> bool {
        self.job.is_some()
    }

    /// Applies events from the print thread. Returns true when something
    /// changed.
    pub(crate) fn drain(&mut self) -> bool {
        let Some(job) = self.job.as_mut() else {
            return false;
        };
        let mut changed = false;
        let mut finished = None;
        while let Ok(event) = job.rx.try_recv() {
            changed = true;
            match event {
                JobEvent::Progress(p) => job.progress = Some(p),
                JobEvent::Finished(result) => finished = Some(result),
            }
        }
        if let Some(result) = finished {
            let printer = job.printer.clone();
            self.job = None;
            self.outcome = Some(outcome(&printer, self.to_file.as_ref(), result));
        }
        changed
    }

    /// Asks the running job to stop; the spool job is aborted (`AbortDoc`).
    pub(crate) fn cancel_job(&mut self) {
        if let Some(job) = self.job.as_mut() {
            job.cancel.cancel();
            job.cancelling = true;
        }
    }

    /// The pages to print, or why they cannot be printed.
    fn page_range(&self, current: PageIndex, text: &str) -> Result<PageRange, String> {
        match self.pages {
            PageChoice::All => Ok(PageRange::All),
            PageChoice::Current => {
                PageRange::parse(&current.display_number().to_string()).map_err(|e| e.to_string())
            }
            PageChoice::Custom => PageRange::parse(text).map_err(|e| e.to_string()),
        }
    }
}

/// The message for a finished job.
fn outcome(
    printer: &str,
    to_file: Option<&PathBuf>,
    result: Result<PrintReport, PrintError>,
) -> Outcome {
    let report = match result {
        Ok(report) => report,
        Err(PrintError::Cancelled) => return Outcome::Cancelled,
        Err(e) => return Outcome::Failed(format!("Printing failed: {e}")),
    };
    let sheets = match report.sheets {
        1 => "1 sheet".to_string(),
        n => format!("{n} sheets"),
    };
    let target = match to_file {
        Some(path) => format!("{printer} (into {})", path.display()),
        None => printer.to_string(),
    };
    if report.failed_pages.is_empty() {
        return Outcome::Done(format!("Sent {sheets} to {target}."));
    }
    let mut pages: Vec<u32> = report
        .failed_pages
        .iter()
        .map(|f| f.page.display_number())
        .collect();
    pages.sort_unstable();
    pages.dedup();
    let mut listed = pages
        .iter()
        .take(LISTED_FAILURES)
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    if pages.len() > LISTED_FAILURES {
        listed.push_str(&format!(" and {} more", pages.len() - LISTED_FAILURES));
    }
    Outcome::Partial(format!(
        "Sent {sheets} to {target}. Could not render (printed blank): page {listed}."
    ))
}

impl ReaderView {
    /// Ctrl+P: opens the panel, or closes it when no job is running.
    pub(crate) fn toggle_print_panel(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if self.print.open && !self.print.printing() {
            self.close_print_panel(window, cx);
            return;
        }
        self.print.open = true;
        self.print.list_open = false;
        if !self.print.printing() {
            self.print.outcome = None;
            self.print.problem = None;
        }
        self.load_printers(cx);
        cx.notify();
    }

    pub(crate) fn close_print_panel(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if self.print.printing() {
            return;
        }
        self.print.open = false;
        self.print.list_open = false;
        // The range field may have had the focus; give it back to the reader.
        window.focus(&self.focus, cx);
        cx.notify();
    }

    /// Reads the printer list in the background (every time the panel
    /// opens, so new printers show up) and preselects the default one.
    fn load_printers(&mut self, cx: &mut Context<'_, Self>) {
        if matches!(self.print.printers, Printers::Loading(_)) {
            return;
        }
        let read = cx.background_executor().spawn(async move {
            let printers = fastpdf_print::list_printers();
            let default = fastpdf_print::default_printer();
            (printers, default)
        });
        let task = cx.spawn(async move |this, cx| {
            let (printers, default) = read.await;
            let _ = this.update(cx, |this, cx| {
                let panel = &mut this.print;
                let still_there = |name: &String| printers.iter().any(|p| &p.name == name);
                if !panel.selected.as_ref().is_some_and(still_there) {
                    panel.selected = default
                        .filter(still_there)
                        .or_else(|| printers.first().map(|p| p.name.clone()));
                }
                log::debug!(
                    "printers: {:?}, selected {:?}",
                    printers.iter().map(|p| &p.name).collect::<Vec<_>>(),
                    panel.selected
                );
                panel.printers = Printers::Ready(printers);
                cx.notify();
            });
        });
        self.print.printers = Printers::Loading(task);
    }

    /// Starts printing with the panel's settings.
    pub(crate) fn start_print(&mut self, cx: &mut Context<'_, Self>) {
        if self.print.printing() {
            return;
        }
        let Some(session) = self.session() else {
            self.print.problem = Some("Open a document to print.".into());
            cx.notify();
            return;
        };
        let doc = std::sync::Arc::clone(session.document());
        let current = session.current_page();
        let Some(printer) = self.print.selected.clone() else {
            self.print.problem = Some("No printer is available.".into());
            cx.notify();
            return;
        };
        let text = self.print.range.read(cx).text().to_string();
        let page_range = match self.print.page_range(current, &text) {
            Ok(range) => range,
            Err(message) => {
                self.print.problem = Some(message);
                cx.notify();
                return;
            }
        };
        let document_name = match &self.doc {
            crate::reader::DocState::Open(open) => open.name.to_string(),
            _ => "FastPDF document".to_string(),
        };
        let job = PrintJob {
            printer_name: Some(printer.clone()),
            output_file: self.print.to_file.clone(),
            page_range,
            copies: self.print.copies,
            fit: self.print.fit,
            document_name,
            ..PrintJob::default()
        };
        log::info!(
            "printing {} on {printer}: pages {:?}, {} copies, {:?}{}",
            job.document_name,
            job.page_range,
            job.copies,
            job.fit,
            job.output_file
                .as_ref()
                .map_or(String::new(), |p| format!(", into {}", p.display()))
        );
        let cancel = CancelToken::new();
        let (tx, rx) = mpsc::channel();
        let waker = self.waker.clone();
        let thread_cancel = cancel.clone();
        let spawned = std::thread::Builder::new()
            .name("fastpdf-print".into())
            .spawn(move || {
                let progress_tx = tx.clone();
                let progress_waker = waker.clone();
                let result = fastpdf_print::print(&doc, &job, &thread_cancel, |p| {
                    if progress_tx.send(JobEvent::Progress(p)).is_ok() {
                        progress_waker.wake();
                    }
                });
                match &result {
                    Ok(report) => log::info!(
                        "printed {} sheets ({} failed pages, {} MiB sent)",
                        report.sheets,
                        report.failed_pages.len(),
                        report.bytes_sent / (1024 * 1024)
                    ),
                    Err(e) => log::warn!("printing stopped: {e}"),
                }
                // The document may be closed meanwhile; the job's reference
                // is released here, on this thread.
                drop(doc);
                let _ = tx.send(JobEvent::Finished(result));
                waker.wake();
            });
        match spawned {
            Ok(_) => {
                self.print.problem = None;
                self.print.outcome = None;
                self.print.list_open = false;
                self.print.job = Some(RunningJob {
                    cancel,
                    rx,
                    progress: None,
                    printer,
                    cancelling: false,
                });
            }
            Err(e) => {
                self.print.outcome = Some(Outcome::Failed(format!(
                    "Printing failed: cannot start the print thread ({e})"
                )));
            }
        }
        cx.notify();
    }

    /// The print panel, floating over the top of the document.
    pub(crate) fn render_print_panel(
        &self,
        cx: &mut Context<'_, Self>,
    ) -> impl IntoElement + use<> {
        let theme = self.theme;
        let panel = &self.print;
        let printing = panel.printing();
        let editable = !printing;
        let has_doc = self.session().is_some();
        let current = self.session().map(|s| s.current_page().display_number());

        let printer_label = match (&panel.printers, &panel.selected) {
            (_, Some(name)) => name.clone(),
            (Printers::Ready(list), None) if list.is_empty() => "No printers installed".into(),
            _ => "Looking for printers\u{2026}".into(),
        };
        let mut body = div()
            .flex()
            .flex_col()
            .gap_2()
            .child(div().text_size(px(15.0)).child("Print"))
            .child(
                row("Printer").child(
                    styled_button(
                        "print-printer",
                        format!("{printer_label}  \u{25be}"),
                        editable,
                        panel.list_open,
                        &theme,
                    )
                    .flex_1()
                    .justify_start()
                    .overflow_hidden()
                    .when(editable, |b| {
                        b.on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.print.list_open = !this.print.list_open;
                            cx.notify();
                        }))
                    }),
                ),
            );
        if panel.list_open
            && let Printers::Ready(list) = &panel.printers
        {
            body = body.child(self.render_printer_list(list, cx));
        }
        body = body
            .child(
                row("Pages")
                    .child(self.page_choice("print-all", "All", PageChoice::All, cx))
                    .child(self.page_choice(
                        "print-current",
                        &current.map_or("Current".into(), |n| format!("Current ({n})")),
                        PageChoice::Current,
                        cx,
                    ))
                    .child(self.page_choice("print-custom", "Custom", PageChoice::Custom, cx))
                    .child(
                        div()
                            .flex_1()
                            .h(px(28.0))
                            .px_2()
                            .rounded(px(4.0))
                            .bg(theme.input_bg)
                            .border_1()
                            .border_color(if panel.pages == PageChoice::Custom {
                                theme.accent
                            } else {
                                theme.toolbar_border
                            })
                            .text_size(px(14.0))
                            .child(panel.range.clone()),
                    ),
            )
            .child(
                row("Copies")
                    .child(
                        styled_button(
                            "print-fewer",
                            "\u{2212}",
                            editable && panel.copies > 1,
                            false,
                            &theme,
                        )
                        .on_click(cx.listener(
                            |this, _: &ClickEvent, _, cx| {
                                if !this.print.printing() {
                                    this.print.copies = this.print.copies.saturating_sub(1).max(1);
                                    cx.notify();
                                }
                            },
                        )),
                    )
                    .child(
                        div()
                            .min_w(px(28.0))
                            .flex()
                            .justify_center()
                            .child(panel.copies.to_string()),
                    )
                    .child(
                        styled_button(
                            "print-more",
                            "+",
                            editable && panel.copies < MAX_COPIES,
                            false,
                            &theme,
                        )
                        .on_click(cx.listener(
                            |this, _: &ClickEvent, _, cx| {
                                if !this.print.printing() {
                                    this.print.copies = (this.print.copies + 1).min(MAX_COPIES);
                                    cx.notify();
                                }
                            },
                        )),
                    ),
            )
            .child(
                row("Size")
                    .child(self.fit_choice("print-fit", "Shrink to fit", FitMode::ShrinkToFit, cx))
                    .child(self.fit_choice("print-actual", "Actual size", FitMode::ActualSize, cx)),
            );
        if let Some(path) = &panel.to_file {
            body = body.child(
                div()
                    .text_size(px(12.0))
                    .text_color(theme.text_muted)
                    .child(format!("Development: output goes into {}", path.display())),
            );
        }
        body = body.child(self.render_print_status(&theme));
        let can_print = has_doc && panel.selected.is_some() && !printing;
        body = body.child(
            div()
                .flex()
                .flex_row()
                .justify_end()
                .gap_1()
                .child(
                    styled_button("print-start", "Print", can_print, false, &theme)
                        .when(can_print, |b| b.bg(theme.button_active))
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.start_print(cx);
                        })),
                )
                .child(
                    styled_button(
                        "print-cancel",
                        if printing { "Cancel printing" } else { "Close" },
                        true,
                        false,
                        &theme,
                    )
                    .on_click(cx.listener(
                        |this, _: &ClickEvent, window, cx| {
                            if this.print.printing() {
                                this.print.cancel_job();
                                cx.notify();
                            } else {
                                this.close_print_panel(window, cx);
                            }
                        },
                    )),
                ),
        );

        div()
            .absolute()
            .top(px(8.0))
            .left_0()
            .right_0()
            .flex()
            .justify_center()
            .child(
                div()
                    .id("print-panel")
                    .occlude()
                    .w(px(WIDTH))
                    .p_3()
                    .rounded(px(6.0))
                    .bg(theme.toolbar_bg)
                    .border_1()
                    .border_color(theme.toolbar_border)
                    .text_size(px(13.0))
                    .child(body),
            )
    }

    fn render_printer_list(&self, list: &[PrinterInfo], cx: &mut Context<'_, Self>) -> Div {
        let theme = self.theme;
        let rows = list.iter().enumerate().map(|(i, printer)| {
            let name = printer.name.clone();
            let mut label = printer.name.clone();
            if printer.is_default {
                label.push_str("  (default)");
            }
            let selected = self.print.selected.as_deref() == Some(printer.name.as_str());
            styled_button(format!("print-printer-{i}"), label, true, selected, &theme)
                .justify_start()
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.print.selected = Some(name.clone());
                    this.print.list_open = false;
                    cx.notify();
                }))
        });
        div()
            .flex()
            .flex_col()
            .ml(px(LABEL_WIDTH))
            .p_1()
            .rounded(px(4.0))
            .border_1()
            .border_color(theme.toolbar_border)
            .bg(theme.input_bg)
            .children(rows)
            .when(list.is_empty(), |d| {
                d.child(
                    div()
                        .p_1()
                        .text_color(theme.text_muted)
                        .child("No printers installed"),
                )
            })
    }

    fn page_choice(
        &self,
        id: &'static str,
        label: &str,
        choice: PageChoice,
        cx: &mut Context<'_, Self>,
    ) -> impl IntoElement + use<> {
        let editable = !self.print.printing();
        styled_button(
            id,
            label.to_string(),
            editable,
            self.print.pages == choice,
            &self.theme,
        )
        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
            if this.print.printing() {
                return;
            }
            this.print.pages = choice;
            this.print.problem = None;
            if choice == PageChoice::Custom {
                let range = this.print.range.clone();
                range.update(cx, |input, cx| {
                    input.select_everything(cx);
                    window.focus(input.focus_handle(), cx);
                });
            }
            cx.notify();
        }))
    }

    fn fit_choice(
        &self,
        id: &'static str,
        label: &'static str,
        fit: FitMode,
        cx: &mut Context<'_, Self>,
    ) -> impl IntoElement + use<> {
        let editable = !self.print.printing();
        styled_button(id, label, editable, self.print.fit == fit, &self.theme).on_click(
            cx.listener(move |this, _: &ClickEvent, _, cx| {
                if !this.print.printing() {
                    this.print.fit = fit;
                    cx.notify();
                }
            }),
        )
    }

    /// Progress of the running job, or the result of the last one.
    fn render_print_status(&self, theme: &Theme) -> Div {
        let panel = &self.print;
        let line = |text: String, color| div().min_h(px(18.0)).text_color(color).child(text);
        if let Some(job) = &panel.job {
            let (text, fraction) = match job.progress {
                _ if job.cancelling => (
                    "Cancelling\u{2026}".to_string(),
                    progress_fraction(job.progress),
                ),
                Some(p) => (
                    format!(
                        "Printing page {} (sheet {} of {})\u{2026}",
                        p.page.display_number(),
                        p.sheet,
                        p.sheets
                    ),
                    progress_fraction(Some(p)),
                ),
                None => ("Starting the print job\u{2026}".to_string(), 0.0),
            };
            return div()
                .flex()
                .flex_col()
                .gap_1()
                .child(line(text, theme.text))
                .child(
                    div()
                        .h(px(6.0))
                        .w_full()
                        .rounded(px(3.0))
                        .bg(theme.button_hover)
                        .child(
                            div()
                                .h_full()
                                .w(relative(fraction))
                                .rounded(px(3.0))
                                .bg(theme.accent),
                        ),
                );
        }
        if let Some(problem) = &panel.problem {
            return line(problem.clone(), theme.error_text);
        }
        match &panel.outcome {
            Some(Outcome::Done(text)) => line(text.clone(), theme.text),
            Some(Outcome::Partial(text) | Outcome::Failed(text)) => {
                line(text.clone(), theme.error_text)
            }
            Some(Outcome::Cancelled) => line("Printing cancelled.".into(), theme.text_muted),
            None => line(String::new(), theme.text_muted),
        }
    }
}

/// Width of the label column of a panel row.
const LABEL_WIDTH: f32 = 64.0;

fn row(label: &'static str) -> Div {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap_1()
        .child(div().w(px(LABEL_WIDTH)).flex_none().child(label))
}

/// Share of the job done: whole sheets plus the bands of the current one.
fn progress_fraction(progress: Option<PrintProgress>) -> f32 {
    let Some(p) = progress else {
        return 0.0;
    };
    if p.sheets == 0 {
        return 0.0;
    }
    let within = if p.bands == 0 {
        0.0
    } else {
        p.bands_done as f32 / p.bands as f32
    };
    ((p.sheet.saturating_sub(1) as f32 + within) / p.sheets as f32).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fastpdf_engine_api::EngineError;
    use fastpdf_print::{PageFailure, PaperInfo};

    fn report(sheets: u32, failed: &[u32]) -> PrintReport {
        PrintReport {
            printer: "P".into(),
            output_file: None,
            paper: PaperInfo {
                width_pt: 595.0,
                height_pt: 842.0,
                printable_pt: [0.0, 0.0, 595.0, 842.0],
            },
            sheets,
            failed_pages: failed
                .iter()
                .map(|&p| PageFailure {
                    page: PageIndex::new(p - 1),
                    copy: 1,
                    error: EngineError::Malformed("x".into()),
                })
                .collect(),
            partial_pages: Vec::new(),
            device_dpi: (600, 600),
            render_dpi: 600,
            band_rows: 256,
            peak_band_bytes: 0,
            bands_drawn: 0,
            bands_blank: 0,
            bytes_sent: 0,
        }
    }

    #[test]
    fn outcomes_name_the_failed_pages_once() {
        assert_eq!(
            outcome("P", None, Ok(report(3, &[]))),
            Outcome::Done("Sent 3 sheets to P.".into())
        );
        let partial = outcome("P", None, Ok(report(6, &[7, 2, 7])));
        assert_eq!(
            partial,
            Outcome::Partial(
                "Sent 6 sheets to P. Could not render (printed blank): page 2, 7.".into()
            )
        );
        let many: Vec<u32> = (1..=20).collect();
        let Outcome::Partial(text) = outcome("P", None, Ok(report(20, &many))) else {
            panic!("partial expected");
        };
        assert!(text.ends_with("11, 12 and 8 more."), "{text}");
        assert_eq!(
            outcome("P", None, Err(PrintError::Cancelled)),
            Outcome::Cancelled
        );
        assert!(matches!(
            outcome("P", None, Err(PrintError::NoDefaultPrinter)),
            Outcome::Failed(_)
        ));
        let to_file = PathBuf::from(r"C:\out\x.pdf");
        assert_eq!(
            outcome("P", Some(&to_file), Ok(report(1, &[]))),
            Outcome::Done(r"Sent 1 sheet to P (into C:\out\x.pdf).".into())
        );
    }

    #[test]
    fn progress_counts_sheets_and_bands() {
        assert_eq!(progress_fraction(None), 0.0);
        let p = |sheet, sheets, bands_done, bands| PrintProgress {
            sheet,
            sheets,
            page: PageIndex::FIRST,
            copy: 1,
            bands_done,
            bands,
        };
        assert_eq!(progress_fraction(Some(p(1, 4, 0, 10))), 0.0);
        assert_eq!(progress_fraction(Some(p(2, 4, 5, 10))), 0.375);
        assert_eq!(progress_fraction(Some(p(4, 4, 10, 10))), 1.0);
        assert_eq!(progress_fraction(Some(p(1, 2, 0, 0))), 0.0);
    }
}
