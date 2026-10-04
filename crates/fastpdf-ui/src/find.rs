//! Find in document (Ctrl+F, spec §22): a find field over the document,
//! hits streamed from `fastpdf_search` while the search runs from the
//! current page outward, F3 / Shift+F3 / Enter navigation, highlights drawn
//! by the viewport canvas.

use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};

use fastpdf_core::keymap::ReaderCommand;
use fastpdf_engine_api::{
    DocumentId, EngineDocument, EngineError, GuardedDocument, PageIndex, PageRect,
};
use fastpdf_search::{SearchEvent, SearchHit, SearchQuery, SearchSession, TextCache};
use gpui::{
    AppContext, ClickEvent, Context, Entity, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, Subscription, div, px,
};

use crate::i18n::Strings;
use crate::reader::ReaderView;
use crate::text_input::{TextChanged, TextInput};
use crate::toolbar::{styled_button, tooltip};

/// Width of the status text, enough for "12345 found so far… (1234/2000 pages)".
const STATUS_WIDTH: f32 = 230.0;

/// Hits in document order with the active one.
#[derive(Debug, Default, Clone, PartialEq)]
pub(crate) struct HitList {
    hits: Vec<SearchHit>,
    active: Option<usize>,
}

impl HitList {
    pub(crate) fn len(&self) -> usize {
        self.hits.len()
    }

    pub(crate) fn active_index(&self) -> Option<usize> {
        self.active
    }

    pub(crate) fn active(&self) -> Option<&SearchHit> {
        self.hits.get(self.active?)
    }

    /// Adds hits from one page, keeping document order and the active hit.
    pub(crate) fn insert(&mut self, mut found: Vec<SearchHit>) {
        found.sort_by_key(|h| (h.page, h.start));
        for hit in found {
            let at = self
                .hits
                .partition_point(|h| (h.page, h.start) < (hit.page, hit.start));
            if let Some(active) = self.active.as_mut()
                && at <= *active
            {
                *active += 1;
            }
            self.hits.insert(at, hit);
        }
    }

    /// Activates the first hit at or after `page` (wrapping to the first),
    /// unless one is active already. Returns true when it changed.
    pub(crate) fn activate_from(&mut self, page: PageIndex) -> bool {
        if self.active.is_some() || self.hits.is_empty() {
            return false;
        }
        let at = self.hits.partition_point(|h| h.page < page);
        self.active = Some(if at < self.hits.len() { at } else { 0 });
        true
    }

    /// Moves to the next (or previous) hit, wrapping around.
    pub(crate) fn step(&mut self, forward: bool) -> Option<&SearchHit> {
        let len = self.hits.len();
        if len == 0 {
            return None;
        }
        let next = match (self.active, forward) {
            (None, true) => 0,
            (None, false) => len - 1,
            (Some(i), true) => (i + 1) % len,
            (Some(i), false) => (i + len - 1) % len,
        };
        self.active = Some(next);
        self.hits.get(next)
    }

    /// Highlight rectangles on `page`; the bool marks the active hit.
    pub(crate) fn on_page(&self, page: PageIndex) -> impl Iterator<Item = (PageRect, bool)> + '_ {
        let start = self.hits.partition_point(|h| h.page < page);
        let active = self.active;
        self.hits[start..]
            .iter()
            .enumerate()
            .take_while(move |(_, h)| h.page == page)
            .flat_map(move |(i, h)| {
                let is_active = active == Some(start + i);
                h.rects.iter().map(move |r| (*r, is_active))
            })
    }
}

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
}

/// The find bar and its search.
pub(crate) struct FindBar {
    pub open: bool,
    pub input: Entity<TextInput>,
    /// Query of the current search (trimmed).
    pub query: String,
    pub hits: HitList,
    pub progress: Progress,
    generation: u64,
    search: Option<SearchSession>,
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
            generation: 0,
            search: None,
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
    }

    /// Starts searching `query` (cancels the old search).
    pub(crate) fn start(
        &mut self,
        query: String,
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
        let generation = self.generation;
        let tx = self.tx.clone();
        self.search = SearchSession::start(
            target.document,
            target.doc as Arc<dyn EngineDocument>,
            Arc::clone(texts),
            &SearchQuery {
                text: self.query.clone(),
                case_sensitive: false,
            },
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
                    self.hits.insert(hits);
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
                SearchEvent::Finished { .. } => self.progress.finished = true,
            }
        }
        got_hits
    }

    /// The status shown next to the field.
    pub(crate) fn status(&self, strings: &Strings) -> String {
        status_text(&self.query, &self.hits, self.progress, strings)
    }
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
        div()
            .id("find-bar")
            .absolute()
            .top(px(8.0))
            .right(px(20.0))
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
                    .w(px(220.0))
                    .h(px(28.0))
                    .px_2()
                    .rounded(px(4.0))
                    .bg(theme.input_bg)
                    .border_1()
                    .border_color(theme.accent)
                    .text_size(px(14.0))
                    .child(self.find.input.clone()),
            )
            .child(
                // Fixed width: the bar is anchored on the right, so a status
                // that changes length while typing would move the input box.
                div()
                    .w(px(STATUS_WIDTH))
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
    match (n, hits.active_index()) {
        (0, _) => strings.no_results.into(),
        (n, Some(i)) => strings.hit_position(i + 1, n),
        (n, None) => strings.result_count(n),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(page: u32, start: usize) -> SearchHit {
        SearchHit {
            page: PageIndex::new(page),
            start,
            end: start + 2,
            rects: vec![PageRect::new(start as f32, 0.0, start as f32 + 10.0, 10.0)],
        }
    }

    #[test]
    fn hits_stay_in_document_order_and_keep_the_active_one() {
        let mut list = HitList::default();
        // The search starts at page 5 and wraps around.
        list.insert(vec![hit(5, 30), hit(5, 10)]);
        assert!(list.activate_from(PageIndex::new(5)));
        assert_eq!(
            list.active().map(|h| (h.page.get(), h.start)),
            Some((5, 10))
        );
        list.insert(vec![hit(7, 0)]);
        list.insert(vec![hit(1, 4), hit(2, 0)]);
        // Earlier hits shift the index, not the active hit.
        assert_eq!(
            list.active().map(|h| (h.page.get(), h.start)),
            Some((5, 10))
        );
        assert!(!list.activate_from(PageIndex::FIRST), "already active");
        let order: Vec<_> = (0..list.len())
            .map(|_| list.step(true).map(|h| (h.page.get(), h.start)))
            .collect();
        assert_eq!(
            order,
            vec![
                Some((5, 30)),
                Some((7, 0)),
                Some((1, 4)),
                Some((2, 0)),
                Some((5, 10))
            ]
        );
        assert_eq!(list.step(false).map(|h| h.page.get()), Some(2));
    }

    #[test]
    fn activation_wraps_when_no_hit_follows_the_page() {
        let mut list = HitList::default();
        list.insert(vec![hit(0, 0), hit(1, 0)]);
        assert!(list.activate_from(PageIndex::new(9)));
        assert_eq!(list.active_index(), Some(0));
        let mut empty = HitList::default();
        assert!(!empty.activate_from(PageIndex::FIRST));
        assert!(empty.step(true).is_none());
    }

    #[test]
    fn page_rects_mark_the_active_hit() {
        let mut list = HitList::default();
        list.insert(vec![hit(3, 0), hit(3, 20), hit(4, 0)]);
        list.step(true);
        list.step(true); // second hit on page 3
        let rects: Vec<_> = list.on_page(PageIndex::new(3)).collect();
        assert_eq!(rects.len(), 2);
        assert!(!rects[0].1 && rects[1].1);
        assert_eq!(list.on_page(PageIndex::new(9)).count(), 0);
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
        p.unsupported = true;
        assert!(status_text("x", &list, p, en).contains("not available"));
    }
}
