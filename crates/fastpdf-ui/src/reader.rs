//! The reader window's root view: document state and commands; pointer
//! input is handled in `crate::input`, moving the view in
//! `crate::navigation`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use fastpdf_cache::{BudgetConfig, MemoryBudgetManager};
use fastpdf_core::keymap::ReaderCommand;
use fastpdf_core::memory::MemoryMonitor;
use fastpdf_core::recent::RecentFiles;
use fastpdf_core::{DocumentSession, Jump, SessionConfig};
use fastpdf_engine_api::write::{WriteError, WriteJob, WriteReport};
use fastpdf_engine_api::{CancelToken, ColorMode, PageIndex, PdfEngine};
use fastpdf_search::TextCache;
use futures::StreamExt;
use futures::channel::mpsc;
use gpui::{
    App, AppContext, Bounds, ClickEvent, ClipboardItem, Context, Entity, ExternalPaths,
    FocusHandle, InteractiveElement, IntoElement, MouseButton, MouseDownEvent, ParentElement,
    Pixels, Render, SharedString, StatefulInteractiveElement, Styled, Subscription, Task,
    TitlebarOptions, Window, WindowBounds, WindowHandle, WindowOptions, canvas, div,
    prelude::FluentBuilder, px, size,
};

use crate::actions::{KEY_CONTEXT, all_actions};
use crate::bench::{BenchEvent, BenchHook};
use crate::document::OpenFailure;
use crate::find::{FindBar, SearchTarget};
use crate::find_options::FindOption;
use crate::i18n::{Language, Strings};
use crate::navigation::{navigate, reveal};
use crate::opening::APP_TITLE;
use crate::overlay::DevOverlay;
use crate::password::LockedDoc;
use crate::print::PrintPanel;
use crate::scrollbar::ScrollBarState;
use crate::select::TextSelection;
use crate::settings::{MIN_WINDOW, PageLayout, Settings, SettingsStore};
use crate::sidebar::Sidebar;
use crate::signing::SignaturePanel;
use crate::smooth_scroll::{PageTurn, SmoothScroll};
use crate::stamps::StampPanel;
use crate::startup::{SessionParts, Startup};
use crate::text_edits::TextEditPanel;
use crate::textures::{DEFAULT_UPLOAD_BUDGET, TileImage, TileTextures};
use crate::theme::{ActiveTheme, Theme};
use crate::toolbar;
use crate::tools::ToolsPanel;
use crate::window_placement::{apply_window_appearance, initial_window_bounds};

/// Runs a write job (ADR 0013) to its end on the calling thread; the app
/// runs it in an isolated child process. The reader calls it from a
/// background thread; cancelling the token ends the job.
pub type WriteRunner =
    Arc<dyn Fn(&WriteJob, &CancelToken) -> Result<WriteReport, WriteError> + Send + Sync>;

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
    /// Writes new PDFs (save a copy, page tools, stamps); `None`: the
    /// writing commands report that writing is not available.
    pub writer: Option<WriteRunner>,
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
            .field("writer", &self.writer.is_some())
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
            writer: None,
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
    /// Opened with a password: never written (ADR 0014).
    pub protected: bool,
    /// Page labels and other answers loaded after opening.
    pub queries: crate::doc_queries::DocQueries,
}

/// A toolbar drop-down menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Menu {
    /// Page layout.
    Layout,
    /// Save a copy, page tools.
    Edit,
}

/// The toolbar's drop-down menus; at most one is open.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct MenuState {
    pub open: Option<Menu>,
    /// The menu that a press outside it closed, and where: a press on the
    /// menu's own button closes the menu, and the click that press ends must
    /// not open it again.
    pub dismissed_at: Option<(Menu, gpui::Point<Pixels>)>,
}

impl MenuState {
    pub(crate) fn is_open(&self, menu: Menu) -> bool {
        self.open == Some(menu)
    }
}

pub(crate) enum DocState {
    Empty,
    Loading { path: PathBuf },
    Failed { path: PathBuf, failure: OpenFailure },
    // Encrypted: the prompt asks for the password (`crate::password`).
    Locked(Box<LockedDoc>),
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
    pub(crate) initial_bounds: WindowBounds,
    /// Saves the placement once the window stops moving (debounce).
    pub(crate) bounds_task: Option<Task<()>>,
    /// The Windows UI language (the settings may override it).
    system_language: Language,
    /// Language the text fields' placeholders were last set in.
    placeholders: Option<Language>,
    /// Mouse-wheel notches in flight (`crate::smooth_scroll`).
    pub(crate) smooth_scroll: SmoothScroll,
    /// Page mode: wheel travel past the edge of the shown row.
    pub(crate) page_turn: PageTurn,
    /// The document area's scroll bars (`crate::scrollbar`).
    pub(crate) scrollbars: ScrollBarState,
    pub(crate) menus: MenuState,
    /// The toolbar's page field (go to a page by number or label).
    pub(crate) go_to: crate::go_to_page::GoToPage,
    /// Save a copy, the page tools panel and the write in progress.
    pub(crate) tools: ToolsPanel,
    /// Stamps placed on the open document and the stamp panel.
    pub(crate) stamps: StampPanel,
    /// Lines of text changed on the open document and the text edit panel.
    pub(crate) text_edits: TextEditPanel,
    /// The signature panel.
    pub(crate) signing: SignaturePanel,
    pub(crate) open_task: Option<Task<()>>,
    pub(crate) dev_script: Option<Task<()>>,
    _wake_task: Task<()>,
    _appearance: Subscription,
    _bounds: Subscription,
    _quit: Subscription,
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
        // A write in progress is undone, not cut off, when FastPDF quits.
        let quit = cx.on_app_quit(|this, _| {
            this.tools.finish_before_exit();
            async {}
        });

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
            page_turn: PageTurn::default(),
            scrollbars: ScrollBarState::default(),
            menus: MenuState::default(),
            go_to: crate::go_to_page::GoToPage::new(window, cx),
            tools: ToolsPanel::new(cx),
            stamps: StampPanel::default(),
            text_edits: TextEditPanel::new(cx),
            signing: SignaturePanel::new(cx),
            open_task: None,
            dev_script: None,
            _wake_task: wake_task,
            _appearance: appearance,
            _bounds: bounds,
            _quit: quit,
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
        self.tools.drain();
        self.poll_page_labels(cx);
        cx.notify();
    }

    // ---- commands -----------------------------------------------------------

    pub(crate) fn run_command(
        &mut self,
        command: ReaderCommand,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        use ReaderCommand as C;
        // A prompt blocks everything but answering or leaving it.
        if self.active_prompt().is_some() && !command.allowed_during_prompt() {
            return;
        }
        if command.writes_document() && self.refuse_protected_write(cx) {
            return;
        }
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
            C::ConfirmPrompt => self.confirm_prompt(window, cx),
            C::GoToPage => self.start_go_to(window, cx),
            C::ConfirmGoToPage => {
                self.confirm_go_to(window, cx);
            }
            C::ToggleMatchCase => self.toggle_find_option(FindOption::MatchCase, cx),
            C::ToggleWholeWord => self.toggle_find_option(FindOption::WholeWord, cx),
            C::ShowSearchResults => self.show_search_results(window, cx),
            C::NextResult => self.select_result(true, cx),
            C::PreviousResult => self.select_result(false, cx),
            C::OpenResult => self.open_active_result(cx),
            C::Print => self.toggle_print_panel(window, cx),
            C::SaveCopy => {
                self.menus.open = None;
                self.save_copy(None, window, cx);
            }
            C::PageTools => {
                self.menus.open = None;
                self.toggle_tools_panel(window, cx);
            }
            C::StampTool => {
                self.menus.open = None;
                self.toggle_stamp_panel(cx);
            }
            C::RemoveStamp => {
                self.remove_selected_stamp();
            }
            C::EditText => {
                self.menus.open = None;
                self.toggle_text_edit_panel(window, cx);
            }
            C::ConfirmEdit => {
                self.apply_line_edit(cx);
                window.focus(&self.focus, cx);
            }
            C::Sign => {
                self.menus.open = None;
                self.toggle_sign_panel(window, cx);
            }
            C::LayoutSinglePage => self.set_page_layout(PageLayout::SinglePage, cx),
            C::LayoutContinuous => self.set_page_layout(PageLayout::Continuous, cx),
            C::LayoutTwoPage => self.set_page_layout(PageLayout::TwoPage, cx),
            C::LayoutTwoPageContinuous => {
                self.set_page_layout(PageLayout::TwoPageContinuous, cx);
            }
            C::ToggleCoverPage => {
                self.settings.cover_page = !self.settings.cover_page;
                self.apply_page_layout(cx);
            }
            C::FirstPage => self.jump(Jump::Direct, |s| s.first_page()),
            C::LastPage => self.jump(Jump::Direct, |s| s.last_page()),
            C::NavigateBack => self.go_back(),
            C::NavigateForward => self.go_forward(),
            C::ZoomIn
            | C::ZoomOut
            | C::ActualSize
            | C::FitPage
            | C::FitWidth
            | C::RotateClockwise
            | C::RotateCounterClockwise
            | C::NextPage
            | C::PreviousPage
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

    /// Esc: cancels a prompt, closes the page field or a menu, stops editing
    /// a line, unarms or deselects a stamp, closes the stamp or text edit
    /// panel, stops printing or closes the print panel, then the page tools,
    /// the settings panel, leaves the result list, closes the find bar, then
    /// clears the selection.
    fn cancel(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if self.cancel_prompt(window, cx) || self.end_go_to(true, window, cx) {
        } else if self.menus.open.is_some() {
            self.menus.open = None;
        } else if self.cancel_line_edit(window, cx) {
        } else if self.stamps.armed.is_some() || self.stamps.selected.is_some() {
            self.stamps.armed = None;
            self.stamps.selected = None;
        } else if self.stamps.open {
            self.toggle_stamp_panel(cx);
        } else if self.text_edits.open {
            self.close_text_edit_panel(window, cx);
        } else if self.signing.open {
            self.toggle_sign_panel(window, cx);
        } else if self.print.printing() {
            self.print.cancel_job();
        } else if self.print.open {
            self.close_print_panel(window, cx);
        } else if self.tools.open {
            self.close_tools_panel(window, cx);
        } else if self.settings_open {
            self.settings_open = false;
        } else if self.find.results.is_focused(window) {
            window.focus(&self.focus, cx);
        } else if self.find.open {
            self.close_find(window, cx);
        } else {
            self.selection.clear();
        }
    }

    // ---- page layout ------------------------------------------------------------

    fn set_page_layout(&mut self, layout: PageLayout, cx: &mut Context<'_, Self>) {
        self.settings.page_layout = layout;
        self.apply_page_layout(cx);
    }

    /// Saves the layout settings and shows the document in them.
    fn apply_page_layout(&mut self, cx: &mut Context<'_, Self>) {
        self.menus.open = None;
        self.save_settings(cx);
        let mode = self.settings.layout_mode();
        self.smooth_scroll.stop();
        self.page_turn = PageTurn::default();
        if let Some(session) = self.session_mut() {
            session.set_layout_mode(mode);
        }
        cx.notify();
    }

    /// A menu's button was clicked.
    pub(crate) fn toggle_menu(
        &mut self,
        menu: Menu,
        event: &ClickEvent,
        cx: &mut Context<'_, Self>,
    ) {
        let dismissed = self.menus.dismissed_at.take();
        let closed_by_this_click =
            matches!(event, ClickEvent::Mouse(m) if dismissed == Some((menu, m.down.position)));
        if !closed_by_this_click {
            self.flip_menu(menu);
        }
        cx.notify();
    }

    /// Opens `menu`, or closes it when open; nothing opens behind a prompt.
    pub(crate) fn flip_menu(&mut self, menu: Menu) {
        if self.active_prompt().is_none() {
            self.menus.open = (!self.menus.is_open(menu)).then_some(menu);
        }
    }

    /// A press outside the open menu closes it.
    pub(crate) fn dismiss_menu(
        &mut self,
        menu: Menu,
        event: &MouseDownEvent,
        cx: &mut Context<'_, Self>,
    ) {
        if self.menus.is_open(menu) {
            self.menus = MenuState {
                open: None,
                dismissed_at: Some((menu, event.position)),
            };
            cx.notify();
        }
    }

    // ---- find (spec §22) -----------------------------------------------------

    pub(crate) fn open_find(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
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
        let options = self.find_options();
        self.find
            .start(text, options, target, &texts, move || waker.wake(), cx);
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
        if self.find.step(forward) {
            self.reveal_active_hit();
        }
        cx.notify();
    }

    pub(crate) fn reveal_active_hit(&mut self) {
        let Some((page, rect)) = self
            .find
            .hits
            .active()
            .and_then(|h| Some((h.page, *h.rects.first()?)))
        else {
            return;
        };
        self.jump(Jump::SearchHit, |s| reveal(s, page, rect));
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
        self.relabel_password_field(cx);
        self.go_to.field.update(cx, |input, cx| {
            input.set_placeholder(strings.go_to_placeholder(), cx)
        });
    }

    /// Everything a new session needs besides the document.
    pub(crate) fn session_parts(&self) -> SessionParts {
        SessionParts {
            config: SessionConfig {
                layout: self.settings.layout_mode(),
                ..self.options.session.clone()
            },
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

    pub(crate) fn settings_path(&self) -> Option<&Path> {
        self.settings_store.path()
    }
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
            .child(toolbar::render(self, window, cx))
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
                crate::navigation::listen_navigation_buttons(view.clone(), window);
                let (selecting, scrolling, stamping) = view.update(cx, |this, cx| {
                    this.paint_viewport(bounds, state, window, cx);
                    (
                        this.selection.dragging,
                        this.scrollbars.drag.is_some(),
                        this.stamps.drag.is_some(),
                    )
                });
                if scrolling {
                    crate::viewport::track_scrollbar_drag(view, window);
                } else if stamping {
                    crate::stamps::track_stamp_drag(view, window);
                } else if selecting {
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
            .on_mouse_move(cx.listener(Self::on_document_mouse_move))
            .on_hover(cx.listener(Self::on_document_hover))
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
            .when(self.tools.open, |area| {
                area.child(self.render_tools_panel(cx))
            })
            .when(self.stamps.open, |area| {
                area.child(self.render_stamp_panel(cx))
            })
            .when(self.text_edits.open, |area| {
                area.child(self.render_text_edit_panel(cx))
            })
            .when(self.signing.open, |area| {
                area.child(self.render_sign_panel(cx))
            })
            .when_some(self.render_line_editor(cx), |area, editor| {
                area.child(editor)
            })
            .when(self.stamps.armed.is_some(), |area| area.cursor_crosshair())
            .when(
                self.text_edits.open && self.text_edits.hover.is_some(),
                |area| area.cursor_text(),
            )
            .when_some(self.render_write_notice(cx), |area, notice| {
                area.child(notice)
            })
            .when(self.settings_open, |area| {
                area.child(self.render_settings_panel(cx))
            })
            .when_some(self.render_prompt(cx), |area, prompt| area.child(prompt))
    }

    /// Page `page` as shown in the toolbar ("12 / 345").
    pub(crate) fn page_indicator(&self) -> Option<(PageIndex, u32)> {
        let session = self.session()?;
        Some((session.current_page(), session.page_count()))
    }
}
