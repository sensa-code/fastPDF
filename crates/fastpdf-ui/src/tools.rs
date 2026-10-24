//! Writing new PDFs from the open document (ADR 0013): "Save a copy" and the
//! page tools panel (extract, delete, rotate, reorder, split, merge).
//!
//! The reader only describes the job ([`WriteJob`]) and asks where to save
//! it. The app's writer (`ReaderOptions::writer`) runs it in an isolated
//! child process; the reader waits on a background thread, so the window
//! stays responsive, and shows the result in a notice at the bottom of the
//! document area, from where the new file can be opened. The open document
//! is never modified.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};

use fastpdf_engine_api::write::{
    OutputContent, OutputSpec, PagePick, WriteError, WriteJob, WriteReport,
};
use fastpdf_engine_api::{CancelToken, PageIndex, Rotation};
use gpui::{
    AppContext, ClickEvent, Context, Div, Entity, InteractiveElement, IntoElement, ParentElement,
    PathPromptOptions, PromptLevel, StatefulInteractiveElement, Styled, Subscription, Window, div,
    prelude::FluentBuilder, px,
};

use crate::i18n::Strings;
use crate::opening::display_name;
use crate::reader::{DocState, ReaderView};
use crate::text_input::{TextChanged, TextInput};
use crate::toolbar::styled_button;

/// Key context of the panel's page field: editing keys only.
const FIELD_CONTEXT: &str = "TextInput ToolsPanel";
const WIDTH: f32 = 470.0;
/// Longest wait for a cancelled write when FastPDF quits.
const EXIT_WAIT: std::time::Duration = std::time::Duration::from_secs(20);
const LABEL_WIDTH: f32 = 76.0;

/// A page tool of the panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum PageTool {
    #[default]
    Extract,
    Delete,
    Rotate,
    Reorder,
    Split,
    Merge,
}

impl PageTool {
    pub(crate) const ALL: [Self; 6] = [
        Self::Extract,
        Self::Delete,
        Self::Rotate,
        Self::Reorder,
        Self::Split,
        Self::Merge,
    ];

    /// The name development scripts use.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Extract => "extract",
            Self::Delete => "delete",
            Self::Rotate => "rotate",
            Self::Reorder => "reorder",
            Self::Split => "split",
            Self::Merge => "merge",
        }
    }

    pub(crate) fn from_name(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|t| t.name().eq_ignore_ascii_case(name.trim()))
    }
}

/// Why the panel's settings cannot be saved yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SpecError {
    /// Nothing entered where pages are needed.
    Empty,
    /// A part that is not a page or a range.
    Invalid(String),
    /// A page the document does not have (one-based).
    NoSuchPage(u32),
    /// Deleting would leave no page.
    DeletesEverything,
    /// Merge without files to append.
    NothingToMerge,
}

/// Pages named by `spec` ("1-3, 5", "8-", "5-1"), zero-based, in the order
/// written, repeats kept; `count` pages exist.
pub(crate) fn parse_pages(spec: &str, count: u32) -> Result<Vec<u32>, SpecError> {
    let mut pages = Vec::new();
    for part in spec
        .split([',', '，', '、'])
        .map(str::trim)
        .filter(|p| !p.is_empty())
    {
        let invalid = || SpecError::Invalid(part.to_owned());
        let number = |text: &str| -> Result<u32, SpecError> {
            let n: u32 = text.trim().parse().map_err(|_| invalid())?;
            if n == 0 {
                return Err(invalid());
            }
            if n > count {
                return Err(SpecError::NoSuchPage(n));
            }
            Ok(n)
        };
        let (first, last) = match part.split_once(['-', '–', '~', '～']) {
            Some((first, last)) if last.trim().is_empty() => (number(first)?, count),
            Some((first, last)) => (number(first)?, number(last)?),
            None => {
                let n = number(part)?;
                (n, n)
            }
        };
        if first <= last {
            pages.extend((first..=last).map(|n| n - 1));
        } else {
            pages.extend((last..=first).rev().map(|n| n - 1));
        }
    }
    if pages.is_empty() {
        return Err(SpecError::Empty);
    }
    Ok(pages)
}

/// The files of a split: a number alone means that many pages per file
/// ("1": every page on its own); otherwise ranges separated by ";".
pub(crate) fn parse_split(spec: &str, count: u32) -> Result<Vec<Vec<u32>>, SpecError> {
    let spec = spec.trim();
    if let Ok(per_file) = spec.parse::<u32>() {
        if per_file == 0 {
            return Err(SpecError::Invalid(spec.to_owned()));
        }
        let pages: Vec<u32> = (0..count).collect();
        return Ok(pages
            .chunks(per_file as usize)
            .map(<[u32]>::to_vec)
            .collect());
    }
    let groups = spec
        .split([';', '；'])
        .map(str::trim)
        .filter(|g| !g.is_empty())
        .map(|g| parse_pages(g, count))
        .collect::<Result<Vec<_>, _>>()?;
    if groups.is_empty() {
        return Err(SpecError::Empty);
    }
    Ok(groups)
}

/// The pages a tool writes into its single output, or the outputs of a
/// split (each a list of pages).
fn plan(
    tool: PageTool,
    spec: &str,
    count: u32,
    angle: Rotation,
) -> Result<Vec<Vec<PagePick>>, SpecError> {
    let picks = |pages: &[u32]| -> Vec<PagePick> {
        pages
            .iter()
            .map(|&p| PagePick::new(0, PageIndex::new(p)))
            .collect()
    };
    Ok(match tool {
        PageTool::Extract | PageTool::Reorder => vec![picks(&parse_pages(spec, count)?)],
        PageTool::Delete => {
            let gone = page_mask(&parse_pages(spec, count)?, count);
            let kept: Vec<u32> = (0..count).filter(|&p| !gone[p as usize]).collect();
            if kept.is_empty() {
                return Err(SpecError::DeletesEverything);
            }
            vec![picks(&kept)]
        }
        PageTool::Rotate => {
            let turned = if spec.trim().is_empty() {
                vec![true; count as usize]
            } else {
                page_mask(&parse_pages(spec, count)?, count)
            };
            let mut all = picks(&(0..count).collect::<Vec<_>>());
            for pick in &mut all {
                if turned[pick.page.as_usize()] {
                    pick.rotate = angle;
                }
            }
            vec![all]
        }
        PageTool::Split => parse_split(spec, count)?
            .iter()
            .map(|pages| picks(pages))
            .collect(),
        // The files to append are added by the caller.
        PageTool::Merge => Vec::new(),
    })
}

/// One flag per page: whether `pages` names it.
fn page_mask(pages: &[u32], count: u32) -> Vec<bool> {
    let mut mask = vec![false; count as usize];
    for &p in pages {
        if let Some(flag) = mask.get_mut(p as usize) {
            *flag = true;
        }
    }
    mask
}

/// `name` with a `.pdf` extension when it has none.
fn with_pdf_extension(mut path: PathBuf) -> PathBuf {
    if path.extension().is_none() {
        path.set_extension("pdf");
    }
    path
}

/// `dir/name`, or `dir/name (2)`, ... when that file exists.
fn unused_path(dir: &Path, stem: &str) -> PathBuf {
    let first = dir.join(format!("{stem}.pdf"));
    if !first.exists() {
        return first;
    }
    (2..)
        .map(|n| dir.join(format!("{stem} ({n}).pdf")))
        .find(|p| !p.exists())
        .unwrap_or(first)
}

/// Where a single output goes: the save dialog's suggestion, or a path
/// given in advance.
struct Target {
    suggested: String,
    preset: Option<PathBuf>,
}

/// A split into `dir`: files named after the document, numbered, never
/// replacing a file that is already there.
fn split_job(source: PathBuf, stem: &str, outputs: Vec<Vec<PagePick>>, dir: &Path) -> WriteJob {
    let width = outputs.len().to_string().len();
    let mut taken: Vec<PathBuf> = Vec::new();
    let specs = outputs
        .into_iter()
        .enumerate()
        .map(|(i, pages)| {
            let mut path = unused_path(dir, &format!("{stem}-{:0width$}", i + 1));
            // Two outputs must not get the same free name either.
            let mut n = 2;
            while taken.contains(&path) {
                path = dir.join(format!("{stem}-{:0width$} ({n}).pdf", i + 1));
                n += 1;
            }
            taken.push(path.clone());
            OutputSpec {
                path,
                content: OutputContent::Pages(pages),
            }
        })
        .collect();
    WriteJob {
        sources: vec![source],
        outputs: specs,
    }
}

/// A write job on its background thread.
pub(crate) struct RunningWrite {
    cancel: CancelToken,
    rx: Receiver<Result<WriteReport, WriteError>>,
}

/// The result of the last write, shown under the document.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum WriteNotice {
    Saving,
    Saved(WriteReport),
    Failed(WriteError),
    Cancelled,
}

/// The page tools panel and the write in progress.
pub(crate) struct ToolsPanel {
    pub open: bool,
    pub tool: PageTool,
    pub field: Entity<TextInput>,
    pub angle: Rotation,
    /// Merge: files appended after the open document, in order.
    pub merge: Vec<PathBuf>,
    pub problem: Option<SpecError>,
    pub job: Option<RunningWrite>,
    pub notice: Option<WriteNotice>,
    _changes: Subscription,
}

impl std::fmt::Debug for ToolsPanel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolsPanel")
            .field("open", &self.open)
            .field("tool", &self.tool)
            .field("writing", &self.job.is_some())
            .field("notice", &self.notice)
            .finish_non_exhaustive()
    }
}

impl ToolsPanel {
    pub(crate) fn new(cx: &mut Context<'_, ReaderView>) -> Self {
        let field = cx.new(|cx| TextInput::with_context("", FIELD_CONTEXT, cx));
        let changes = cx.subscribe(&field, |view: &mut ReaderView, _, _: &TextChanged, cx| {
            view.tools.problem = None;
            cx.notify();
        });
        Self {
            open: false,
            tool: PageTool::default(),
            field,
            angle: Rotation::R90,
            merge: Vec::new(),
            problem: None,
            job: None,
            notice: None,
            _changes: changes,
        }
    }

    pub(crate) fn writing(&self) -> bool {
        self.job.is_some()
    }

    /// FastPDF is quitting: a write in progress is cancelled, and its
    /// thread is waited for (bounded), so that the runner can undo what the
    /// writer left before the process, and with it the writer, ends.
    pub(crate) fn finish_before_exit(&mut self) {
        if let Some(job) = self.job.take() {
            job.cancel.cancel();
            if job.rx.recv_timeout(EXIT_WAIT).is_err() {
                log::warn!("a write was still running when FastPDF quit");
            }
        }
    }

    /// Picks up a finished write. True when something changed.
    pub(crate) fn drain(&mut self) -> bool {
        let Some(job) = &self.job else {
            return false;
        };
        let Ok(result) = job.rx.try_recv() else {
            return false;
        };
        let cancelled = job.cancel.is_cancelled();
        self.job = None;
        // A job that finished before the cancel took effect did save.
        self.notice = Some(match result {
            Ok(report) => WriteNotice::Saved(report),
            Err(_) if cancelled => WriteNotice::Cancelled,
            Err(e) => WriteNotice::Failed(e),
        });
        true
    }
}

impl ReaderView {
    /// The open document's path, its display stem and page count.
    fn write_source(&self) -> Option<(PathBuf, String, u32)> {
        let DocState::Open(open) = &self.doc else {
            return None;
        };
        let stem = open
            .path
            .file_stem()
            .map_or_else(|| "document".into(), |s| s.to_string_lossy().into_owned());
        Some((open.path.clone(), stem, open.session.page_count()))
    }

    pub(crate) fn toggle_tools_panel(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if self.tools.open {
            self.close_tools_panel(window, cx);
            return;
        }
        self.tools.open = true;
        self.tools.problem = None;
        let field = self.tools.field.clone();
        field.update(cx, |input, cx| {
            input.select_everything(cx);
            window.focus(input.focus_handle(), cx);
        });
        cx.notify();
    }

    pub(crate) fn close_tools_panel(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.tools.open = false;
        window.focus(&self.focus, cx);
        cx.notify();
    }

    /// Ctrl+Shift+S: the open file, as it is, under a new name. `preset`
    /// skips the save dialog (development scripts).
    pub(crate) fn save_copy(
        &mut self,
        preset: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let Some((source, stem, _)) = self.write_source() else {
            return;
        };
        let suggested = format!("{stem}{}.pdf", self.strings().copy_suffix);
        let target = Target { suggested, preset };
        self.ask_where(source.clone(), target, window, cx, move |source, path| {
            Ok(WriteJob {
                sources: vec![source],
                outputs: vec![OutputSpec {
                    path,
                    content: OutputContent::Copy { source: 0 },
                }],
            })
        });
    }

    /// The panel's "Save as" button. `preset` skips the save dialog (a file,
    /// or a folder for a split; development scripts).
    pub(crate) fn save_tool_result(
        &mut self,
        preset: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let Some((source, stem, count)) = self.write_source() else {
            return;
        };
        let tool = self.tools.tool;
        let spec = self.tools.field.read(cx).text().to_string();
        let merge = self.tools.merge.clone();
        let planned = if tool == PageTool::Merge {
            if merge.is_empty() {
                Err(SpecError::NothingToMerge)
            } else {
                Ok(Vec::new())
            }
        } else {
            plan(tool, &spec, count, self.tools.angle)
        };
        let outputs = match planned {
            Ok(outputs) => outputs,
            Err(problem) => {
                self.tools.problem = Some(problem);
                cx.notify();
                return;
            }
        };
        let strings = self.strings();
        if tool == PageTool::Split {
            if let Some(dir) = preset {
                self.start_write(split_job(source, &stem, outputs, &dir), cx);
            } else {
                self.ask_folder(source, stem, outputs, window, cx);
            }
            return;
        }
        let suffix = match tool {
            PageTool::Extract => strings.extract_suffix,
            PageTool::Delete => strings.delete_suffix,
            PageTool::Rotate => strings.rotate_suffix,
            PageTool::Reorder => strings.reorder_suffix,
            PageTool::Merge | PageTool::Split => strings.merge_suffix,
        };
        let target = Target {
            suggested: format!("{stem}{suffix}.pdf"),
            preset,
        };
        self.ask_where(source, target, window, cx, move |source, path| {
            let mut sources = vec![source];
            let content = if tool == PageTool::Merge {
                sources.extend(merge.iter().cloned());
                OutputContent::Merge((0..sources.len()).collect())
            } else {
                OutputContent::Pages(outputs.into_iter().next().unwrap_or_default())
            };
            Ok(WriteJob {
                sources,
                outputs: vec![OutputSpec { path, content }],
            })
        });
    }

    /// [`Self::ask_where`] for other writers (stamps): `suggested` in the
    /// save dialog, or `preset` without it.
    pub(crate) fn ask_where_for(
        &mut self,
        source: PathBuf,
        suggested: String,
        preset: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
        make: impl FnOnce(PathBuf, PathBuf) -> Result<WriteJob, SpecError> + 'static,
    ) {
        self.ask_where(source, Target { suggested, preset }, window, cx, make);
    }

    /// Asks for a file name next to the open file (unless `target` has
    /// one), then writes the job `make` builds for it.
    fn ask_where(
        &mut self,
        source: PathBuf,
        target: Target,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
        make: impl FnOnce(PathBuf, PathBuf) -> Result<WriteJob, SpecError> + 'static,
    ) {
        if self.tools.writing() {
            return;
        }
        if let Some(path) = target.preset {
            match make(source, with_pdf_extension(path)) {
                Ok(job) => self.start_write(job, cx),
                Err(problem) => {
                    self.tools.problem = Some(problem);
                    cx.notify();
                }
            }
            return;
        }
        let dir = source.parent().map(Path::to_path_buf).unwrap_or_default();
        let answer = cx.prompt_for_new_path(&dir, Some(&target.suggested));
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(chosen))) = answer.await else {
                return;
            };
            let path = with_pdf_extension(chosen.clone());
            // The dialog asked about the name as typed; a file under the
            // completed name must not be replaced without asking.
            if path != chosen && path.exists() {
                let confirm = this.update_in(cx, |this, window, cx| {
                    let strings = this.strings();
                    window.prompt(
                        PromptLevel::Warning,
                        &strings.replace_file(&display_name(&path)),
                        None,
                        &[strings.replace, strings.cancel],
                        cx,
                    )
                });
                let Ok(confirm) = confirm else {
                    return;
                };
                if confirm.await != Ok(0) {
                    return;
                }
            }
            let _ = this.update_in(cx, |this, _, cx| match make(source, path) {
                Ok(job) => this.start_write(job, cx),
                Err(problem) => {
                    this.tools.problem = Some(problem);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Split: asks for a folder; the files are named after the document.
    fn ask_folder(
        &mut self,
        source: PathBuf,
        stem: String,
        outputs: Vec<Vec<PagePick>>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        if self.tools.writing() {
            return;
        }
        let answer = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: None,
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(dirs))) = answer.await else {
                return;
            };
            let Some(dir) = dirs.into_iter().next() else {
                return;
            };
            let job = split_job(source, &stem, outputs, &dir);
            let _ = this.update(cx, |this, cx| this.start_write(job, cx));
        })
        .detach();
    }

    /// Merge: asks for the files to append.
    pub(crate) fn add_merge_files(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let answer = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: None,
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = answer.await else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                this.tools.merge.extend(paths);
                this.tools.problem = None;
                cx.notify();
            });
        })
        .detach();
    }

    /// Runs `job` with the app's writer on a background thread.
    fn start_write(&mut self, job: WriteJob, cx: &mut Context<'_, Self>) {
        if self.tools.writing() {
            return;
        }
        let Some(writer) = self.options.writer.clone() else {
            self.tools.notice = Some(WriteNotice::Failed(WriteError::Process(
                self.strings().writing_unavailable.into(),
            )));
            cx.notify();
            return;
        };
        log::info!(
            "writing {} output(s) from {} source(s)",
            job.outputs.len(),
            job.sources.len()
        );
        let cancel = CancelToken::new();
        let thread_cancel = cancel.clone();
        let (tx, rx) = mpsc::channel();
        let waker = self.waker.clone();
        let spawned = std::thread::Builder::new()
            .name("fastpdf-write".into())
            .spawn(move || {
                let result = writer(&job, &thread_cancel);
                match &result {
                    Ok(report) => log::info!("wrote {} file(s)", report.outputs.len()),
                    Err(e) => log::warn!("writing failed: {e}"),
                }
                let _ = tx.send(result);
                waker.wake();
            });
        self.tools.notice = Some(match spawned {
            Ok(_) => {
                self.tools.job = Some(RunningWrite { cancel, rx });
                WriteNotice::Saving
            }
            Err(e) => WriteNotice::Failed(WriteError::Process(format!(
                "cannot start the writer thread ({e})"
            ))),
        });
        cx.notify();
    }

    pub(crate) fn cancel_write(&mut self, cx: &mut Context<'_, Self>) {
        if let Some(job) = &self.tools.job {
            job.cancel.cancel();
            cx.notify();
        }
    }

    /// The page tools panel, floating over the top of the document.
    pub(crate) fn render_tools_panel(
        &self,
        cx: &mut Context<'_, Self>,
    ) -> impl IntoElement + use<> {
        let theme = self.theme;
        let strings = self.strings();
        let panel = &self.tools;
        let editable = !panel.writing();
        let tools = PageTool::ALL.into_iter().enumerate().map(|(i, tool)| {
            styled_button(
                format!("page-tool-{i}"),
                tool_name(strings, tool),
                editable,
                panel.tool == tool,
                &theme,
            )
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                this.tools.tool = tool;
                this.tools.problem = None;
                if tool != PageTool::Merge {
                    let field = this.tools.field.clone();
                    field.update(cx, |input, cx| {
                        window.focus(input.focus_handle(), cx);
                    });
                }
                cx.notify();
            }))
        });
        let mut body = div()
            .flex()
            .flex_col()
            .gap_2()
            .child(div().text_size(px(15.0)).child(strings.page_tools_title))
            .child(div().flex().flex_row().flex_wrap().gap_1().children(tools))
            .child(
                div()
                    .text_size(px(12.0))
                    .text_color(theme.text_muted)
                    .child(tool_hint(strings, panel.tool)),
            );
        if panel.tool == PageTool::Merge {
            body = body.child(self.render_merge_list(cx));
        } else {
            body = body.child(
                row(strings.pages).child(
                    div()
                        .flex_1()
                        .h(px(28.0))
                        .px_2()
                        .rounded(px(4.0))
                        .bg(theme.input_bg)
                        .border_1()
                        .border_color(if panel.problem.is_some() {
                            theme.error_text
                        } else {
                            theme.accent
                        })
                        .text_size(px(14.0))
                        .child(panel.field.clone()),
                ),
            );
        }
        if panel.tool == PageTool::Rotate {
            let angles = [
                (Rotation::R90, strings.rotate_right_90),
                (Rotation::R180, strings.rotate_180),
                (Rotation::R270, strings.rotate_left_90),
            ];
            body = body.child(row(strings.angle).children(angles.into_iter().map(
                |(angle, label)| {
                    styled_button(
                        format!("rotate-angle-{}", angle.degrees()),
                        label,
                        editable,
                        panel.angle == angle,
                        &theme,
                    )
                    .on_click(cx.listener(
                        move |this, _: &ClickEvent, _, cx| {
                            this.tools.angle = angle;
                            cx.notify();
                        },
                    ))
                },
            )));
        }
        if let Some(problem) = &panel.problem {
            body = body.child(
                div()
                    .text_color(theme.error_text)
                    .child(spec_problem(strings, problem)),
            );
        }
        body = body.child(
            div()
                .text_size(px(12.0))
                .text_color(theme.text_muted)
                .child(strings.original_untouched),
        );
        let has_doc = self.session().is_some();
        let can_save = has_doc && editable;
        let save_label = if panel.tool == PageTool::Split {
            strings.choose_folder
        } else {
            strings.save_as
        };
        body = body.child(
            div()
                .flex()
                .flex_row()
                .justify_end()
                .gap_1()
                .child(
                    styled_button("tools-save", save_label, can_save, false, &theme)
                        .when(can_save, |b| b.bg(theme.button_active))
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.save_tool_result(None, window, cx);
                        })),
                )
                .child(
                    styled_button("tools-close", strings.close, true, false, &theme).on_click(
                        cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.close_tools_panel(window, cx);
                        }),
                    ),
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
                    .id("tools-panel")
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

    fn render_merge_list(&self, cx: &mut Context<'_, Self>) -> Div {
        let theme = self.theme;
        let strings = self.strings();
        let editable = !self.tools.writing();
        let current = match &self.doc {
            DocState::Open(open) => open.name.to_string(),
            _ => String::new(),
        };
        let mut list = div()
            .id("merge-list")
            .flex()
            .flex_col()
            .gap_1()
            .p_1()
            .max_h(px(180.0))
            .overflow_y_scroll()
            .rounded(px(4.0))
            .border_1()
            .border_color(theme.toolbar_border)
            .bg(theme.input_bg)
            .child(
                div()
                    .px_1()
                    .text_color(theme.text_muted)
                    .child(format!("1. {current}")),
            );
        for (i, path) in self.tools.merge.iter().enumerate() {
            list = list.child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_1()
                    .child(
                        div()
                            .flex_1()
                            .px_1()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .child(format!("{}. {}", i + 2, display_name(path))),
                    )
                    .child(
                        styled_button(
                            format!("merge-remove-{i}"),
                            "\u{2715}",
                            editable,
                            false,
                            &theme,
                        )
                        .on_click(cx.listener(
                            move |this, _: &ClickEvent, _, cx| {
                                if i < this.tools.merge.len() {
                                    this.tools.merge.remove(i);
                                    cx.notify();
                                }
                            },
                        )),
                    ),
            );
        }
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(list)
            .child(div().flex().flex_row().child(
                styled_button("merge-add", strings.add_files, editable, false, &theme).on_click(
                    cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.add_merge_files(window, cx);
                    }),
                ),
            ))
    }

    /// The last write's result (or progress), at the bottom of the
    /// document area.
    pub(crate) fn render_write_notice(&self, cx: &mut Context<'_, Self>) -> Option<Div> {
        let notice = self.tools.notice.as_ref()?;
        let theme = self.theme;
        let strings = self.strings();
        let (text, color) = match notice {
            WriteNotice::Saving => (strings.saving.to_string(), theme.text),
            WriteNotice::Saved(report) => (saved_text(strings, report), theme.text),
            WriteNotice::Failed(e) => (write_error_text(strings, e), theme.error_text),
            WriteNotice::Cancelled => (strings.write_cancelled.to_string(), theme.text_muted),
        };
        let open_target = match notice {
            WriteNotice::Saved(report) if report.outputs.len() == 1 => {
                report.outputs.first().map(|o| o.path.clone())
            }
            _ => None,
        };
        let saving = matches!(notice, WriteNotice::Saving);
        Some(
            div()
                .absolute()
                .bottom(px(8.0 + crate::scrollbar::THICKNESS))
                .left(px(8.0))
                .max_w(px(560.0))
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .p_2()
                .rounded(px(6.0))
                .bg(theme.toolbar_bg)
                .border_1()
                .border_color(theme.toolbar_border)
                .text_size(px(13.0))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .text_color(color)
                        .child(text),
                )
                .when_some(open_target, |d, path| {
                    d.child(
                        styled_button("notice-open", strings.open_result, true, false, &theme)
                            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                this.tools.notice = None;
                                this.open_path(path.clone(), window, cx);
                            })),
                    )
                })
                .child(
                    styled_button(
                        "notice-close",
                        if saving { strings.cancel } else { "\u{2715}" },
                        true,
                        false,
                        &theme,
                    )
                    .on_click(cx.listener(
                        move |this, _: &ClickEvent, _, cx| {
                            if this.tools.writing() {
                                this.cancel_write(cx);
                            } else {
                                this.tools.notice = None;
                                cx.notify();
                            }
                        },
                    )),
                ),
        )
    }
}

fn row(label: &'static str) -> Div {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap_1()
        .child(div().w(px(LABEL_WIDTH)).flex_none().child(label))
}

fn tool_name(strings: &Strings, tool: PageTool) -> &'static str {
    match tool {
        PageTool::Extract => strings.tool_extract,
        PageTool::Delete => strings.tool_delete,
        PageTool::Rotate => strings.tool_rotate,
        PageTool::Reorder => strings.tool_reorder,
        PageTool::Split => strings.tool_split,
        PageTool::Merge => strings.tool_merge,
    }
}

fn tool_hint(strings: &Strings, tool: PageTool) -> &'static str {
    match tool {
        PageTool::Extract => strings.hint_extract,
        PageTool::Delete => strings.hint_delete,
        PageTool::Rotate => strings.hint_rotate,
        PageTool::Reorder => strings.hint_reorder,
        PageTool::Split => strings.hint_split,
        PageTool::Merge => strings.hint_merge,
    }
}

fn spec_problem(strings: &Strings, problem: &SpecError) -> String {
    match problem {
        SpecError::Empty => strings.no_pages_given.into(),
        SpecError::Invalid(part) => strings.invalid_pages(part),
        SpecError::NoSuchPage(n) => strings.no_such_page(*n),
        SpecError::DeletesEverything => strings.cannot_delete_all.into(),
        SpecError::NothingToMerge => strings.nothing_to_merge.into(),
    }
}

fn saved_text(strings: &Strings, report: &WriteReport) -> String {
    let saved = match report.outputs.as_slice() {
        [one] => strings.saved_file(&display_name(&one.path), one.pages),
        many => {
            let dir = many
                .first()
                .and_then(|o| o.path.parent())
                .map(display_name)
                .unwrap_or_default();
            strings.saved_files(many.len(), &dir)
        }
    };
    let removed: u32 = report.outputs.iter().map(|o| o.signatures_removed).sum();
    if removed == 0 {
        saved
    } else {
        format!("{saved} {}", strings.signatures_removed(removed))
    }
}

pub(crate) fn write_error_text(strings: &Strings, error: &WriteError) -> String {
    match error {
        WriteError::Encrypted { .. } => strings.write_encrypted.into(),
        WriteError::NoSuchPage { page, .. } => strings.no_such_page(page.display_number()),
        WriteError::Source { message, .. } => strings.write_source_failed(message),
        WriteError::Image { path, message } => {
            strings.write_image_failed(&display_name(path), message)
        }
        WriteError::Edit { page, problem, .. } => {
            strings.edit_failed(page.display_number(), problem)
        }
        WriteError::Sign(problem) => strings.sign_failed(problem),
        WriteError::Output { path, message } => {
            strings.write_output_failed(&display_name(path), message)
        }
        WriteError::Verify { message, .. }
        | WriteError::Job(message)
        | WriteError::Process(message) => strings.write_failed(message),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_specs_keep_order_and_repeats() {
        assert_eq!(parse_pages("1-3, 5", 10), Ok(vec![0, 1, 2, 4]));
        assert_eq!(parse_pages("3,1,2", 3), Ok(vec![2, 0, 1]));
        assert_eq!(parse_pages("8-", 10), Ok(vec![7, 8, 9]));
        assert_eq!(parse_pages("5-3", 10), Ok(vec![4, 3, 2]));
        assert_eq!(parse_pages("2, 2", 3), Ok(vec![1, 1]));
        assert_eq!(parse_pages(" 1 ～ 2 ，3、4 ", 4), Ok(vec![0, 1, 2, 3]));
        assert_eq!(parse_pages("", 3), Err(SpecError::Empty));
        assert_eq!(parse_pages("0", 3), Err(SpecError::Invalid("0".into())));
        assert_eq!(parse_pages("4", 3), Err(SpecError::NoSuchPage(4)));
        assert_eq!(parse_pages("1-x", 3), Err(SpecError::Invalid("1-x".into())));
    }

    #[test]
    fn splits_by_size_or_by_ranges() {
        assert_eq!(
            parse_split("2", 5),
            Ok(vec![vec![0, 1], vec![2, 3], vec![4]])
        );
        assert_eq!(parse_split("1", 2), Ok(vec![vec![0], vec![1]]));
        assert_eq!(parse_split("1-2; 3-", 4), Ok(vec![vec![0, 1], vec![2, 3]]));
        assert_eq!(parse_split("0", 4), Err(SpecError::Invalid("0".into())));
        assert_eq!(parse_split(" ; ", 4), Err(SpecError::Empty));
    }

    #[test]
    fn tools_plan_their_pages() {
        let pages = |plan: Vec<Vec<PagePick>>| -> Vec<Vec<(u32, Rotation)>> {
            plan.into_iter()
                .map(|o| o.into_iter().map(|p| (p.page.get(), p.rotate)).collect())
                .collect()
        };
        let r0 = Rotation::R0;
        assert_eq!(
            pages(plan(PageTool::Delete, "2", 3, r0).unwrap()),
            [[(0, r0), (2, r0)]]
        );
        assert_eq!(
            plan(PageTool::Delete, "1-3", 3, r0).unwrap_err(),
            SpecError::DeletesEverything
        );
        let turned = pages(plan(PageTool::Rotate, "2", 3, Rotation::R270).unwrap());
        assert_eq!(turned, [[(0, r0), (1, Rotation::R270), (2, r0)]]);
        let all = pages(plan(PageTool::Rotate, "", 2, Rotation::R180).unwrap());
        assert_eq!(all, [[(0, Rotation::R180), (1, Rotation::R180)]]);
        assert_eq!(pages(plan(PageTool::Split, "1", 2, r0).unwrap()).len(), 2);
    }

    #[test]
    fn output_names() {
        assert_eq!(
            with_pdf_extension(PathBuf::from(r"C:\a\b")),
            PathBuf::from(r"C:\a\b.pdf")
        );
        assert_eq!(
            with_pdf_extension(PathBuf::from(r"C:\a\b.PDF")),
            PathBuf::from(r"C:\a\b.PDF")
        );
        let dir = std::env::temp_dir().join(format!("fastpdf-tools-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(unused_path(&dir, "x-1"), dir.join("x-1.pdf"));
        std::fs::write(dir.join("x-1.pdf"), b"").unwrap();
        assert_eq!(unused_path(&dir, "x-1"), dir.join("x-1 (2).pdf"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
