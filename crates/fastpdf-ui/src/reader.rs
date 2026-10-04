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
use fastpdf_engine_api::{CancelToken, ColorMode, EngineDocument, PageIndex, PageRect, PdfEngine};
use fastpdf_search::TextCache;
use futures::StreamExt;
use futures::channel::mpsc;
use gpui::{
    App, AppContext, Bounds, ClickEvent, ClipboardItem, Context, Entity, ExternalPaths,
    FocusHandle, InteractiveElement, IntoElement, MouseButton, MouseDownEvent, ParentElement,
    PathPromptOptions, PinchEvent, Pixels, Render, ScrollWheelEvent, SharedString,
    StatefulInteractiveElement, Styled, Subscription, Task, TitlebarOptions, Window,
    WindowAppearance, WindowBounds, WindowHandle, WindowOptions, canvas, div,
    prelude::FluentBuilder, px, size,
};

use crate::actions::{KEY_CONTEXT, all_actions};
use crate::bench::{BenchEvent, BenchHook};
use crate::document::{OpenFailure, Opened, open_document_blocking};
use crate::find::{FindBar, SearchTarget};
use crate::i18n::{Language, Strings};
use crate::overlay::DevOverlay;
use crate::print::PrintPanel;
use crate::select::{PagePoint, TextSelection};
use crate::settings::{
    Appearance, DefaultZoom, MIN_WINDOW, Settings, SettingsStore, WindowPlacement,
};
use crate::sidebar::Sidebar;
use crate::smooth_scroll::SmoothScroll;
use crate::startup::{SessionParts, Startup, default_window_size};
use crate::textures::{DEFAULT_UPLOAD_BUDGET, TileImage, TileTextures};
use crate::theme::{ActiveTheme, Theme};
use crate::toolbar;

/// Scroll distance of one wheel "line" and of the arrow keys, in logical
/// pixels (three lines per wheel notch at the Windows default).
const LINE_SCROLL_PX: f32 = 33.0;
/// Ctrl + wheel: logical pixels of wheel travel that double the zoom.
const WHEEL_PX_PER_ZOOM_DOUBLING: f32 = 400.0;
/// Recent files listed in the empty window.
const RECENT_SHOWN: usize = 10;
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
    /// Development only: every print job writes into this file instead of
    /// reaching the printer (the app sets it from `FASTPDF_PRINT_TO_FILE`
    /// in debug builds or with the development overlay).
    pub print_to_file: Option<PathBuf>,
    /// Development only: a script of steps to run (`crate::devscript`).
    pub dev_script: Option<String>,
    /// Where UI settings (appearance, night mode, sidebar, default zoom,
    /// window placement) are kept; `None`: defaults, nothing is written.
    pub settings_file: Option<PathBuf>,
    /// The Windows UI language (`GetUserDefaultUILanguage`, a LANGID), which
    /// picks the UI text unless the settings name a language.
    pub system_ui_language: Option<u16>,
}

impl std::fmt::Debug for ReaderOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReaderOptions")
            .field("engine", &self.engine.info().name)
            .field("session", &self.session)
            .field("upload_budget", &self.upload_budget)
            .field("dev_overlay", &self.dev_overlay)
            .field("print_to_file", &self.print_to_file)
            .field("dev_script", &self.dev_script.is_some())
            .field("settings_file", &self.settings_file)
            .field("system_ui_language", &self.system_ui_language)
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
            print_to_file: None,
            dev_script: None,
            settings_file: crate::settings::default_location(),
            system_ui_language: None,
        }
    }
}

/// Opens the main window with what `startup` prepared (settings, the
/// command-line document, maybe its first page).
pub fn open_reader_window(
    cx: &mut App,
    options: ReaderOptions,
    startup: Startup,
) -> gpui::Result<WindowHandle<ReaderView>> {
    let bench = options.bench.clone();
    // The window opens where it was left, in the chosen appearance.
    apply_window_appearance(startup.settings.appearance, cx);
    let window_bounds = initial_window_bounds(startup.settings.window, cx);
    let handle = cx.open_window(
        WindowOptions {
            window_bounds: Some(window_bounds),
            titlebar: Some(TitlebarOptions {
                title: Some(APP_TITLE.into()),
                ..Default::default()
            }),
            window_min_size: Some(size(px(MIN_WINDOW.0), px(MIN_WINDOW.1))),
            app_id: Some(APP_TITLE.into()),
            ..Default::default()
        },
        move |window, cx| cx.new(|cx| ReaderView::new(options, startup, window, cx)),
    )?;
    if let Some(hook) = bench {
        hook(BenchEvent::WindowVisible);
    }
    Ok(handle)
}

/// How long the window must stay put before its placement is saved.
const PLACEMENT_SETTLE: std::time::Duration = std::time::Duration::from_millis(600);
/// Part of a restored window's top edge that must lie on a display, so the
/// title bar can still be grabbed (a monitor may have been unplugged).
const GRIP: (f32, f32) = (120.0, 24.0);

/// Asks the platform to draw native window chrome in the chosen appearance.
/// GPUI implements this on macOS only; on Windows (this GPUI revision) the
/// title bar keeps following the system, while everything FastPDF paints
/// follows the setting.
pub(crate) fn apply_window_appearance(appearance: Appearance, cx: &mut App) {
    cx.set_window_appearance(match appearance {
        Appearance::System => None,
        Appearance::Light => Some(WindowAppearance::Light),
        Appearance::Dark => Some(WindowAppearance::Dark),
    });
}

/// The saved placement when it is still on a display, else a default
/// window centered on the primary display.
fn initial_window_bounds(saved: Option<WindowPlacement>, cx: &App) -> WindowBounds {
    if let Some(p) = saved.filter(WindowPlacement::is_plausible) {
        let bounds = Bounds::new(
            gpui::point(px(p.x), px(p.y)),
            size(px(p.width), px(p.height)),
        );
        let displays: Vec<Bounds<Pixels>> = cx.displays().iter().map(|d| d.bounds()).collect();
        if grip_visible(bounds, &displays) {
            return if p.maximized {
                WindowBounds::Maximized(bounds)
            } else {
                WindowBounds::Windowed(bounds)
            };
        }
        log::info!("the saved window placement is off screen; using the default");
    }
    WindowBounds::Windowed(Bounds::centered(None, default_window_size_on(cx), cx))
}

/// Whether enough of the window's top edge is on one of `displays`.
fn grip_visible(window: Bounds<Pixels>, displays: &[Bounds<Pixels>]) -> bool {
    let top = Bounds::new(window.origin, size(window.size.width, px(GRIP.1)));
    displays.iter().any(|d| {
        let seen = d.intersect(&top);
        seen.size.width >= px(GRIP.0) && seen.size.height >= px(GRIP.1)
    })
}

/// What to save for the window's current bounds (the restore bounds when
/// maximized; full screen is saved as its windowed bounds).
fn placement_of(bounds: WindowBounds) -> WindowPlacement {
    let b = bounds.get_bounds();
    WindowPlacement {
        x: f32::from(b.origin.x),
        y: f32::from(b.origin.y),
        width: f32::from(b.size.width),
        height: f32::from(b.size.height),
        maximized: matches!(bounds, WindowBounds::Maximized(_)),
    }
}

/// A portrait window that fits the primary display (the same size
/// `crate::startup` predicts).
fn default_window_size_on(cx: &App) -> gpui::Size<Pixels> {
    let Some(display) = cx.primary_display() else {
        return size(px(1024.0), px(768.0));
    };
    let screen = display.bounds().size;
    let (width, height) = default_window_size(f32::from(screen.width), f32::from(screen.height));
    size(px(width), px(height))
}

/// Wakes the UI thread from any thread; see the crate docs.
#[derive(Clone, Debug)]
pub(crate) struct Waker(pub(crate) mpsc::UnboundedSender<()>);

impl Waker {
    pub(crate) fn wake(&self) {
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
    Failed { path: PathBuf, failure: OpenFailure },
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
    pub(crate) sidebar: Sidebar,
    pub(crate) find: FindBar,
    pub(crate) print: PrintPanel,
    pub(crate) selection: TextSelection,
    /// Extracted text for search and selection (budgeted, shared).
    pub(crate) texts: Arc<TextCache>,
    /// Recently opened files, most recent first (empty window).
    pub(crate) recent: Vec<PathBuf>,
    /// Bounds of the document area from the last layout.
    pub(crate) viewport_bounds: Option<Bounds<Pixels>>,
    pub(crate) render_started: Option<Instant>,
    /// Incremented by every render (the root view renders once per frame);
    /// identifies the frame for the texture upload budget.
    pub(crate) frame_seq: u64,
    pub(crate) first_paint_reported: bool,
    pub(crate) waker: Waker,
    pub(crate) memory: MemoryMonitor,
    /// UI settings, saved whenever they change (`crate::settings`).
    pub(crate) settings: Settings,
    settings_store: SettingsStore,
    /// The settings panel is shown.
    pub(crate) settings_open: bool,
    /// Window bounds when the window opened: a window the user never moved
    /// or resized does not overwrite the saved placement.
    initial_bounds: WindowBounds,
    /// Saves the placement once the window stops moving (debounce).
    bounds_task: Option<Task<()>>,
    /// The Windows UI language (the settings may override it).
    system_language: Language,
    /// Language the text fields' placeholders were last set in.
    placeholders: Option<Language>,
    /// Mouse-wheel notches in flight (`crate::smooth_scroll`).
    pub(crate) smooth_scroll: SmoothScroll,
    open_task: Option<Task<()>>,
    pub(crate) dev_script: Option<Task<()>>,
    _wake_task: Task<()>,
    _appearance: Subscription,
    _bounds: Subscription,
}

impl std::fmt::Debug for ReaderView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReaderView")
            .field("textures", &self.textures)
            .field("find", &self.find)
            .finish_non_exhaustive()
    }
}

impl ReaderView {
    fn new(
        options: ReaderOptions,
        startup: Startup,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Self {
        let Startup {
            settings,
            pending,
            wake: waker,
            wake_rx: mut rx,
            retire,
        } = startup;
        let focus = cx.focus_handle();
        window.focus(&focus, cx);

        // Settings are saved as they change, the window placement shortly
        // after the window stops moving: nothing depends on how the process
        // ends (close button, Ctrl+Q, logoff, a crash).
        let initial_bounds = window.window_bounds();
        let bounds = cx.observe_window_bounds(window, |this, window, cx| {
            this.window_moved(window.window_bounds(), cx);
        });
        let settings_store = SettingsStore::new(options.settings_file.clone(), &settings);

        // Render workers, searches and cache evictions wake the UI through
        // this channel (created before the window, see `crate::startup`): a
        // foreground task awaits it and notifies the view. No polling, no
        // timers (spec §1: idle CPU ~ 0).
        let wake_task = cx.spawn(async move |this, cx| {
            while rx.next().await.is_some() {
                // Coalesce a burst of wake-ups into one update.
                while rx.try_recv().is_ok() {}
                if this.update(cx, |view, cx| view.on_wake(cx)).is_err() {
                    break;
                }
            }
        });
        let appearance = cx.observe_window_appearance(window, |_, _, cx| cx.notify());

        let memory = MemoryMonitor::new(Arc::new(MemoryBudgetManager::new(options.memory)));
        let texts = Arc::new(TextCache::new(TextCache::DEFAULT_BUDGET));
        memory.manager().register(texts.budgeted());
        let system_language = options
            .system_ui_language
            .map(Language::from_langid)
            .unwrap_or_default();
        let mut view = Self {
            textures: TileTextures::with_retire_queue(options.upload_budget, retire),
            overlay: DevOverlay::new(options.dev_overlay),
            find: FindBar::new(cx),
            print: PrintPanel::new(options.print_to_file.clone(), cx),
            options,
            focus,
            doc: DocState::Empty,
            theme: Theme::resolve(settings.appearance, window.appearance()),
            sidebar: Sidebar {
                open: settings.sidebar_open,
                tab: settings.sidebar_tab,
                ..Sidebar::default()
            },
            selection: TextSelection::default(),
            texts,
            recent: Vec::new(),
            viewport_bounds: None,
            render_started: None,
            frame_seq: 0,
            first_paint_reported: false,
            waker,
            memory,
            settings,
            settings_store,
            settings_open: false,
            initial_bounds,
            bounds_task: None,
            system_language,
            placeholders: None,
            smooth_scroll: SmoothScroll::default(),
            open_task: None,
            dev_script: None,
            _wake_task: wake_task,
            _appearance: appearance,
            _bounds: bounds,
        };
        cx.set_global(ActiveTheme(view.theme));
        view.apply_language(cx);
        if let Some(mut pending) = pending {
            // Usually finished long before the window: adopt it now, so the
            // very first frame shows the document (and its early tiles).
            match pending.result.try_recv() {
                Ok(Some(result)) => {
                    view.doc = DocState::Loading {
                        path: pending.path.clone(),
                    };
                    view.finish_open(pending.path, result, window, cx);
                }
                Ok(None) => {
                    let result = pending.result;
                    let result = async move { result.await.unwrap_or(Err(OpenFailure::Abandoned)) };
                    view.begin_open(pending.path, result, window, cx);
                }
                Err(_) => {
                    view.doc = DocState::Loading {
                        path: pending.path.clone(),
                    };
                    view.finish_open(pending.path, Err(OpenFailure::Abandoned), window, cx);
                }
            }
        } else {
            view.load_recent(cx);
        }
        if let Some(script) = view.options.dev_script.clone() {
            match crate::devscript::parse(&script) {
                Ok(steps) => view.start_dev_script(steps, window, cx),
                Err(e) => log::warn!("dev script ignored: {e}"),
            }
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

    pub(crate) fn session_mut(&mut self) -> Option<&mut DocumentSession<TileImage>> {
        match &mut self.doc {
            DocState::Open(open) => Some(&mut open.session),
            _ => None,
        }
    }

    /// Something finished in the background (tiles, evictions, search).
    fn on_wake(&mut self, cx: &mut Context<'_, Self>) {
        self.report(BenchEvent::Wake);
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
        if self.find.drain() {
            self.on_new_hits();
        }
        self.print.drain();
        cx.notify();
    }

    // ---- opening and closing ---------------------------------------------

    /// Opens `path`, replacing the current document.
    pub fn open_path(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<'_, Self>) {
        let path = std::path::absolute(&path).unwrap_or(path);
        let engine = Arc::clone(&self.options.engine);
        let open_path = path.clone();
        let task = cx.background_executor().spawn(async move {
            open_document_blocking(engine.as_ref(), &open_path).map(Opened::from)
        });
        self.begin_open(path, task, window, cx);
    }

    fn begin_open(
        &mut self,
        path: PathBuf,
        result: impl Future<Output = Result<Opened, OpenFailure>> + 'static,
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
        result: Result<Opened, OpenFailure>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        // A newer open replaced this one.
        if !matches!(&self.doc, DocState::Loading { path: p } if *p == path) {
            return;
        }
        let Opened {
            doc: opened,
            session,
        } = match result {
            Ok(opened) => opened,
            Err(failure) => {
                log::warn!("cannot open {}: {failure}", path.display());
                self.doc = DocState::Failed { failure, path };
                window.set_window_title(APP_TITLE);
                cx.notify();
                return;
            }
        };

        let session = match session {
            // Started while GPUI was starting; the first frame resizes it to
            // the real document area if the guess was off. Night mode or the
            // default zoom may have changed while the document was opening:
            // nothing of it has been shown yet, so apply the current ones.
            Some(mut session) => {
                session.set_color_mode(self.color_mode());
                apply_default_zoom(&mut session, self.settings.default_zoom);
                session
            }
            None => {
                let (view_size, scale) = self.viewport_estimate(window);
                self.session_parts().session(&opened, view_size, scale)
            }
        };
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
        // Panels that were open carry over to the new document.
        if self.sidebar.open {
            self.ensure_outline(cx);
            self.sync_thumbnails(window);
        }
        if self.find.open {
            let query = self.find.input.read(cx).text().to_string();
            self.on_find_query(query, cx);
        }
        cx.notify();
    }

    /// Closes the current document and frees its GPU textures, its cached
    /// text and (once a running print job has stopped) the engine document.
    fn close_document(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.find.reset(cx);
        self.selection.reset();
        self.sidebar.reset_document();
        self.smooth_scroll.stop();
        // A print job keeps the document alive until it stops.
        self.print.cancel_job();
        if let DocState::Open(open) = mem::replace(&mut self.doc, DocState::Empty) {
            let OpenDoc {
                mut session, path, ..
            } = *open;
            log::info!("closing {}", path.display());
            // The text cache is shared by all documents (spec §15).
            self.texts.remove_document(session.id());
            // Cancel rendering and evict every tile and thumbnail (they
            // reach the retire queue), then release every texture.
            session.close();
            self.textures.release_all(window);
            // Dropping the session joins its render workers, which may wait
            // for a page that is still rasterizing: keep that off the UI thread.
            // A text extraction that was running when the document closed
            // (search thread, selection) may still have added its page;
            // sweep once more after the workers are gone.
            let document = session.id();
            let texts = Arc::clone(&self.texts);
            cx.background_executor()
                .spawn(async move {
                    drop(session);
                    texts.remove_document(document);
                })
                .detach();
            window.set_window_title(APP_TITLE);
            self.load_recent(cx);
        } else if !matches!(self.doc, DocState::Empty) {
            self.doc = DocState::Empty;
        }
        cx.notify();
    }

    fn remember_recent(&mut self, path: PathBuf, cx: &mut Context<'_, Self>) {
        let Some(storage) = self.options.recent_files.clone() else {
            return;
        };
        self.recent.retain(|p| !same_file(p, &path));
        self.recent.insert(0, path.clone());
        self.recent.truncate(RECENT_SHOWN);
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

    /// Reads the recent-files list in the background (empty window).
    fn load_recent(&mut self, cx: &mut Context<'_, Self>) {
        let Some(storage) = self.options.recent_files.clone() else {
            return;
        };
        let read = cx.background_executor().spawn(async move {
            RecentFiles::load(storage, RecentFiles::DEFAULT_MAX)
                .entries()
                .iter()
                .take(RECENT_SHOWN)
                .cloned()
                .collect::<Vec<_>>()
        });
        cx.spawn(async move |this, cx| {
            let recent = read.await;
            let _ = this.update(cx, |this, cx| {
                this.recent = recent;
                cx.notify();
            });
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

    // ---- commands -----------------------------------------------------------

    pub(crate) fn run_command(
        &mut self,
        command: ReaderCommand,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        use ReaderCommand as C;
        // Toolbar clicks must not leave the keymap without a focused target;
        // a focused find field keeps its focus.
        if !self.focus.contains_focused(window, cx) {
            window.focus(&self.focus, cx);
        }
        match command {
            C::OpenFile => self.prompt_open(window, cx),
            C::CloseDocument => self.close_document(window, cx),
            C::Quit => cx.quit(),
            C::ToggleFullscreen => window.toggle_fullscreen(),
            C::ToggleSidebar => self.toggle_sidebar(window, cx),
            C::ToggleDevOverlay => self.overlay.visible = !self.overlay.visible,
            C::ToggleNightMode => self.toggle_night_mode(cx),
            C::Find => self.open_find(window, cx),
            C::FindNext => self.find_step(true, window, cx),
            C::FindPrevious => self.find_step(false, window, cx),
            C::Copy => self.copy_selection(cx),
            C::SelectAll => self.select_current_page(cx),
            C::Cancel => self.cancel(window, cx),
            C::Print => self.toggle_print_panel(window, cx),
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

    /// Esc: stops printing or closes the print panel, then the settings
    /// panel, then the find bar, then clears the selection.
    fn cancel(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if self.print.printing() {
            self.print.cancel_job();
        } else if self.print.open {
            self.close_print_panel(window, cx);
        } else if self.settings_open {
            self.settings_open = false;
        } else if self.find.open {
            self.close_find(window, cx);
        } else {
            self.selection.clear();
        }
    }

    // ---- find (spec §22) -----------------------------------------------------

    fn open_find(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.find.open = true;
        let input = self.find.input.clone();
        input.update(cx, |input, cx| {
            input.select_everything(cx);
            window.focus(input.focus_handle(), cx);
        });
    }

    pub(crate) fn close_find(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.find.open = false;
        self.find.reset(cx);
        self.find.query.clear();
        window.focus(&self.focus, cx);
        cx.notify();
    }

    /// The find field's committed text changed: search again.
    pub(crate) fn on_find_query(&mut self, text: String, cx: &mut Context<'_, Self>) {
        let target = self.session().map(|s| SearchTarget {
            doc: Arc::clone(s.document()),
            document: s.id(),
            start: s.current_page(),
        });
        let Some(target) = target else {
            self.find.reset(cx);
            self.find.query = text.trim().to_string();
            cx.notify();
            return;
        };
        let waker = self.waker.clone();
        let texts = Arc::clone(&self.texts);
        self.find
            .start(text, target, &texts, move || waker.wake(), cx);
        cx.notify();
    }

    /// The first hit after the start page becomes active and is shown.
    fn on_new_hits(&mut self) {
        let start = self
            .session()
            .map_or(PageIndex::FIRST, |s| s.current_page());
        if self.find.hits.activate_from(start) {
            self.reveal_active_hit();
        }
    }

    pub(crate) fn find_step(
        &mut self,
        forward: bool,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        if !self.find.open {
            self.open_find(window, cx);
            return;
        }
        if self.find.hits.step(forward).is_some() {
            self.reveal_active_hit();
        }
        cx.notify();
    }

    fn reveal_active_hit(&mut self) {
        let Some((page, rect)) = self
            .find
            .hits
            .active()
            .and_then(|h| Some((h.page, *h.rects.first()?)))
        else {
            return;
        };
        if let Some(session) = self.session_mut() {
            reveal(session, page, rect);
        }
    }

    // ---- text selection --------------------------------------------------------

    fn copy_selection(&mut self, cx: &mut Context<'_, Self>) {
        if self.selection.is_empty() {
            return;
        }
        let text = self.selection.text();
        if !text.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    fn select_current_page(&mut self, cx: &mut Context<'_, Self>) {
        let Some(page) = self.session().map(|s| s.current_page()) else {
            return;
        };
        let missing = self.selection.select_page(page);
        self.fetch_text_layers(missing, cx);
    }

    /// Fetches text layers in the background through the shared cache.
    pub(crate) fn fetch_text_layers(&mut self, pages: Vec<PageIndex>, cx: &mut Context<'_, Self>) {
        let Some(session) = self.session() else {
            return;
        };
        for page in pages {
            let doc = Arc::clone(session.document());
            let document = session.id();
            let texts = Arc::clone(&self.texts);
            let work = cx.background_executor().spawn(async move {
                texts.get_or_extract(document, doc.as_ref(), page, &CancelToken::new())
            });
            cx.spawn(async move |this, cx| {
                let result = work.await;
                let _ = this.update(cx, |this, cx| {
                    // Ignore layers of a document that was closed meanwhile.
                    if this.session().map(|s| s.id()) != Some(document) {
                        return;
                    }
                    if let Err(e) = &result {
                        log::debug!("no text for page {}: {e}", page.display_number());
                    }
                    this.selection.layer_loaded(page, result.ok());
                    cx.notify();
                });
            })
            .detach();
        }
    }

    pub(crate) fn on_document_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        window.focus(&self.focus, cx);
        match self.page_point(event.position) {
            Some(point) => {
                let missing = self.selection.begin(point);
                self.fetch_text_layers(missing, cx);
            }
            None => self.selection.clear(),
        }
        cx.notify();
    }

    /// Drag update from the canvas' window-wide listener.
    pub(crate) fn on_selection_drag(
        &mut self,
        position: gpui::Point<Pixels>,
        cx: &mut Context<'_, Self>,
    ) {
        if let Some(point) = self.page_point(position) {
            let missing = self.selection.extend(point);
            self.fetch_text_layers(missing, cx);
            cx.notify();
        }
    }

    pub(crate) fn on_selection_end(&mut self, cx: &mut Context<'_, Self>) {
        self.selection.end();
        if self.selection.anchor == self.selection.focus {
            // A click, not a drag.
            self.selection.clear();
        }
        cx.notify();
    }

    /// The page-space point under a window position.
    fn page_point(&self, position: gpui::Point<Pixels>) -> Option<PagePoint> {
        let (x, y) = self.local_position(position)?;
        let (page, px, py) = self.session()?.view_to_page(x, y)?;
        Some(PagePoint { page, x: px, y: py })
    }

    // ---- input -------------------------------------------------------------------

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
        let smooth = self.settings.smooth_scrolling
            && !event.modifiers.control
            && crate::smooth_scroll::is_wheel_notch(&event.delta);
        let delta = event.delta.pixel_delta(px(LINE_SCROLL_PX));
        let (dx, dy) = (f32::from(delta.x), f32::from(delta.y));
        let Some(session) = self.session_mut() else {
            return;
        };
        if event.modifiers.control {
            // Wheel away from the user zooms in, around the cursor.
            let factor = 2f32.powf(dy / WHEEL_PX_PER_ZOOM_DOUBLING);
            session.set_zoom(session.zoom().scaled(factor), anchor);
        } else if smooth {
            // Animated by the viewport's prepaint (`crate::smooth_scroll`).
            self.smooth_scroll.add((-dx, -dy), Instant::now());
        } else {
            // GPUI deltas move the content; the session scrolls the view.
            session.scroll_by(-dx, -dy);
        }
        cx.stop_propagation();
        cx.notify();
    }

    /// A wheel turn of `notches` (positive: down) as a mouse sends it, at
    /// the Windows default of three lines per notch (dev scripts).
    pub(crate) fn wheel_notches(
        &mut self,
        notches: i32,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let position = self
            .viewport_bounds
            .map_or_else(gpui::Point::default, |b| b.center());
        let event = ScrollWheelEvent {
            position,
            delta: gpui::ScrollDelta::Lines(gpui::point(0.0, -3.0 * notches as f32)),
            ..ScrollWheelEvent::default()
        };
        self.on_scroll_wheel(&event, window, cx);
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

/// Puts a session in the zoom new documents start with.
pub(crate) fn apply_default_zoom(session: &mut DocumentSession<TileImage>, zoom: DefaultZoom) {
    match zoom {
        DefaultZoom::FitWidth => session.fit_width(),
        DefaultZoom::FitPage => session.fit_page(),
        DefaultZoom::ActualSize => session.actual_size(),
    }
}

impl ReaderView {
    /// The UI language: the settings' choice, else the Windows UI language.
    pub(crate) fn language(&self) -> Language {
        self.settings.language.resolve(self.system_language)
    }

    pub(crate) fn strings(&self) -> &'static Strings {
        self.language().strings()
    }

    /// Text that lives outside the views' render (text field placeholders)
    /// follows a language change here.
    pub(crate) fn apply_language(&mut self, cx: &mut Context<'_, Self>) {
        let language = self.language();
        if self.placeholders == Some(language) {
            return;
        }
        self.placeholders = Some(language);
        let strings = language.strings();
        self.find.input.update(cx, |input, cx| {
            input.set_placeholder(strings.find_placeholder, cx)
        });
        self.print.range.update(cx, |input, cx| {
            input.set_placeholder(strings.range_placeholder, cx)
        });
    }

    /// Everything a new session needs besides the document.
    fn session_parts(&self) -> SessionParts {
        SessionParts {
            config: self.options.session.clone(),
            color: self.color_mode(),
            zoom: self.settings.default_zoom,
            wake: self.waker.clone(),
            retire: self.textures.retire_queue(),
        }
    }

    /// Page colors for the night mode setting.
    pub(crate) fn color_mode(&self) -> ColorMode {
        if self.settings.night_mode {
            ColorMode::Inverted
        } else {
            ColorMode::Normal
        }
    }

    /// Saves changed settings in the background.
    pub(crate) fn save_settings(&mut self, cx: &mut Context<'_, Self>) {
        let executor = cx.background_executor().clone();
        self.settings_store.save(&self.settings, &executor);
    }

    /// The window moved, was resized, maximized or restored: remember the
    /// placement once it has been still for a moment. A window that never
    /// left its initial (default) placement writes nothing.
    fn window_moved(&mut self, bounds: WindowBounds, cx: &mut Context<'_, Self>) {
        if bounds == self.initial_bounds && self.settings.window.is_none() {
            self.bounds_task = None;
            return;
        }
        // Replacing the task cancels the previous wait.
        self.bounds_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(PLACEMENT_SETTLE).await;
            let _ = this.update(cx, |this, cx| {
                this.settings.window = Some(placement_of(bounds));
                this.save_settings(cx);
            });
        }));
    }

    pub(crate) fn settings_path(&self) -> Option<&Path> {
        self.settings_store.path()
    }
}

/// Scrolls so `rect` on `page` is visible, about a third from the top when
/// the view has to move.
fn reveal(session: &mut DocumentSession<TileImage>, page: PageIndex, rect: PageRect) {
    let vp = *session.viewport();
    let fits = |r: [f32; 4]| {
        r[0] >= 0.0 && r[1] >= 0.0 && r[0] + r[2] <= vp.width && r[1] + r[3] <= vp.height
    };
    if session.page_to_view(page, rect).is_some_and(fits) {
        return;
    }
    // Makes the page's size known and puts its top at the top.
    session.go_to_page(page);
    if let Some(r) = session.page_to_view(page, rect) {
        let dx = if r[0] < 0.0 || r[0] + r[2] > vp.width {
            r[0] + r[2] / 2.0 - vp.width / 2.0
        } else {
            0.0
        };
        session.scroll_by(dx, r[1] - vp.height / 3.0);
    }
}

fn is_pdf(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("pdf"))
}

/// Windows paths are case-insensitive.
fn same_file(a: &Path, b: &Path) -> bool {
    a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
}

pub(crate) fn display_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

impl Render for ReaderView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        self.report(BenchEvent::Render);
        self.render_started = Some(Instant::now());
        self.frame_seq = self.frame_seq.wrapping_add(1);
        // Every frame: a system theme switch arrives as a window appearance
        // change (observed in `new`), a settings change as a notify.
        let theme = Theme::resolve(self.settings.appearance, window.appearance());
        if theme != self.theme {
            self.theme = theme;
            cx.set_global(ActiveTheme(theme));
        }
        self.apply_language(cx);
        let view = cx.entity();

        let root = div()
            .id("fastpdf")
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .flex_col()
            .font_family(self.language().ui_font())
            .text_color(theme.text)
            .bg(theme.canvas_bg)
            .child(toolbar::render(self, cx))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_1()
                    .min_h_0()
                    .when(self.sidebar.open, |row| {
                        row.child(self.render_sidebar(view.clone(), cx))
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
            move |bounds, state, window, cx| {
                let dragging = view.update(cx, |this, cx| {
                    this.paint_viewport(bounds, state, window, cx);
                    this.selection.dragging
                });
                if dragging {
                    crate::viewport::track_selection_drag(view, window);
                }
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
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_document_mouse_down))
            .on_drop(cx.listener(Self::on_drop_paths))
            .drag_over::<ExternalPaths>(move |style, _, _, _| style.bg(theme.drop_highlight))
            .child(document)
            .when_some(self.placeholder(cx), |area, placeholder| {
                area.child(placeholder)
            })
            .when_some(self.render_host_notice(), |area, notice| area.child(notice))
            .when(self.find.open, |area| area.child(self.render_find_bar(cx)))
            .when(self.print.open, |area| {
                area.child(self.render_print_panel(cx))
            })
            .when(self.settings_open, |area| {
                area.child(self.render_settings_panel(cx))
            })
    }

    /// Shown over the document once its render host stopped restarting
    /// after a crash storm (ADR 0008 §2): every page fails from then on,
    /// and the pages alone would not say why.
    fn render_host_notice(&self) -> Option<gpui::Div> {
        let DocState::Open(doc) = &self.doc else {
            return None;
        };
        let status = doc.session.document().host_status()?;
        if !status.restarts_disabled {
            return None;
        }
        let theme = self.theme;
        Some(
            div()
                .absolute()
                .bottom(px(8.0))
                .left(px(8.0))
                .max_w(px(520.0))
                .p_2()
                .rounded(px(6.0))
                .bg(theme.toolbar_bg)
                .border_1()
                .border_color(theme.toolbar_border)
                .text_size(px(13.0))
                .text_color(theme.error_text)
                .child(self.strings().rendering_stopped(status.crashes)),
        )
    }

    /// Text shown instead of a document: the empty state with recent files,
    /// progress, errors.
    fn placeholder(&self, cx: &mut Context<'_, Self>) -> Option<impl IntoElement + use<>> {
        let theme = self.theme;
        let strings = self.strings();
        let (title, detail, is_error): (String, String, bool) = match &self.doc {
            DocState::Open(_) => return None,
            DocState::Empty => (APP_TITLE.into(), strings.empty_hint.into(), false),
            DocState::Loading { path } => {
                (strings.opening(&display_name(path)), String::new(), false)
            }
            DocState::Failed { path, failure } => (
                strings.cannot_open(&display_name(path)),
                strings.open_failure(failure),
                true,
            ),
        };
        let recent = if matches!(self.doc, DocState::Empty) {
            self.render_recent(cx)
        } else {
            None
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
                })
                .when_some(recent, |d, recent| d.child(recent)),
        )
    }

    /// Clickable recent files (spec §8 "Open Recent").
    fn render_recent(&self, cx: &mut Context<'_, Self>) -> Option<gpui::Div> {
        if self.recent.is_empty() {
            return None;
        }
        let theme = self.theme;
        let rows: Vec<_> = self
            .recent
            .iter()
            .enumerate()
            .map(|(i, path)| {
                let name = display_name(path);
                let dir = path
                    .parent()
                    .map(|d| d.display().to_string())
                    .unwrap_or_default();
                let open = path.clone();
                div()
                    .id(("recent", i))
                    .flex()
                    .flex_col()
                    .px_3()
                    .py_1()
                    .rounded(px(4.0))
                    .cursor_pointer()
                    .hover(move |s| s.bg(theme.button_hover))
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.open_path(open.clone(), window, cx);
                    }))
                    .child(div().text_size(px(14.0)).child(name))
                    .child(
                        div()
                            .text_size(px(12.0))
                            .text_color(theme.text_muted)
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .child(dir),
                    )
            })
            .collect();
        Some(
            div()
                .mt_4()
                .w(px(480.0))
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .px_3()
                        .text_size(px(13.0))
                        .text_color(theme.text_muted)
                        .child(self.strings().recent),
                )
                .children(rows),
        )
    }

    /// Page `page` as shown in the toolbar ("12 / 345").
    pub(crate) fn page_indicator(&self) -> Option<(PageIndex, u32)> {
        let session = self.session()?;
        Some((session.current_page(), session.page_count()))
    }
}
