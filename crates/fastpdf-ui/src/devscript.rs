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
//! | `idle` | waits for a frame painted after the step started (it plans the tiles), then until no tile render is queued or running |
//! | `cmd=NAME` | runs a command by name (`ZoomIn`, `Print`, `LayoutTwoPage`, `ToggleDevOverlay`, `CloseDocument`, `Quit`, ...) |
//! | `find=TEXT` | opens the find bar and searches for TEXT |
//! | `wait-find` | waits until the search finished |
//! | `page=N` | goes to page N (one-based), a jump Back returns from |
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
//! | `appearance=system`, `=light`, `=dark` | sets the appearance setting |
//! | `night=on`, `=off`, `=toggle` | sets night mode (inverted pages) |
//! | `sidebar=off`, `=outline`, `=pages`, `=results` | closes the sidebar or shows a tab |
//! | `default-zoom=fit-width`, `=fit-page`, `=actual-size` | sets the default zoom |
//! | `settings` | shows (or hides) the settings panel |
//! | `layout-menu` | opens (or closes) the toolbar's page layout menu |
//! | `edit-menu` | opens (or closes) the toolbar's edit menu |
//! | `tool=extract`, `=delete`, `=rotate`, `=reorder`, `=split`, `=merge` | opens the page tools on that tool |
//! | `tool-pages=SPEC` | the page tools' page field |
//! | `tool-angle=90`, `=180`, `=270` | the rotation of the rotate tool |
//! | `merge-add=PATH` | appends a file to merge |
//! | `tool-save=PATH` | saves the page tools' result there (a folder for a split), without the dialog |
//! | `save-copy=PATH` | saves a copy there, without the dialog |
//! | `wait-write` | waits until the write finished |
//! | `stamp-add=PATH` | adds a stamp image (read right away) and opens the stamp panel |
//! | `stamp-place=PAGE,X,Y` | places the first stamp image centred on that point (one-based page, points from the top left) |
//! | `stamp-width=MM` | the width of stamps placed next |
//! | `stamp-save=PATH` | saves the stamped document there, without the dialog |
//! | `text-edit-panel` | opens (or closes) the text edit panel |
//! | `text-edit=PAGE\|FIND\|NEW` | on that page (one-based), the line containing FIND gets NEW in its place |
//! | `text-edit-begin=PAGE\|FIND` | starts editing the line containing FIND in place (the field over it) |
//! | `text-edit-font=N` | the font new text is drawn with (0: 標楷體, 1: 新細明體, 2: 微軟正黑體) |
//! | `text-edit-save=PATH` | saves the document with its changes there, without the dialog |
//! | `sign-panel` | opens (or closes) the signature panel |
//! | `sign-test` | signs with a key made for the job (needs the development overlay) |
//! | `sign-visible=on`, `=off` | whether a placed stamp is the signature's appearance |
//! | `sign-reason=TEXT` | the signature's reason |
//! | `sign-save=PATH` | signs the document there, without the dialog |
//! | `language=system`, `=en`, `=zh-TW` | sets the UI language setting |
//! | `smooth=on`, `=off` | turns smooth wheel scrolling on or off |
//! | `wheel=NOTCHES` | a mouse-wheel turn (negative: up), as the wheel sends it |
//! | `expect-page=N`, `expect-zoom=PERCENT` | stops the script unless on page N (one-based) or at that zoom (`checks.rs`) |
//! | `expect-history=B,F` | stops the script unless Back and Forward hold B and F views (`checks.rs`) |
//! | `expect-focus=find`, `=document`, `=prompt`, `=results` | stops the script unless the find field (or the reader itself, the password prompt's field, the search result list) has the keyboard focus (`checks.rs`) |
//! | `focus=document` | gives the keyboard focus to the reader itself, as a mouse press on the toolbar does (`checks.rs`) |
//! | `outline=N` | activates outline row N (one-based), once the outline has loaded (`checks.rs`) |
//! | `wait-prompt=password` | waits until the password prompt waits for input (`checks.rs`) |
//! | `password=TEXT` | types TEXT into the password prompt and confirms it; fixture passwords only, never logged (`checks.rs`) |
//! | `password-text=TEXT` | only types TEXT into the password prompt (to see the masked field) (`checks.rs`) |
//! | `password-compose=TEXT` | puts TEXT into the password prompt as an unfinished IME composition (`checks.rs`) |
//! | `prompt-cancel` | cancels the prompt, like Esc (`checks.rs`) |
//! | `wait-labels` | asks for the page labels and waits for the answer (`labels.rs`) |
//! | `expect-label=TEXT` | stops the script unless the current page is labeled TEXT (`labels.rs`) |
//! | `goto=TEXT` | types TEXT into the page field (a label, a number, `#N`) and presses Enter (`labels.rs`) |
//! | `find-case=on`, `=off`, `find-word=on`, `=off` | sets the find options match case and whole word, like their toggles (`search.rs`) |
//! | `find-result=N` | shows search result N (one-based) as a click on its row does; waits while the search runs, stops if it found fewer (`search.rs`) |
//!
//! Every waiting step gives up after 60 s and ends the script.

mod checks;
mod labels;
mod search;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use fastpdf_core::Jump;
use fastpdf_core::keymap::ReaderCommand;
use fastpdf_engine_api::PageIndex;
use fastpdf_render::ZoomLevel;
use gpui::{Context, Window};

use crate::print::PageChoice;
use crate::reader::{DocState, ReaderView};
use crate::settings::{Appearance, DefaultZoom, LanguageChoice, tab_from_name};
use crate::sidebar::SidebarTab;

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
    Appearance(Appearance),
    /// `None`: toggle.
    Night(Option<bool>),
    /// `None`: closed.
    Sidebar(Option<SidebarTab>),
    DefaultZoom(DefaultZoom),
    SettingsPanel,
    LayoutMenu,
    EditMenu,
    Tool(crate::tools::PageTool),
    ToolPages(String),
    ToolAngle(fastpdf_engine_api::Rotation),
    MergeAdd(PathBuf),
    ToolSave(PathBuf),
    SaveCopy(PathBuf),
    WaitWrite,
    StampAdd(PathBuf),
    StampPlace(u32, f32, f32),
    StampWidth(f32),
    StampSave(PathBuf),
    TextEditPanel,
    TextEdit(u32, String, String),
    TextEditBegin(u32, String),
    TextEditFont(usize),
    TextEditSave(PathBuf),
    SignPanel,
    SignTest,
    SignVisible(bool),
    SignReason(String),
    SignSave(PathBuf),
    Language(LanguageChoice),
    Smooth(bool),
    /// Wheel notches over the document (positive: down).
    Wheel(i32),
    Check(checks::Check),
    Labels(labels::LabelStep),
    Search(search::SearchStep),
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
        ("appearance", Some(name)) => Step::Appearance(Appearance::from_name(name)?),
        ("night", Some("on")) => Step::Night(Some(true)),
        ("night", Some("off")) => Step::Night(Some(false)),
        ("night", Some("toggle")) => Step::Night(None),
        ("sidebar", Some("off")) => Step::Sidebar(None),
        ("sidebar", Some("results")) => Step::Sidebar(Some(SidebarTab::Results)),
        ("sidebar", Some(tab)) => Step::Sidebar(Some(tab_from_name(tab)?)),
        ("default-zoom", Some(name)) => Step::DefaultZoom(DefaultZoom::from_name(name)?),
        ("settings", None) => Step::SettingsPanel,
        ("layout-menu", None) => Step::LayoutMenu,
        ("edit-menu", None) => Step::EditMenu,
        ("tool", Some(name)) => Step::Tool(crate::tools::PageTool::from_name(name)?),
        ("tool-pages", Some(spec)) => Step::ToolPages(spec.to_owned()),
        ("tool-angle", Some(degrees)) => Step::ToolAngle(
            degrees
                .trim()
                .parse()
                .ok()
                .and_then(fastpdf_engine_api::Rotation::from_degrees)?,
        ),
        ("merge-add", Some(path)) => Step::MergeAdd(PathBuf::from(path.trim())),
        ("tool-save", Some(path)) => Step::ToolSave(PathBuf::from(path.trim())),
        ("save-copy", Some(path)) => Step::SaveCopy(PathBuf::from(path.trim())),
        ("wait-write", None) => Step::WaitWrite,
        ("stamp-add", Some(path)) => Step::StampAdd(PathBuf::from(path.trim())),
        ("stamp-place", Some(spec)) => {
            let parts: Vec<&str> = spec.split(',').map(str::trim).collect();
            let [page, x, y] = parts[..] else {
                return None;
            };
            Step::StampPlace(page.parse().ok()?, x.parse().ok()?, y.parse().ok()?)
        }
        ("stamp-width", Some(mm)) => Step::StampWidth(mm.trim().parse().ok()?),
        ("stamp-save", Some(path)) => Step::StampSave(PathBuf::from(path.trim())),
        ("text-edit-panel", None) => Step::TextEditPanel,
        ("text-edit", Some(spec)) => {
            let parts: Vec<&str> = spec.splitn(3, '|').collect();
            let [page, find, new] = parts[..] else {
                return None;
            };
            let page = page.trim().parse().ok().filter(|&p| p >= 1)?;
            (!find.is_empty()).then_some(())?;
            Step::TextEdit(page, find.to_owned(), new.to_owned())
        }
        ("text-edit-begin", Some(spec)) => {
            let (page, find) = spec.split_once('|')?;
            let page = page.trim().parse().ok().filter(|&p| p >= 1)?;
            (!find.is_empty()).then_some(())?;
            Step::TextEditBegin(page, find.to_owned())
        }
        ("text-edit-font", Some(n)) => Step::TextEditFont(
            n.parse()
                .ok()
                .filter(|&n| n < crate::text_edits::EDIT_FONTS.len())?,
        ),
        ("text-edit-save", Some(path)) => Step::TextEditSave(PathBuf::from(path.trim())),
        ("sign-panel", None) => Step::SignPanel,
        ("sign-test", None) => Step::SignTest,
        ("sign-visible", Some("on")) => Step::SignVisible(true),
        ("sign-visible", Some("off")) => Step::SignVisible(false),
        ("sign-reason", Some(text)) => Step::SignReason(text.to_owned()),
        ("sign-save", Some(path)) => Step::SignSave(PathBuf::from(path.trim())),
        ("language", Some(name)) => Step::Language(LanguageChoice::from_name(name)?),
        ("smooth", Some("on")) => Step::Smooth(true),
        ("smooth", Some("off")) => Step::Smooth(false),
        ("wheel", Some(n)) => Step::Wheel(n.parse().ok().filter(|n: &i32| *n != 0)?),
        _ => match (
            labels::LabelStep::parse(name, value),
            search::SearchStep::parse(name, value),
        ) {
            (Some(step), _) => Step::Labels(step),
            (None, Some(step)) => Step::Search(step),
            (None, None) => Step::Check(checks::Check::parse(name, value)?),
        },
    })
}

/// The command called `name` (the `ReaderCommand` variant name).
fn command_named(name: &str) -> Option<ReaderCommand> {
    ReaderCommand::ALL
        .iter()
        .copied()
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
                DocState::Failed { failure, .. } => Abort(failure.to_string()),
                DocState::Locked(locked) if locked.checking => Pending,
                DocState::Locked(locked) if locked.wrong => {
                    Abort("the password did not open the document".into())
                }
                DocState::Locked(_) => Abort("the document needs a password".into()),
                _ => Pending,
            },
            Step::Wait(duration) => {
                if started.elapsed() >= *duration {
                    Done
                } else {
                    Pending
                }
            }
            // A command only asks for a frame; the frame schedules the tiles.
            Step::Idle if !self.render_started.is_some_and(|at| at > started) => {
                if first {
                    cx.notify();
                }
                Pending
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
            Step::Page(n) if self.session().is_some() => {
                self.jump(Jump::Direct, |s| s.go_to_page(PageIndex::new(n - 1)));
                cx.notify();
                Done
            }
            Step::Page(_) => Abort("no document".into()),
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
                    let english = crate::i18n::Language::English.strings();
                    Abort(match self.print.problem {
                        Some(problem) => problem.text(english).to_string(),
                        None => format!("print did not start: {:?}", self.print.outcome),
                    })
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
            Step::Appearance(appearance) => {
                self.set_appearance(*appearance, cx);
                Done
            }
            Step::Night(on) => {
                let on = on.unwrap_or(!self.settings.night_mode);
                self.set_night_mode(on, cx);
                Done
            }
            Step::Sidebar(tab) => {
                match tab {
                    Some(tab) => {
                        if !self.sidebar.open {
                            self.set_sidebar_open(true, window, cx);
                        }
                        self.set_sidebar_tab(*tab, window, cx);
                    }
                    None => self.set_sidebar_open(false, window, cx),
                }
                Done
            }
            Step::DefaultZoom(zoom) => {
                self.set_default_zoom(*zoom, cx);
                Done
            }
            Step::SettingsPanel => {
                self.toggle_settings_panel(cx);
                Done
            }
            Step::Tool(tool) => {
                if !self.tools.open {
                    self.toggle_tools_panel(window, cx);
                }
                self.tools.tool = *tool;
                cx.notify();
                Done
            }
            Step::ToolPages(spec) => {
                let field = self.tools.field.clone();
                field.update(cx, |input, cx| input.set_text(spec, cx));
                Done
            }
            Step::ToolAngle(angle) => {
                self.tools.angle = *angle;
                cx.notify();
                Done
            }
            Step::MergeAdd(path) => {
                self.tools.merge.push(path.clone());
                cx.notify();
                Done
            }
            Step::ToolSave(path) => {
                self.save_tool_result(Some(path.clone()), window, cx);
                Done
            }
            Step::SaveCopy(path) => {
                self.save_copy(Some(path.clone()), window, cx);
                Done
            }
            Step::WaitWrite => {
                if self.tools.writing() {
                    Pending
                } else {
                    Done
                }
            }
            Step::StampAdd(path) => {
                if !self.stamps.open {
                    self.toggle_stamp_panel(cx);
                }
                match crate::stamps::load_stamp_image(path) {
                    Ok(image) => self.dev_add_stamp(image),
                    Err(e) => return Abort(format!("stamp image {}: {e}", path.display())),
                }
                cx.notify();
                Done
            }
            Step::StampPlace(page, x, y) => {
                // The page must be laid out and known: show it first.
                let index = PageIndex::new(page.saturating_sub(1));
                if let Some(session) = self.session_mut() {
                    session.go_to_page(index);
                }
                let point = self.session().and_then(|s| {
                    let r =
                        s.page_to_view(index, fastpdf_engine_api::PageRect::new(*x, *y, *x, *y))?;
                    Some((r[0], r[1]))
                });
                let placed = point.is_some_and(|(vx, vy)| self.place_stamp(0, vx, vy));
                if !placed {
                    return Abort(format!("cannot place a stamp on page {page}"));
                }
                cx.notify();
                Done
            }
            Step::StampWidth(mm) => {
                self.stamps.width_mm = *mm;
                Done
            }
            Step::StampSave(path) | Step::TextEditSave(path) => {
                self.save_changes(Some(path.clone()), window, cx);
                Done
            }
            Step::TextEditPanel => {
                self.toggle_text_edit_panel(window, cx);
                Done
            }
            Step::TextEdit(page, find, new) => match self.dev_text_edit(*page, find, new, cx) {
                None => Pending,
                Some(Ok(())) => Done,
                Some(Err(why)) => Abort(why),
            },
            Step::TextEditBegin(page, find) => {
                let index = PageIndex::new(page.saturating_sub(1));
                if let Some(session) = self.session_mut() {
                    session.go_to_page(index);
                }
                let span = match self.selection.layer(index) {
                    Some(layer) => layer
                        .spans
                        .iter()
                        .find(|s| s.text.contains(find.as_str()))
                        .cloned(),
                    None => {
                        let missing: Vec<PageIndex> =
                            self.selection.want_layer(index).into_iter().collect();
                        self.fetch_text_layers(missing, cx);
                        return Pending;
                    }
                };
                let Some(span) = span else {
                    return Abort(format!("no line with {find:?} on page {page}"));
                };
                if !self.text_edits.open {
                    self.toggle_text_edit_panel(window, cx);
                }
                self.begin_line_edit(index, span, window, cx);
                Done
            }
            Step::TextEditFont(n) => {
                self.text_edits.font = *n;
                cx.notify();
                Done
            }
            Step::SignPanel => {
                self.toggle_sign_panel(window, cx);
                Done
            }
            Step::SignTest => {
                self.signing.test_signer = true;
                cx.notify();
                Done
            }
            Step::SignVisible(on) => {
                self.signing.visible = *on;
                cx.notify();
                Done
            }
            Step::SignReason(text) => {
                let field = self.signing.reason.clone();
                field.update(cx, |input, cx| input.set_text(text, cx));
                Done
            }
            Step::SignSave(path) => {
                self.sign_document(Some(path.clone()), window, cx);
                Done
            }
            Step::EditMenu => {
                self.flip_menu(crate::reader::Menu::Edit);
                cx.notify();
                Done
            }
            Step::LayoutMenu => {
                self.flip_menu(crate::reader::Menu::Layout);
                cx.notify();
                Done
            }
            Step::Language(choice) => {
                self.set_language(*choice, cx);
                Done
            }
            Step::Smooth(on) => {
                self.set_smooth_scrolling(*on, cx);
                Done
            }
            Step::Wheel(notches) => {
                self.wheel_notches(*notches, window, cx);
                Done
            }
            Step::Check(check) => self.run_check_step(check, window, cx),
            Step::Labels(step) => self.run_label_step(step, window, cx),
            Step::Search(step) => self.run_search_step(step, cx),
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
    fn every_command_runs_by_name() {
        for &command in ReaderCommand::ALL {
            assert_eq!(
                parse(&format!("cmd={command:?}")).unwrap(),
                vec![Step::Command(command)]
            );
        }
    }

    #[test]
    fn text_edit_steps_parse() {
        assert_eq!(
            parse(
                r"text-edit-panel; text-edit=2|11月3日|12月31日; text-edit=1|a|;
                  text-edit-begin=3|主旨; text-edit-font=1; text-edit-save=D:\o u.pdf"
            )
            .unwrap(),
            vec![
                Step::TextEditPanel,
                Step::TextEdit(2, "11月3日".into(), "12月31日".into()),
                Step::TextEdit(1, "a".into(), String::new()),
                Step::TextEditBegin(3, "主旨".into()),
                Step::TextEditFont(1),
                Step::TextEditSave(PathBuf::from(r"D:\o u.pdf")),
            ]
        );
        for bad in [
            "text-edit=0|a|b",
            "text-edit=1||b",
            "text-edit=1|a",
            "text-edit-begin=1",
            "text-edit-font=3",
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn sign_steps_parse() {
        assert_eq!(
            parse(r"sign-test; sign-panel; sign-visible=on; sign-reason=同意; sign-save=D:\s.pdf")
                .unwrap(),
            vec![
                Step::SignTest,
                Step::SignPanel,
                Step::SignVisible(true),
                Step::SignReason("同意".into()),
                Step::SignSave(PathBuf::from(r"D:\s.pdf")),
            ]
        );
        assert!(parse("sign-visible=maybe").is_err());
    }

    #[test]
    fn settings_steps_parse() {
        let steps = parse(
            "appearance=dark; night=on; night=toggle; sidebar=pages; sidebar=off; \
             default-zoom=fit-page; settings; layout-menu",
        )
        .unwrap();
        assert_eq!(
            steps,
            vec![
                Step::Appearance(Appearance::Dark),
                Step::Night(Some(true)),
                Step::Night(None),
                Step::Sidebar(Some(SidebarTab::Pages)),
                Step::Sidebar(None),
                Step::DefaultZoom(DefaultZoom::FitPage),
                Step::SettingsPanel,
                Step::LayoutMenu,
            ]
        );
        assert_eq!(
            parse("language=zh-TW; smooth=off; wheel=-3").unwrap(),
            vec![
                Step::Language(LanguageChoice::Fixed(
                    crate::i18n::Language::TraditionalChinese
                )),
                Step::Smooth(false),
                Step::Wheel(-3),
            ]
        );
        for bad in [
            "appearance=sepia",
            "night=maybe",
            "sidebar=left",
            "default-zoom=200",
            "language=fr",
            "wheel=0",
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
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
