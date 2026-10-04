//! The reader window's root view: document state, commands, input.

use std::future::Future;
use std::mem;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use fastpdf_cache::{BudgetConfig, MemoryBudgetManager};
use fastpdf_core::keymap::ReaderCommand;
use fastpdf_core::memory::MemoryMonitor;
use fastpdf_core::recent::RecentFiles;
use fastpdf_core::{DocumentSession, SessionConfig};
use fastpdf_engine_api::{EngineDocument, PageIndex, PdfEngine};
use futures::StreamExt;
use futures::channel::mpsc;
use gpui::{
    App, AppContext, Bounds, Context, Entity, ExternalPaths, FocusHandle, InteractiveElement,
    IntoElement, ParentElement, PathPromptOptions, PinchEvent, Pixels, Render, ScrollWheelEvent,
    SharedString, Styled, Subscription, Task, TitlebarOptions, Window, WindowBounds, WindowHandle,
    WindowOptions, canvas, div, prelude::FluentBuilder, px, size,
};

use crate::actions::{KEY_CONTEXT, all_actions};
use crate::bench::{BenchEvent, BenchHook};
use crate::document::{OpenFailure, OpenedDocument, PendingOpen, open_document_blocking};
use crate::overlay::DevOverlay;
use crate::textures::{DEFAULT_UPLOAD_BUDGET, TileImage, TileTextures, to_render_image};
use crate::theme::{Theme, UI_FONT};
use crate::toolbar;

/// Scroll distance of one wheel "line" and of the arrow keys, in logical
/// pixels (three lines per wheel notch at the Windows default).
const LINE_SCROLL_PX: f32 = 33.0;
/// Ctrl + wheel: logical pixels of wheel travel that double the zoom.
const WHEEL_PX_PER_ZOOM_DOUBLING: f32 = 400.0;
const APP_TITLE: &str = "FastPDF";

/// Everything the reader needs from the application.
#[derive(Clone)]
pub struct ReaderOptions {
    pub engine: Arc<dyn PdfEngine>,
    pub session: SessionConfig,
    /// Bytes of new tile textures uploaded per frame at most.
    pub upload_budget: usize,
    pub dev_overlay: bool,
    pub memory: BudgetConfig,
    pub bench: Option<BenchHook>,
    /// Where the recent-files list lives; `None` disables it.
    pub recent_files: Option<PathBuf>,
}

impl std::fmt::Debug for ReaderOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReaderOptions")
            .field("engine", &self.engine.info().name)
            .field("session", &self.session)
            .field("upload_budget", &self.upload_budget)
            .field("dev_overlay", &self.dev_overlay)
            .finish_non_exhaustive()
    }
}

impl ReaderOptions {
    pub fn new(engine: Arc<dyn PdfEngine>) -> Self {
        Self {
            engine,
            session: SessionConfig::default(),
            upload_budget: DEFAULT_UPLOAD_BUDGET,
            dev_overlay: false,
            memory: BudgetConfig::default(),
            bench: None,
            recent_files: RecentFiles::default_location(),
        }
    }
}

/// Opens the main window; `initial` is a document already being opened.
pub fn open_reader_window(
    cx: &mut App,
    options: ReaderOptions,
    initial: Option<PendingOpen>,
) -> gpui::Result<WindowHandle<ReaderView>> {
    let bench = options.bench.clone();
    let window_size = default_window_size(cx);
    let handle = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                None,
                window_size,
                cx,
            ))),
            titlebar: Some(TitlebarOptions {
                title: Some(APP_TITLE.into()),
                ..Default::default()
            }),
            window_min_size: Some(size(px(360.0), px(240.0))),
            app_id: Some(APP_TITLE.into()),
            ..Default::default()
        },
        |window, cx| cx.new(|cx| ReaderView::new(options, initial, window, cx)),
    )?;
    if let Some(hook) = bench {
        hook(BenchEvent::WindowVisible);
    }
    Ok(handle)
}

/// A portrait window that fits the primary display.
fn default_window_size(cx: &App) -> gpui::Size<Pixels> {
    let Some(display) = cx.primary_display() else {
        return size(px(1024.0), px(768.0));
    };
    let screen = display.bounds().size;
    let height = (f32::from(screen.height) * 0.85).clamp(480.0, 1400.0);
    let width = (height * 0.9).clamp(640.0, (f32::from(screen.width) * 0.9).max(640.0));
    size(px(width), px(height))
}

/// Wakes the UI thread from any thread; see the crate docs.
#[derive(Clone, Debug)]
struct Waker(mpsc::UnboundedSender<()>);

impl Waker {
    fn wake(&self) {
        // Fails only when the view is gone; nothing to wake then.
        let _ = self.0.unbounded_send(());
    }
}

pub(crate) struct OpenDoc {
    pub session: DocumentSession<TileImage>,
    pub path: PathBuf,
    pub name: SharedString,
    pub opened_at: Instant,
    pub exact_reported: bool,
}

pub(crate) enum DocState {
    Empty,
    Loading { path: PathBuf },
    Failed { path: PathBuf, message: String },
    Open(Box<OpenDoc>),
}

/// The reader window's root view.
pub struct ReaderView {
    pub(crate) options: ReaderOptions,
    pub(crate) focus: FocusHandle,
    pub(crate) doc: DocState,
    pub(crate) textures: TileTextures,
    pub(crate) overlay: DevOverlay,
    pub(crate) theme: Theme,
    pub(crate) sidebar_open: bool,
    /// Bounds of the document area from the last layout.
    pub(crate) viewport_bounds: Option<Bounds<Pixels>>,
    pub(crate) render_started: Option<Instant>,
    pub(crate) first_paint_reported: bool,
    waker: Waker,
    memory: MemoryMonitor,
    open_task: Option<Task<()>>,
    _wake_task: Task<()>,
    _appearance: Subscription,
}

impl std::fmt::Debug for ReaderView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReaderView")
            .field("textures", &self.textures)
            .finish_non_exhaustive()
    }
}

impl ReaderView {
    fn new(
        options: ReaderOptions,
        initial: Option<PendingOpen>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);

        // Render workers and cache evictions wake the UI through this
        // channel: a foreground task awaits it and notifies the view. No
        // polling, no timers (spec §1: idle CPU ~ 0).
        let (tx, mut rx) = mpsc::unbounded::<()>();
        let wake_task = cx.spawn(async move |this, cx| {
            while rx.next().await.is_some() {
                if this.update(cx, |view, cx| view.on_wake(cx)).is_err() {
                    break;
                }
            }
        });
        let appearance = cx.observe_window_appearance(window, |_, _, cx| cx.notify());

        let memory = MemoryMonitor::new(Arc::new(MemoryBudgetManager::new(options.memory)));
        let mut view = Self {
            textures: TileTextures::new(options.upload_budget),
            overlay: DevOverlay::new(options.dev_overlay),
            options,
            focus,
            doc: DocState::Empty,
            theme: Theme::for_appearance(window.appearance()),
            sidebar_open: false,
            viewport_bounds: None,
            render_started: None,
            first_paint_reported: false,
            waker: Waker(tx),
            memory,
            open_task: None,
            _wake_task: wake_task,
            _appearance: appearance,
        };
        if let Some(pending) = initial {
            let PendingOpen { path, result } = pending;
            let result = async move { result.await.unwrap_or(Err(OpenFailure::Abandoned)) };
            view.begin_open(path, result, window, cx);
        }
        view
    }

    pub(crate) fn report(&self, event: BenchEvent) {
        if let Some(hook) = &self.options.bench {
            hook(event);
        }
    }

    pub(crate) fn bench_hook(&self) -> Option<BenchHook> {
        self.options.bench.clone()
    }

    pub(crate) fn session(&self) -> Option<&DocumentSession<TileImage>> {
        match &self.doc {
            DocState::Open(open) => Some(&open.session),
            _ => None,
        }
    }

    fn session_mut(&mut self) -> Option<&mut DocumentSession<TileImage>> {
        match &mut self.doc {
            DocState::Open(open) => Some(&mut open.session),
            _ => None,
        }
    }

    /// A worker finished something (tiles, evictions).
    fn on_wake(&mut self, cx: &mut Context<'_, Self>) {
        // Work happened, so let the memory monitor sample (rate limited).
        if let Some(relief) = self.memory.poll()
            && relief.freed > 0
        {
            log::debug!(
                "memory pressure {:?}: freed {} bytes",
                relief.pressure,
                relief.freed
            );
        }
        cx.notify();
    }

    // ---- opening and closing ---------------------------------------------

    /// Opens `path`, replacing the current document.
    pub fn open_path(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<'_, Self>) {
        let path = std::path::absolute(&path).unwrap_or(path);
        let engine = Arc::clone(&self.options.engine);
        let open_path = path.clone();
        let task = cx
            .background_executor()
            .spawn(async move { open_document_blocking(engine.as_ref(), &open_path) });
        self.begin_open(path, task, window, cx);
    }

    fn begin_open(
        &mut self,
        path: PathBuf,
        result: impl Future<Output = Result<OpenedDocument, OpenFailure>> + 'static,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        self.close_document(window, cx);
        self.doc = DocState::Loading { path: path.clone() };
        // Replacing the task drops (cancels) any open still in progress; its
        // background work finishes and the result is discarded.
        self.open_task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = result.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.finish_open(path, result, window, cx);
            });
        }));
        cx.notify();
    }

    fn finish_open(
        &mut self,
        path: PathBuf,
        result: Result<OpenedDocument, OpenFailure>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        // A newer open replaced this one.
        if !matches!(&self.doc, DocState::Loading { path: p } if *p == path) {
            return;
        }
        let opened = match result {
            Ok(opened) => opened,
            Err(failure) => {
                log::warn!("cannot open {}: {failure}", path.display());
                self.doc = DocState::Failed {
                    message: failure.to_string(),
                    path,
                };
                window.set_window_title(APP_TITLE);
                cx.notify();
                return;
            }
        };

        let (view_size, scale) = self.viewport_estimate(window);
        let waker = self.waker.clone();
        let evict_waker = self.waker.clone();
        let retire = self.textures.retire_queue();
        let session = DocumentSession::new(
            Arc::clone(&opened.doc),
            self.options.session.clone(),
            view_size,
            scale,
            to_render_image,
            move || waker.wake(),
            // Evicted tiles must leave the GPU atlas too; only the UI thread
            // may call drop_image, so hand them over.
            move |evicted| {
                if retire.retire(evicted.into_iter().map(|(_, image)| image)) {
                    evict_waker.wake();
                }
            },
        );
        self.memory
            .manager()
            .register(session.tile_cache().budgeted());
        self.memory.watch(&opened.doc);

        let name: SharedString = display_name(&opened.path).into();
        window.set_window_title(&format!("{name} - {APP_TITLE}"));
        log::info!(
            "session for {} ({} pages, engine {})",
            opened.path.display(),
            opened.doc.page_count(),
            opened.doc.engine().name
        );
        self.remember_recent(opened.path.clone(), cx);
        self.doc = DocState::Open(Box::new(OpenDoc {
            session,
            path: opened.path,
            name,
            opened_at: Instant::now(),
            exact_reported: false,
        }));
        self.report(BenchEvent::DocumentOpened);
        cx.notify();
    }

    /// Closes the current document and frees its GPU textures.
    fn close_document(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if let DocState::Open(open) = mem::replace(&mut self.doc, DocState::Empty) {
            let OpenDoc { session, path, .. } = *open;
            log::info!("closing {}", path.display());
            // Evict every tile (they reach the retire queue), then release
            // every texture this window holds.
            session.tile_cache().remove_document(session.id());
            self.textures.release_all(window);
            // Dropping the session joins its render workers, which may wait
            // for a page that is still rasterizing: keep that off the UI thread.
            cx.background_executor()
                .spawn(async move { drop(session) })
                .detach();
            window.set_window_title(APP_TITLE);
        } else if !matches!(self.doc, DocState::Empty) {
            self.doc = DocState::Empty;
        }
        cx.notify();
    }

    fn remember_recent(&self, path: PathBuf, cx: &mut Context<'_, Self>) {
        let Some(storage) = self.options.recent_files.clone() else {
            return;
        };
        cx.background_executor()
            .spawn(async move {
                let mut recent = RecentFiles::load(storage, RecentFiles::DEFAULT_MAX);
                recent.add(&path);
                if let Err(e) = recent.save() {
                    log::warn!("cannot save the recent files list: {e}");
                }
            })
            .detach();
    }

    fn prompt_open(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: None,
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            let _ = this.update_in(cx, |this, window, cx| this.open_path(path, window, cx));
        })
        .detach();
    }

    /// Size and scale for a new session before its first layout.
    fn viewport_estimate(&self, window: &Window) -> ((f32, f32), f32) {
        let scale = window.scale_factor();
        match self.viewport_bounds {
            Some(b) => ((f32::from(b.size.width), f32::from(b.size.height)), scale),
            None => {
                let s = window.viewport_size();
                (
                    (
                        f32::from(s.width),
                        (f32::from(s.height) - toolbar::HEIGHT).max(1.0),
                    ),
                    scale,
                )
            }
        }
    }

    // ---- commands and input ----------------------------------------------

    pub(crate) fn run_command(
        &mut self,
        command: ReaderCommand,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        use ReaderCommand as C;
        // Toolbar clicks must not leave the keymap without a focused target.
        if !self.focus.is_focused(window) {
            window.focus(&self.focus, cx);
        }
        match command {
            C::OpenFile => self.prompt_open(window, cx),
            C::CloseDocument => self.close_document(window, cx),
            C::Quit => cx.quit(),
            C::ToggleFullscreen => window.toggle_fullscreen(),
            C::ToggleSidebar => self.sidebar_open = !self.sidebar_open,
            C::ToggleDevOverlay => self.overlay.visible = !self.overlay.visible,
            C::Find | C::FindNext | C::FindPrevious | C::Print => {
                log::info!("{command:?} is not available in this version");
            }
            C::ZoomIn
            | C::ZoomOut
            | C::ActualSize
            | C::FitPage
            | C::FitWidth
            | C::RotateClockwise
            | C::RotateCounterClockwise
            | C::NextPage
            | C::PreviousPage
            | C::FirstPage
            | C::LastPage
            | C::PageDown
            | C::PageUp
            | C::ScrollDown
            | C::ScrollUp => {
                if let Some(session) = self.session_mut() {
                    navigate(session, command);
                }
            }
        }
        cx.notify();
    }

    fn local_position(&self, position: gpui::Point<Pixels>) -> Option<(f32, f32)> {
        let b = self.viewport_bounds?;
        Some((
            f32::from(position.x - b.origin.x),
            f32::from(position.y - b.origin.y),
        ))
    }

    pub(crate) fn on_scroll_wheel(
        &mut self,
        event: &ScrollWheelEvent,
        _window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let anchor = self.local_position(event.position);
        let Some(session) = self.session_mut() else {
            return;
        };
        let delta = event.delta.pixel_delta(px(LINE_SCROLL_PX));
        let (dx, dy) = (f32::from(delta.x), f32::from(delta.y));
        if event.modifiers.control {
            // Wheel away from the user zooms in, around the cursor.
            let factor = 2f32.powf(dy / WHEEL_PX_PER_ZOOM_DOUBLING);
            session.set_zoom(session.zoom().scaled(factor), anchor);
        } else {
            // GPUI deltas move the content; the session scrolls the view.
            session.scroll_by(-dx, -dy);
        }
        cx.stop_propagation();
        cx.notify();
    }

    pub(crate) fn on_pinch(
        &mut self,
        event: &PinchEvent,
        _window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let anchor = self.local_position(event.position);
        let Some(session) = self.session_mut() else {
            return;
        };
        let factor = (1.0 + event.delta).clamp(0.5, 2.0);
        session.set_zoom(session.zoom().scaled(factor), anchor);
        cx.stop_propagation();
        cx.notify();
    }

    pub(crate) fn on_drop_paths(
        &mut self,
        paths: &ExternalPaths,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let paths = paths.paths();
        let chosen = paths
            .iter()
            .find(|p| is_pdf(p))
            .or_else(|| paths.first())
            .cloned();
        if let Some(path) = chosen {
            self.open_path(path, window, cx);
        }
    }
}

fn navigate(session: &mut DocumentSession<TileImage>, command: ReaderCommand) {
    use ReaderCommand as C;
    match command {
        C::ZoomIn => session.zoom_in(None),
        C::ZoomOut => session.zoom_out(None),
        C::ActualSize => session.actual_size(),
        C::FitPage => session.fit_page(),
        C::FitWidth => session.fit_width(),
        C::RotateClockwise => session.rotate_clockwise(),
        C::RotateCounterClockwise => session.rotate_counter_clockwise(),
        C::NextPage => session.next_page(),
        C::PreviousPage => session.prev_page(),
        C::FirstPage => session.first_page(),
        C::LastPage => session.last_page(),
        C::PageDown => session.page_down(),
        C::PageUp => session.page_up(),
        C::ScrollDown => session.scroll_by(0.0, LINE_SCROLL_PX * 2.0),
        C::ScrollUp => session.scroll_by(0.0, -LINE_SCROLL_PX * 2.0),
        _ => {}
    }
}

fn is_pdf(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("pdf"))
}

pub(crate) fn display_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

impl Render for ReaderView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        self.render_started = Some(Instant::now());
        self.theme = Theme::for_appearance(window.appearance());
        let theme = self.theme;
        let view = cx.entity();

        let root = div()
            .id("fastpdf")
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .flex_col()
            .font_family(UI_FONT)
            .text_color(theme.text)
            .bg(theme.canvas_bg)
            .child(toolbar::render(self, cx))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_1()
                    .min_h_0()
                    .when(self.sidebar_open, |row| {
                        row.child(
                            div()
                                .w(px(220.0))
                                .h_full()
                                .flex_none()
                                .bg(theme.sidebar_bg)
                                .border_r_1()
                                .border_color(theme.toolbar_border)
                                .p_3()
                                .text_size(px(13.0))
                                .text_color(theme.text_muted)
                                .child("Outline and thumbnails are not available yet."),
                        )
                    })
                    .child(self.render_document_area(view, cx)),
            );
        all_actions!(root, cx)
    }
}

impl ReaderView {
    fn render_document_area(
        &self,
        view: Entity<Self>,
        cx: &mut Context<'_, Self>,
    ) -> impl IntoElement {
        let theme = self.theme;
        let prepaint_view = view.clone();
        // The canvas is always present: it measures the document area and,
        // with a document open, paints it (crate::viewport).
        let document = canvas(
            move |bounds, window, cx| {
                prepaint_view.update(cx, |this, _| this.prepare_viewport(bounds, window))
            },
            move |bounds, frame, window, cx| {
                view.update(cx, |this, cx| {
                    this.paint_viewport(bounds, frame, window, cx)
                });
            },
        )
        .absolute()
        .size_full();

        div()
            .id("document")
            .relative()
            .flex_1()
            .min_w_0()
            .h_full()
            .overflow_hidden()
            .on_scroll_wheel(cx.listener(Self::on_scroll_wheel))
            .on_pinch(cx.listener(Self::on_pinch))
            .on_drop(cx.listener(Self::on_drop_paths))
            .drag_over::<ExternalPaths>(move |style, _, _, _| style.bg(theme.drop_highlight))
            .child(document)
            .when_some(self.placeholder(), |area, placeholder| {
                area.child(placeholder)
            })
    }

    /// Text shown instead of a document: the empty state, progress, errors.
    fn placeholder(&self) -> Option<impl IntoElement + use<>> {
        let theme = self.theme;
        let (title, detail, is_error): (String, String, bool) = match &self.doc {
            DocState::Open(_) => return None,
            DocState::Empty => (
                APP_TITLE.into(),
                "Open a PDF with Ctrl+O, or drop one onto this window.".into(),
                false,
            ),
            DocState::Loading { path } => (
                format!("Opening {}...", display_name(path)),
                String::new(),
                false,
            ),
            DocState::Failed { path, message } => (
                format!("Cannot open {}", display_name(path)),
                message.clone(),
                true,
            ),
        };
        Some(
            div()
                .absolute()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_2()
                .p_4()
                .child(div().text_size(px(22.0)).child(title))
                .when(!detail.is_empty(), |d| {
                    d.child(
                        div()
                            .max_w(px(640.0))
                            .text_size(px(14.0))
                            .text_color(if is_error {
                                theme.error_text
                            } else {
                                theme.text_muted
                            })
                            .child(detail),
                    )
                }),
        )
    }

    /// Page `page` as shown in the toolbar ("12 / 345").
    pub(crate) fn page_indicator(&self) -> Option<(PageIndex, u32)> {
        let session = self.session()?;
        Some((session.current_page(), session.page_count()))
    }
}
