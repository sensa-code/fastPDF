//! Find in document (Ctrl+F, spec §22): a find field over the document,
//! hits streamed from `fastpdf_search` while the search runs from the
//! current page outward, F3 / Shift+F3 / Enter navigation, highlights drawn
//! by the viewport canvas. The hits are kept by `crate::hit_list`, the
//! options (match case, whole words) by `crate::find_options`, and listed
//! by `crate::search_results`.

use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Instant;

use fastpdf_core::keymap::ReaderCommand;
use fastpdf_engine_api::{DocumentId, EngineDocument, EngineError, GuardedDocument, PageIndex};
use fastpdf_search::{SearchEvent, SearchSession, TextCache, search_order};
use gpui::{
    AppContext, ClickEvent, Context, Entity, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, Subscription, div, prelude::FluentBuilder, px,
};

use crate::find_options::FindOptions;
use crate::hit_list::HitList;
use crate::i18n::Strings;
use crate::reader::ReaderView;
use crate::search_results::ResultsList;
use crate::sidebar::SidebarTab;
use crate::text_input::{TextChanged, TextInput};
use crate::toolbar::{styled_button, tooltip};

/// Width of the status text, enough for "12345 found so far… (1234/2000 pages)".
const STATUS_WIDTH: f32 = 230.0;
/// Width of the find field with its option toggles.
const FIELD_WIDTH: f32 = 264.0;
/// The status gives way first in a narrow document area, down to this.
const STATUS_MIN_WIDTH: f32 = 96.0;

/// What to search: a document, from a page outward.
#[derive(Debug)]
pub(crate) struct SearchTarget {
    pub doc: Arc<GuardedDocument>,
    pub document: DocumentId,
    pub start: PageIndex,
}

/// Where a running search stands.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Progress {
    pub pages_done: u32,
    pub page_count: u32,
    pub finished: bool,
    pub unsupported: bool,
    pub failed_pages: u32,
    /// The search stopped at its hit cap; more hits may exist.
    pub limit_reached: bool,
}

/// The find bar and its search.
pub(crate) struct FindBar {
    pub open: bool,
    pub input: Entity<TextInput>,
    /// Query of the current search (trimmed).
    pub query: String,
    pub hits: HitList,
    pub progress: Progress,
    /// The list of the hits in the sidebar (`crate::search_results`).
    pub results: ResultsList,
    /// Where the current search started.
    start_page: PageIndex,
    /// A search stopped at its cap looked at these pages (one-based, first
    /// and last; the pages searched around the start page are contiguous).
    pub searched: Option<(u32, u32)>,
    generation: u64,
    search: Option<SearchSession>,
    /// When the current search started (logged with its result).
    started: Option<Instant>,
    tx: Sender<(u64, SearchEvent)>,
    rx: Receiver<(u64, SearchEvent)>,
    _input_changes: Subscription,
}

impl std::fmt::Debug for FindBar {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FindBar")
            .field("open", &self.open)
            .field("query", &self.query)
            .field("hits", &self.hits.len())
            .finish_non_exhaustive()
    }
}

impl FindBar {
    pub(crate) fn new(cx: &mut Context<'_, ReaderView>) -> Self {
        // The placeholder follows the UI language (`ReaderView::apply_language`).
        let input = cx.new(|cx| TextInput::new("", cx));
        let subscription = cx.subscribe(
            &input,
            |view: &mut ReaderView, _, event: &TextChanged, cx| {
                view.on_find_query(event.0.clone(), cx);
            },
        );
        let (tx, rx) = mpsc::channel();
        Self {
            open: false,
            input,
            query: String::new(),
            hits: HitList::default(),
            progress: Progress::default(),
            results: ResultsList::new(cx.focus_handle()),
            start_page: PageIndex::FIRST,
            searched: None,
            generation: 0,
            search: None,
            started: None,
            tx,
            rx,
            _input_changes: subscription,
        }
    }

    /// Stops the running search (the thread is joined off the UI thread)
    /// and forgets its results.
    pub(crate) fn reset(&mut self, cx: &mut Context<'_, ReaderView>) {
        self.generation += 1;
        if let Some(search) = self.search.take() {
            search.cancel();
            cx.background_executor()
                .spawn(async move { drop(search) })
                .detach();
        }
        self.hits = HitList::default();
        self.progress = Progress::default();
        self.results.reset();
        self.searched = None;
    }

    /// Starts searching `query` with `options` (cancels the old search).
    pub(crate) fn start(
        &mut self,
        query: String,
        options: FindOptions,
        target: SearchTarget,
        texts: &Arc<TextCache>,
        wake: impl Fn() + Send + 'static,
        cx: &mut Context<'_, ReaderView>,
    ) {
        self.reset(cx);
        self.query = query.trim().to_string();
        if self.query.is_empty() {
            return;
        }
        self.progress.page_count = target.doc.page_count();
        self.start_page = target.start;
        self.started = Some(Instant::now());
        let generation = self.generation;
        let tx = self.tx.clone();
        self.search = SearchSession::start(
            target.document,
            target.doc as Arc<dyn EngineDocument>,
            Arc::clone(texts),
            &options.query(self.query.clone()),
            target.start,
            move |event| {
                if tx.send((generation, event)).is_ok() {
                    wake();
                }
            },
        );
    }

    /// Applies events from the search thread. Returns true when hits
    /// arrived (the first one may need revealing).
    pub(crate) fn drain(&mut self) -> bool {
        let mut got_hits = false;
        while let Ok((generation, event)) = self.rx.try_recv() {
            if generation != self.generation {
                continue;
            }
            match event {
                SearchEvent::Hits(hits) => {
                    let before = self.hits.len();
                    if let Some(inserted) = self.hits.insert(hits) {
                        self.results.rows_inserted(inserted, before);
                    }
                    got_hits = true;
                }
                SearchEvent::Progress {
                    pages_done,
                    page_count,
                    ..
                } => {
                    self.progress.pages_done = pages_done;
                    self.progress.page_count = page_count;
                }
                SearchEvent::PageFailed { error, .. } => {
                    if matches!(error, EngineError::Unsupported(_)) {
                        self.progress.unsupported = true;
                    } else {
                        self.progress.failed_pages += 1;
                    }
                }
                SearchEvent::Finished {
                    hits,
                    cancelled,
                    limit_reached,
                } => {
                    self.progress.finished = true;
                    self.progress.limit_reached = limit_reached;
                    if limit_reached {
                        self.searched = searched_range(
                            self.start_page,
                            self.progress.page_count,
                            self.progress.pages_done,
                        );
                    }
                    if !cancelled {
                        log::info!(
                            "search finished: {hits} hits{}, {} KiB of results, in {:.1} ms",
                            if limit_reached { " (limit)" } else { "" },
                            self.hits.heap_bytes().div_ceil(1024),
                            self.started
                                .map_or(0.0, |t| t.elapsed().as_secs_f64() * 1000.0),
                        );
                    }
                }
            }
        }
        got_hits
    }

    /// The status shown next to the field.
    pub(crate) fn status(&self, strings: &Strings) -> String {
        status_text(&self.query, &self.hits, self.progress, strings)
    }

    /// F3 / Shift+F3: the next (or previous) hit becomes the active one,
    /// and its row shows in the result list. False without hits.
    pub(crate) fn step(&mut self, forward: bool) -> bool {
        let moved = self.hits.step(forward).is_some();
        if let Some(active) = self.hits.active_index() {
            self.results.reveal(active);
        }
        moved
    }
}

/// The pages (one-based, first and last) of the first `pages_done` pages
/// searched from `start`: contiguous, as the search goes outward.
fn searched_range(start: PageIndex, page_count: u32, pages_done: u32) -> Option<(u32, u32)> {
    let pages = search_order(start, page_count).take(pages_done as usize);
    let (first, last) = pages.fold((u32::MAX, 0), |(first, last), page| {
        (first.min(page.get()), last.max(page.get()))
    });
    (first <= last).then(|| (first + 1, last + 1))
}

impl ReaderView {
    /// The find bar, floating over the top-right corner of the document.
    pub(crate) fn render_find_bar(&self, cx: &mut Context<'_, Self>) -> impl IntoElement + use<> {
        let theme = self.theme;
        let strings = self.strings();
        let font = self.language().ui_font();
        let has_hits = self.find.hits.len() > 0;
        let next_keys = fastpdf_core::keymap::keystrokes_for(ReaderCommand::FindNext).next();
        let previous_keys =
            fastpdf_core::keymap::keystrokes_for(ReaderCommand::FindPrevious).next();
        // In a narrow document area (a small window with the sidebar open)
        // the status gives way, never the field or the buttons.
        let room = self
            .viewport_bounds
            .map(|b| (f32::from(b.size.width) - 28.0).max(0.0));
        div()
            .id("find-bar")
            .absolute()
            .top(px(8.0))
            .right(px(20.0))
            .when_some(room, |bar, room| bar.max_w(px(room)))
            .occlude()
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .p_1()
            .rounded(px(6.0))
            .bg(theme.toolbar_bg)
            .border_1()
            .border_color(theme.toolbar_border)
            .text_size(px(13.0))
            .child(
                div()
                    .w(px(FIELD_WIDTH))
                    .h(px(28.0))
                    .flex_none()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_1()
                    .pl_2()
                    .pr(px(2.0))
                    .rounded(px(4.0))
                    .bg(theme.input_bg)
                    .border_1()
                    .border_color(theme.accent)
                    .text_size(px(14.0))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .child(self.find.input.clone()),
                    )
                    .child(self.render_find_toggles(cx)),
            )
            .child(
                // Fixed width: the bar is anchored on the right, so a status
                // that changes length while typing would move the input box.
                div()
                    .w(px(STATUS_WIDTH))
                    .min_w(px(STATUS_MIN_WIDTH))
                    .flex_shrink_1()
                    .px_1()
                    .text_color(theme.text_muted)
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(self.find.status(strings)),
            )
            .child(
                styled_button("find-prev", "\u{2191}", has_hits, false, &theme)
                    .tooltip(tooltip(
                        strings
                            .with_shortcut(strings.tip_previous_match, previous_keys)
                            .into(),
                        theme,
                        font,
                    ))
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.find_step(false, window, cx);
                    })),
            )
            .child(
                styled_button("find-next", "\u{2193}", has_hits, false, &theme)
                    .tooltip(tooltip(
                        strings
                            .with_shortcut(strings.tip_next_match, next_keys)
                            .into(),
                        theme,
                        font,
                    ))
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.find_step(true, window, cx);
                    })),
            )
            .child(
                styled_button(
                    "find-list",
                    "\u{2630}",
                    true,
                    self.sidebar.open && self.sidebar.tab == SidebarTab::Results,
                    &theme,
                )
                .tooltip(tooltip(
                    strings
                        .with_shortcut(
                            strings.tip_show_results(),
                            fastpdf_core::keymap::keystrokes_for(ReaderCommand::ShowSearchResults)
                                .next(),
                        )
                        .into(),
                    theme,
                    font,
                ))
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                    this.run_command(ReaderCommand::ShowSearchResults, window, cx);
                })),
            )
            .child(
                styled_button("find-close", "\u{2715}", true, false, &theme)
                    .tooltip(tooltip(
                        strings.with_shortcut(strings.close, Some("escape")).into(),
                        theme,
                        font,
                    ))
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.close_find(window, cx);
                    })),
            )
    }
}

fn status_text(query: &str, hits: &HitList, p: Progress, strings: &Strings) -> String {
    if query.is_empty() {
        return String::new();
    }
    if p.unsupported {
        return strings.search_unavailable.into();
    }
    let n = hits.len();
    if !p.finished {
        return strings.found_so_far(n, p.pages_done, p.page_count);
    }
    match (n, hits.active_index(), p.limit_reached) {
        (0, _, _) => strings.no_results.into(),
        (n, Some(i), true) => strings.hit_position_capped(i + 1, n),
        (n, Some(i), false) => strings.hit_position(i + 1, n),
        (n, None, true) => strings.limit_reached(n),
        (n, None, false) => strings.result_count(n),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fastpdf_engine_api::PageRect;
    use fastpdf_search::SearchHit;

    fn hit(page: u32, start: usize) -> SearchHit {
        SearchHit {
            page: PageIndex::new(page),
            start,
            end: start + 2,
            rects: vec![PageRect::new(start as f32, 0.0, start as f32 + 10.0, 10.0)],
            context: Default::default(),
        }
    }

    #[test]
    fn status_names_the_active_hit_in_document_order() {
        let mut list = HitList::default();
        list.insert(vec![hit(3, 60), hit(3, 40), hit(3, 0)]);
        assert!(list.activate_from(PageIndex::new(3)));
        list.step(true);
        let done = Progress {
            finished: true,
            ..Progress::default()
        };
        let en = crate::i18n::Language::English.strings();
        assert_eq!(status_text("x", &list, done, en), "2 of 3");
    }

    #[test]
    fn searched_pages_are_the_ones_around_the_start() {
        let at =
            |start: u32, count: u32, done: u32| searched_range(PageIndex::new(start), count, done);
        // From page 20 (index 19): 20, 21, 19, 22, 18.
        assert_eq!(at(19, 40, 5), Some((18, 22)));
        // Past the last page the search goes on before the start only.
        assert_eq!(at(38, 40, 6), Some((35, 40)));
        assert_eq!(at(0, 300, 12), Some((1, 12)));
        assert_eq!(at(0, 300, 0), None);
        assert_eq!(at(0, 0, 3), None);
    }

    #[test]
    fn status_reports_progress_then_position() {
        let mut list = HitList::default();
        let mut p = Progress {
            pages_done: 12,
            page_count: 300,
            ..Progress::default()
        };
        let en = crate::i18n::Language::English.strings();
        let zh = crate::i18n::Language::TraditionalChinese.strings();
        assert_eq!(status_text("", &list, p, en), "");
        list.insert(vec![hit(0, 0), hit(1, 0), hit(2, 0)]);
        assert_eq!(
            status_text("x", &list, p, en),
            "3 found so far\u{2026} (12/300 pages)"
        );
        assert_eq!(
            status_text("x", &list, p, zh),
            "已找到 3 筆\u{2026}（12／300 頁）"
        );
        p.finished = true;
        assert_eq!(status_text("x", &list, p, en), "3 results");
        list.step(true);
        assert_eq!(status_text("x", &list, p, en), "1 of 3");
        assert_eq!(status_text("x", &list, p, zh), "第 1／3 筆");
        assert_eq!(status_text("x", &HitList::default(), p, en), "No results");
        // The search stopped at its cap: more may exist.
        p.limit_reached = true;
        assert_eq!(status_text("x", &list, p, en), "1 of 3+");
        assert_eq!(status_text("x", &list, p, zh), "第 1／3+ 筆");
        let mut fresh = HitList::default();
        fresh.insert(vec![hit(0, 0), hit(1, 0)]);
        assert_eq!(status_text("x", &fresh, p, en), "Stopped at 2 results");
        assert_eq!(status_text("x", &fresh, p, zh), "已達 2 筆上限，搜尋已停止");
        p.unsupported = true;
        assert!(status_text("x", &list, p, en).contains("not available"));
    }
}
