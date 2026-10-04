//! Development scripts: drive the reader from a step list instead of
//! synthesized input, to reproduce a scenario and take screenshots from
//! outside the process (spec §46 tooling). The app passes
//! `FASTPDF_DEV_SCRIPT` here only in debug builds or with the development
//! overlay enabled.
//!
//! A script is a `;`-separated list of steps, each `name` or `name=value`:
//!
//! | step | effect |
//! |---|---|
//! | `open=PATH` | opens a document (replacing the current one) |
//! | `wait-open` | waits for the document to open (stops if it fails) |
//! | `wait=MS` | sleeps MS milliseconds |
//! | `idle` | waits until no tile render is queued or running |
//! | `cmd=NAME` | runs a command by name (`ZoomIn`, `Print`, `ToggleDevOverlay`, `CloseDocument`, `Quit`, ...) |
//! | `find=TEXT` | opens the find bar and searches for TEXT |
//! | `wait-find` | waits until the search finished |
//! | `page=N` | goes to page N (one-based) |
//! | `zoom=PERCENT` | sets the zoom |
//! | `print-pages=all`, `=current`, `=1-3,5` | page choice of the print panel |
//! | `print-copies=N` | copies in the print panel |
//! | `wait-printers` | waits for the print panel's printer list |
//! | `print-printers` | unfolds (or folds) the panel's printer list |
//! | `print-printer=NAME` | selects that printer (stops if it is not installed) |
//! | `print` | presses Print in the print panel |
//! | `wait-print` | waits until the print job ended |
//! | `print-cancel` | cancels the running print job |
//! | `mark=PATH` | writes `PATH.mark`, then waits (60 s at most) for `PATH.ack` |
//! | `memory=LABEL` | logs the memory breakdown (info level) |
//!
//! Every waiting step gives up after 60 s and ends the script.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use fastpdf_core::keymap::ReaderCommand;
use fastpdf_engine_api::PageIndex;
use fastpdf_render::ZoomLevel;
use gpui::{Context, Window};

use crate::print::PageChoice;
use crate::reader::{DocState, ReaderView};

/// How often a waiting step checks its condition.
const POLL: Duration = Duration::from_millis(50);
/// Longest a step may wait.
const STEP_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Step {
    Open(PathBuf),
    WaitOpen,
    Wait(Duration),
    Idle,
    Command(ReaderCommand),
    Find(String),
    WaitFind,
    Page(u32),
    Zoom(f32),
    PrintPages(String),
    PrintCopies(u32),
    WaitPrinters,
    PrinterList,
    Printer(String),
    Print,
    WaitPrint,
    PrintCancel,
    Mark(PathBuf),
    Memory(String),
}

/// What a step reports after one poll.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum StepState {
    Done,
    Pending,
    Abort(String),
}

/// Parses a script; the error names the bad step.
pub(crate) fn parse(script: &str) -> Result<Vec<Step>, String> {
    script
        .split(';')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|step| parse_step(step).ok_or_else(|| format!("bad step `{step}`")))
        .collect()
}

fn parse_step(step: &str) -> Option<Step> {
    let (name, value) = match step.split_once('=') {
        Some((name, value)) => (name.trim(), Some(value.trim())),
        None => (step, None),
    };
    Some(match (name, value) {
        ("open", Some(path)) if !path.is_empty() => Step::Open(PathBuf::from(path)),
        ("wait-open", None) => Step::WaitOpen,
        ("wait", Some(ms)) => Step::Wait(Duration::from_millis(ms.parse().ok()?)),
        ("idle", None) => Step::Idle,
        ("cmd", Some(command)) => Step::Command(command_named(command)?),
        ("find", Some(text)) => Step::Find(text.to_string()),
        ("wait-find", None) => Step::WaitFind,
        ("page", Some(n)) => Step::Page(n.parse().ok().filter(|&n| n >= 1)?),
        ("zoom", Some(p)) => Step::Zoom(p.trim_end_matches('%').parse().ok()?),
        ("print-pages", Some(choice)) => Step::PrintPages(choice.to_string()),
        ("print-copies", Some(n)) => Step::PrintCopies(n.parse().ok().filter(|&n| n >= 1)?),
        ("wait-printers", None) => Step::WaitPrinters,
        ("print-printers", None) => Step::PrinterList,
        ("print-printer", Some(name)) if !name.is_empty() => Step::Printer(name.to_string()),
        ("print", None) => Step::Print,
        ("wait-print", None) => Step::WaitPrint,
        ("print-cancel", None) => Step::PrintCancel,
        ("mark", Some(path)) if !path.is_empty() => Step::Mark(PathBuf::from(path)),
        ("memory", label) => Step::Memory(label.unwrap_or_default().to_string()),
        _ => return None,
    })
}

/// The command called `name` (the `ReaderCommand` variant name).
fn command_named(name: &str) -> Option<ReaderCommand> {
    use ReaderCommand as C;
    const ALL: [ReaderCommand; 28] = [
        C::OpenFile,
        C::CloseDocument,
        C::Quit,
        C::Find,
        C::FindNext,
        C::FindPrevious,
        C::Print,
        C::ZoomIn,
        C::ZoomOut,
        C::ActualSize,
        C::FitPage,
        C::FitWidth,
        C::RotateClockwise,
        C::RotateCounterClockwise,
        C::NextPage,
        C::PreviousPage,
        C::FirstPage,
        C::LastPage,
        C::PageDown,
        C::PageUp,
        C::ScrollDown,
        C::ScrollUp,
        C::ToggleFullscreen,
        C::ToggleSidebar,
        C::ToggleDevOverlay,
        C::Copy,
        C::SelectAll,
        C::Cancel,
    ];
    ALL.into_iter()
        .find(|c| format!("{c:?}").eq_ignore_ascii_case(name))
}

fn path_with(path: &std::path::Path, extension: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(extension);
    PathBuf::from(name)
}

impl ReaderView {
    /// Runs `steps` one after another on the UI thread.
    pub(crate) fn start_dev_script(
        &mut self,
        steps: Vec<Step>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        log::info!("dev script: {} steps", steps.len());
        let task = cx.spawn_in(window, async move |this, cx| {
            for (index, step) in steps.into_iter().enumerate() {
                let started = Instant::now();
                let mut first = true;
                loop {
                    let state = match this.update_in(cx, |view, window, cx| {
                        view.dev_step(&step, first, started, window, cx)
                    }) {
                        Ok(state) => state,
                        Err(_) => return, // the window is gone
                    };
                    first = false;
                    match state {
                        StepState::Done => break,
                        StepState::Pending if started.elapsed() < STEP_TIMEOUT => {
                            cx.background_executor().timer(POLL).await;
                        }
                        StepState::Pending => {
                            log::warn!("dev script: step {} {step:?} timed out", index + 1);
                            return;
                        }
                        StepState::Abort(why) => {
                            log::warn!("dev script: step {} {step:?} failed: {why}", index + 1);
                            return;
                        }
                    }
                }
                log::info!(
                    "dev script: step {} {step:?} done after {:.0} ms",
                    index + 1,
                    started.elapsed().as_secs_f64() * 1000.0
                );
            }
            log::info!("dev script: finished");
        });
        self.dev_script = Some(task);
    }

    /// Polls one step; `first` is true on the first poll of the step.
    fn dev_step(
        &mut self,
        step: &Step,
        first: bool,
        started: Instant,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> StepState {
        use StepState::{Abort, Done, Pending};
        match step {
            Step::Open(path) => {
                self.open_path(path.clone(), window, cx);
                Done
            }
            Step::WaitOpen => match &self.doc {
                DocState::Open(_) => Done,
                DocState::Failed { message, .. } => Abort(message.clone()),
                _ => Pending,
            },
            Step::Wait(duration) => {
                if started.elapsed() >= *duration {
                    Done
                } else {
                    Pending
                }
            }
            Step::Idle => match self.session() {
                Some(session) => {
                    let q = session.stats().scheduler;
                    if q.queued == 0 && q.in_flight == 0 {
                        Done
                    } else {
                        Pending
                    }
                }
                None => Done,
            },
            Step::Command(command) => {
                self.run_command(*command, window, cx);
                Done
            }
            Step::Find(text) => {
                self.run_command(ReaderCommand::Find, window, cx);
                let input = self.find.input.clone();
                input.update(cx, |input, cx| input.set_text(text, cx));
                Done
            }
            Step::WaitFind => {
                if self.find.progress.finished || self.find.query.is_empty() {
                    Done
                } else {
                    Pending
                }
            }
            Step::Page(n) => match self.session_mut() {
                Some(session) => {
                    session.go_to_page(PageIndex::new(n - 1));
                    cx.notify();
                    Done
                }
                None => Abort("no document".into()),
            },
            Step::Zoom(percent) => match self.session_mut() {
                Some(session) => {
                    session.set_zoom(ZoomLevel::new(percent / 100.0), None);
                    cx.notify();
                    Done
                }
                None => Abort("no document".into()),
            },
            Step::PrintPages(choice) => {
                self.print.pages = match choice.as_str() {
                    "all" => PageChoice::All,
                    "current" => PageChoice::Current,
                    range => {
                        let input = self.print.range.clone();
                        input.update(cx, |input, cx| input.set_text(range, cx));
                        PageChoice::Custom
                    }
                };
                cx.notify();
                Done
            }
            Step::PrintCopies(n) => {
                self.print.copies = *n;
                cx.notify();
                Done
            }
            Step::WaitPrinters => {
                if matches!(self.print.printers, crate::print::Printers::Ready(_)) {
                    Done
                } else {
                    Pending
                }
            }
            Step::PrinterList => {
                self.print.list_open = !self.print.list_open;
                cx.notify();
                Done
            }
            Step::Printer(name) => match &self.print.printers {
                crate::print::Printers::Ready(list) if list.iter().any(|p| &p.name == name) => {
                    self.print.selected = Some(name.clone());
                    cx.notify();
                    Done
                }
                crate::print::Printers::Ready(_) => Abort(format!("no printer named {name}")),
                _ => Pending,
            },
            Step::Print => {
                self.start_print(cx);
                if self.print.printing() {
                    Done
                } else {
                    Abort(self.print.problem.clone().unwrap_or_else(|| {
                        format!("print did not start: {:?}", self.print.outcome)
                    }))
                }
            }
            Step::WaitPrint => {
                if self.print.printing() {
                    Pending
                } else {
                    log::info!("dev script: print outcome {:?}", self.print.outcome);
                    Done
                }
            }
            Step::PrintCancel => {
                self.print.cancel_job();
                cx.notify();
                Done
            }
            Step::Mark(path) => {
                if first {
                    // Paint the current state before announcing it.
                    cx.notify();
                    if let Err(e) = std::fs::write(path_with(path, ".mark"), b"") {
                        return Abort(format!("cannot write the mark: {e}"));
                    }
                    return Pending;
                }
                if path_with(path, ".ack").exists() {
                    Done
                } else {
                    Pending
                }
            }
            Step::Memory(label) => {
                for line in self.memory_breakdown().lines() {
                    log::info!("memory {label}: {line}");
                }
                Done
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scripts_parse_every_step() {
        let steps = parse(
            "wait-open; wait=500; idle; cmd=ZoomIn; cmd=toggledevoverlay; find=lorem ipsum; \
             wait-find; page=300; zoom=250%; print-pages=1-3, 5; print-copies=2; wait-printers; \
             print; wait-print; print-cancel; mark=C:\\shots\\a; memory=after search; memory; \
             print-printers; print-printer=Microsoft Print to PDF; open=D:\\x y.pdf",
        )
        .unwrap();
        assert_eq!(steps.len(), 21);
        assert_eq!(steps[20], Step::Open(PathBuf::from(r"D:\x y.pdf")));
        assert_eq!(steps[18], Step::PrinterList);
        assert_eq!(steps[19], Step::Printer("Microsoft Print to PDF".into()));
        assert_eq!(steps[1], Step::Wait(Duration::from_millis(500)));
        assert_eq!(steps[3], Step::Command(ReaderCommand::ZoomIn));
        assert_eq!(steps[4], Step::Command(ReaderCommand::ToggleDevOverlay));
        assert_eq!(steps[5], Step::Find("lorem ipsum".into()));
        assert_eq!(steps[7], Step::Page(300));
        assert_eq!(steps[8], Step::Zoom(250.0));
        assert_eq!(steps[9], Step::PrintPages("1-3, 5".into()));
        assert_eq!(steps[15], Step::Mark(PathBuf::from(r"C:\shots\a")));
        assert_eq!(steps[17], Step::Memory(String::new()));
    }

    #[test]
    fn bad_steps_are_reported() {
        for bad in ["wait=soon", "cmd=Explode", "page=0", "teleport", "mark="] {
            assert!(
                parse(bad)
                    .unwrap_err()
                    .contains(bad.split('=').next().unwrap())
            );
        }
        assert_eq!(parse(" ; ;").unwrap(), Vec::new());
    }

    #[test]
    fn marks_append_their_extension() {
        assert_eq!(
            path_with(std::path::Path::new(r"C:\s\shot.1"), ".ack"),
            PathBuf::from(r"C:\s\shot.1.ack")
        );
    }
}
