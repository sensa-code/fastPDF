//! The search result list (Ctrl+Shift+F): the sidebar's third tab (搜尋結果 /
//! Results), a row per hit of the find bar's search with its page (and
//! page label) and the hit in a line of the page's text.
//!
//! * Only the visible rows are built (`uniform_list`); a row's text is the
//!   hit's snippet, shared, not copied.
//! * The list follows the search as it streams from the start page outward:
//!   rows of pages found later above the top row are added above it without
//!   moving the rows in view, so the start page's results stay where they
//!   are. F3 / Shift+F3 keep the active row in view.
//! * A click on a row, or Enter in the list, shows the hit in the document as
//!   a search-hit jump (a run of them is one Back); Up / Down move the active
//!   row only. Esc gives the keyboard back to the document.
//! * The list belongs to the find bar's search: closing the find bar, or the
//!   document, empties it. It is never the tab the sidebar opens with.

use std::ops::Range;

use fastpdf_core::keymap::{self, ReaderCommand, keystrokes_for};
use fastpdf_engine_api::{ColorMode, PageIndex};
use gpui::{
    AnyElement, ClickEvent, Context, Div, ElementId, FocusHandle, FontWeight, HighlightStyle,
    InteractiveElement, IntoElement, ParentElement, ScrollStrategy, SharedString,
    StatefulInteractiveElement, Styled, StyledText, UniformListScrollHandle, Window, div, point,
    prelude::FluentBuilder, px, uniform_list,
};

use crate::hit_list::Inserted;
use crate::reader::ReaderView;
use crate::sidebar::SidebarTab;
use crate::theme::{PageHighlights, Theme};

/// Height of a row: the page line and two lines of context. Laid out in
/// whole device pixels, so rows are [`row_pitch`] apart.
pub(crate) const ROW_HEIGHT: f32 = 58.0;
/// Line heights in a row: 4 + 15 + 2 x 17 + 4 = 57 of [`ROW_HEIGHT`].
const CAPTION_LINE: f32 = 15.0;
const CONTEXT_LINE: f32 = 17.0;

/// Scroll position and keyboard focus of the result list.
pub(crate) struct ResultsList {
    pub scroll: UniformListScrollHandle,
    pub focus: FocusHandle,
    /// The window's scale factor at the last frame: rows are laid out in
    /// whole device pixels, so their logical pitch depends on it.
    scale: f32,
}

impl ResultsList {
    pub(crate) fn new(focus: FocusHandle) -> Self {
        Self {
            scroll: UniformListScrollHandle::new(),
            focus,
            scale: 1.0,
        }
    }

    /// The distance between the tops of two rows, in logical pixels.
    pub(crate) fn pitch(&self) -> f32 {
        row_pitch(self.scale)
    }

    /// A new search starts at the top.
    pub(crate) fn reset(&mut self) {
        self.scroll = UniformListScrollHandle::new();
    }

    /// Scrolls just enough to show `row` (on the next layout).
    pub(crate) fn reveal(&self, row: usize) {
        self.scroll.scroll_to_item(row, ScrollStrategy::Nearest);
    }

    /// Rows came in (`rows_before` rows were listed): rows above the top row
    /// shift the view by as much, so the rows in view stay; a scroll asked
    /// for but not done yet follows its row.
    pub(crate) fn rows_inserted(&self, inserted: Inserted, rows_before: usize) {
        let mut state = self.scroll.0.borrow_mut();
        if let Some(pending) = state.deferred_scroll_to_item.as_mut()
            && inserted.at <= pending.item_index
        {
            pending.item_index += inserted.count;
        }
        let offset = state.base_handle.offset();
        let y = f32::from(offset.y);
        if rows_before > 0 && inserted.at <= top_row(y, self.pitch()) {
            state
                .base_handle
                .set_offset(point(offset.x, px(anchored(y, inserted.count, self.scale))));
        }
    }

    pub(crate) fn is_focused(&self, window: &Window) -> bool {
        self.focus.is_focused(window)
    }
}

/// The logical distance between rows at `scale`: GPUI lays lengths out in
/// whole device pixels, rounding halves toward zero (58 px at 125% is 72
/// device pixels, 57.6 logical), and the list places rows one measured
/// row apart. The anchoring arithmetic must use the same pitch, or every
/// row added above the view would scroll it by the difference.
fn row_pitch(scale: f32) -> f32 {
    if !(scale.is_finite() && scale > 0.0) {
        return ROW_HEIGHT;
    }
    let device = ((ROW_HEIGHT * scale).abs() - 0.5).ceil().max(1.0);
    device / scale
}

/// Part of a row that may show above the top row without counting: the
/// offset is kept in `f32`, so after many anchors in a long list the top
/// row may sit a fraction of a pixel below the edge.
const SLIVER: f32 = 0.02;

/// The first row in view at scroll offset `y` (zero or negative, as GPUI
/// keeps it), rows `pitch` apart; a row whose top is less than a
/// [`SLIVER`] below the edge counts as on top.
fn top_row(y: f32, pitch: f32) -> usize {
    let scrolled = -y / pitch + SLIVER;
    if scrolled.is_finite() && scrolled > 0.0 {
        scrolled.floor() as usize
    } else {
        0
    }
}

/// The offset that keeps the rows in view where they are after `rows` rows
/// were added above them, at `scale`. It is made from the top row and how
/// far into it the view starts, in whole device pixels, not by moving the
/// old offset: the offset is an `f32`, and a pitch such as 57.6 px rounds
/// the same way on every insert, which would add up to rows in a long list.
fn anchored(y: f32, rows: usize, scale: f32) -> f32 {
    let pitch = row_pitch(scale);
    let top = top_row(y, pitch) as f64;
    let pitch = f64::from(pitch);
    let scale = if scale.is_finite() && scale > 0.0 {
        f64::from(scale)
    } else {
        1.0
    };
    let into_row = ((-f64::from(y) - top * pitch) * scale).round() / scale;
    (-((top + rows as f64) * pitch + into_row)) as f32
}

/// A row's element id: its hit (page and byte offset in the page's search
/// text), not its place in the list.
fn row_key(page: PageIndex, start: usize) -> ElementId {
    let mut key = [0u8; 20];
    key[..4].copy_from_slice(&page.get().to_le_bytes());
    key[4..12].copy_from_slice(&(start as u64).to_le_bytes());
    ElementId::OpaqueId(key)
}

impl ReaderView {
    /// The Results tab: a header (count, how far a capped search got) and
    /// the rows, or what there is instead (a hint, searching, no results).
    /// Whatever it shows takes the keyboard focus of the list, so the list
    /// keeps it while a search starts again or the document closes; never
    /// while a prompt waits, which keeps the keyboard (a press here goes to
    /// the reader, which hands it back to the prompt).
    pub(crate) fn render_search_results(&self, cx: &mut Context<'_, Self>) -> AnyElement {
        let free = self.active_prompt().is_none();
        div()
            .id("search-results")
            .key_context(keymap::SEARCH_RESULTS)
            .when(free, |list| list.track_focus(&self.find.results.focus))
            .size_full()
            .child(self.results_content(cx))
            .into_any_element()
    }

    /// Every frame: a list that holds the keyboard but is not shown (the
    /// sidebar closed, another tab) gives it back to the document, and to
    /// the prompt's field while a prompt waits. A focused element that is
    /// not drawn would leave the keyboard outside the reader, where none of
    /// its shortcuts work.
    /// Also notes the window's scale, which sets the rows' pitch.
    pub(crate) fn prepare_search_results(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        self.find.results.scale = window.scale_factor();
        let shown = self.sidebar.open
            && self.sidebar.tab == SidebarTab::Results
            && self.active_prompt().is_none();
        if !shown && self.find.results.is_focused(window) {
            window.focus(&self.focus, cx);
            self.refocus_prompt(window, cx);
        }
    }

    fn results_content(&self, cx: &mut Context<'_, Self>) -> AnyElement {
        let theme = self.theme;
        let strings = self.strings();
        let message = |text: String| {
            div()
                .p_3()
                .text_size(px(13.0))
                .text_color(theme.text_muted)
                .child(text)
                .into_any_element()
        };
        let find = &self.find;
        if self.session().is_none() {
            return message(strings.no_document.into());
        }
        if find.query.is_empty() {
            return message(strings.results_hint(keystrokes_for(ReaderCommand::Find).next()));
        }
        if find.progress.unsupported {
            return message(strings.search_unavailable.into());
        }
        let count = find.hits.len();
        if count == 0 {
            return message(if find.progress.finished {
                strings.no_results.into()
            } else {
                strings.searching().into()
            });
        }
        let list = uniform_list(
            "search-result-rows",
            count,
            cx.processor(|this, range: Range<usize>, window, cx| {
                this.result_rows(range, window, cx)
            }),
        )
        .track_scroll(&find.results.scroll)
        .size_full();
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(self.results_header())
            .child(div().flex_1().min_h_0().child(list))
            .into_any_element()
    }

    /// How many results, and for a search stopped at its cap, the pages it
    /// looked at.
    fn results_header(&self) -> Div {
        let theme = self.theme;
        let strings = self.strings();
        let p = self.find.progress;
        let n = self.find.hits.len();
        let count = if !p.finished {
            strings.found_so_far(n, p.pages_done, p.page_count)
        } else if p.limit_reached {
            strings.limit_reached(n)
        } else {
            strings.result_count(n)
        };
        let searched = self
            .find
            .searched
            .filter(|_| p.limit_reached)
            .map(|(first, last)| strings.searched_pages(first, last));
        div()
            .flex_none()
            .px_3()
            .py_1()
            .text_size(px(12.0))
            .text_color(theme.text_muted)
            .border_b_1()
            .border_color(theme.toolbar_border)
            .child(count)
            .when_some(searched, |header, searched| header.child(searched))
    }

    /// The rows `range` of the list (only the visible ones are asked for).
    /// The active row is shaded; its accent mark shows while the list has
    /// the keyboard.
    fn result_rows(
        &mut self,
        range: Range<usize>,
        window: &Window,
        cx: &mut Context<'_, Self>,
    ) -> Vec<AnyElement> {
        let theme = self.theme;
        let active = self.find.hits.active_index();
        let focused = self.find.results.is_focused(window);
        let pitch = self.find.results.pitch();
        let marks = PageHighlights::for_mode(if theme == Theme::DARK {
            ColorMode::Inverted
        } else {
            ColorMode::Normal
        });
        let highlight = HighlightStyle {
            background_color: Some(marks.search_hit.into()),
            font_weight: Some(FontWeight::SEMIBOLD),
            ..HighlightStyle::default()
        };
        range
            .filter_map(|ix| {
                let hit = self.find.hits.get(ix)?;
                // The row is the hit, not its place: hits of earlier pages
                // come in above it while the search runs.
                let (page, start) = (hit.page, hit.start);
                let caption = self.result_caption(hit.page);
                let context = &hit.context;
                let text = StyledText::new(SharedString::from(context.text.clone()))
                    .with_highlights(context.hit().map(|range| (range, highlight)));
                let selected = active == Some(ix);
                let hover = theme.button_hover;
                Some(
                    div()
                        .id(row_key(page, start))
                        .h(px(pitch))
                        .w_full()
                        .overflow_hidden()
                        .flex()
                        .flex_col()
                        .px_2()
                        .py(px(4.0))
                        .border_l_2()
                        .border_color(gpui::transparent_black())
                        .when(selected, |row| row.bg(theme.button_active))
                        .when(selected && focused, |row| row.border_color(theme.accent))
                        .cursor_pointer()
                        .hover(move |style| style.bg(hover))
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.open_hit(page, start, cx);
                        }))
                        .child(
                            div()
                                .flex_none()
                                .text_size(px(11.0))
                                .line_height(px(CAPTION_LINE))
                                .text_color(theme.text_muted)
                                .whitespace_nowrap()
                                .overflow_hidden()
                                .text_ellipsis()
                                .child(caption),
                        )
                        .child(
                            div()
                                .text_size(px(13.0))
                                .line_height(px(CONTEXT_LINE))
                                .text_color(theme.text)
                                .line_clamp(2)
                                .text_ellipsis()
                                .child(text),
                        )
                        .into_any_element(),
                )
            })
            .collect()
    }

    /// "第 4 頁", or "iv（第 4 頁）" when the page has a label.
    fn result_caption(&self, page: PageIndex) -> String {
        self.strings()
            .result_page(page.display_number(), self.page_label(page).as_deref())
    }

    /// Ctrl+Shift+F and the find bar's list button: the sidebar on the
    /// Results tab, the keyboard in the list at the active result (in the
    /// find field, the find bar open, while there is nothing to list).
    pub(crate) fn show_search_results(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if !self.sidebar.open {
            self.set_sidebar_open(true, window, cx);
        }
        if self.sidebar.tab != SidebarTab::Results {
            self.set_sidebar_tab(SidebarTab::Results, window, cx);
        }
        if !self.find.open || self.find.hits.len() == 0 {
            self.open_find(window, cx);
        } else {
            window.focus(&self.find.results.focus, cx);
            self.find
                .results
                .reveal(self.find.hits.active_index().unwrap_or(0));
        }
        cx.notify();
    }

    /// Up / Down in the list: the next or previous result becomes the
    /// active one; the document stays where it is until Enter.
    pub(crate) fn select_result(&mut self, forward: bool, cx: &mut Context<'_, Self>) {
        let next = match self.find.hits.active_index() {
            Some(i) if forward => i + 1,
            Some(i) => i.saturating_sub(1),
            None => 0,
        };
        if self.find.hits.select(next).is_some()
            && let Some(active) = self.find.hits.active_index()
        {
            self.find.results.reveal(active);
        }
        cx.notify();
    }

    /// Enter in the list: shows the active result.
    pub(crate) fn open_active_result(&mut self, cx: &mut Context<'_, Self>) {
        let row = self.find.hits.active_index().unwrap_or(0);
        self.open_result(row, cx);
    }

    /// Shows the hit at byte `start` of `page`'s search text, wherever its
    /// row is now (a click on its row).
    pub(crate) fn open_hit(&mut self, page: PageIndex, start: usize, cx: &mut Context<'_, Self>) {
        if let Some(row) = self.find.hits.index_of(page, start) {
            self.open_result(row, cx);
        }
    }

    /// Shows result `row` (zero-based) in the document, as a search-hit
    /// jump, and makes it the active one.
    pub(crate) fn open_result(&mut self, row: usize, cx: &mut Context<'_, Self>) {
        if self.active_prompt().is_some() {
            return;
        }
        let Some(page) = self.find.hits.select(row).map(|hit| hit.page) else {
            return;
        };
        let row = self.find.hits.active_index().unwrap_or(row);
        self.find.results.reveal(row);
        self.reveal_active_hit();
        log::info!("search result {}: page {}", row + 1, page.display_number());
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_above_the_view_keep_the_view() {
        let pitch = row_pitch(1.0);
        assert_eq!(pitch, ROW_HEIGHT);
        assert_eq!(top_row(0.0, pitch), 0);
        assert_eq!(top_row(-56.0, pitch), 0);
        // A sliver of a pixel of the row above does not count.
        assert_eq!(top_row(-57.0, pitch), 1);
        assert_eq!(top_row(-58.0, pitch), 1);
        assert_eq!(top_row(-600.0, pitch), 10);
        // Never negative or nonsense.
        assert_eq!(top_row(30.0, pitch), 0);
        assert_eq!(top_row(f32::NAN, pitch), 0);
        assert_eq!(top_row(f32::NEG_INFINITY, pitch), 0);
        // Three rows added above row 10: row 10 is now row 13, still on top.
        let y = anchored(-600.0, 3, 1.0);
        assert_eq!(y, -600.0 - 3.0 * ROW_HEIGHT);
        assert_eq!(top_row(y, pitch), 13);
        assert_eq!(anchored(0.0, 0, 1.0), 0.0);
    }

    #[test]
    fn rows_are_whole_device_pixels_apart() {
        // GPUI rounds 72.5 and 101.5 device pixels toward zero.
        assert!((row_pitch(1.25) - 57.6).abs() < 1e-4);
        assert!((row_pitch(1.75) - 101.0 / 1.75).abs() < 1e-4);
        for scale in [1.0, 1.5, 2.0] {
            assert_eq!(row_pitch(scale), ROW_HEIGHT, "{scale}");
        }
        for scale in [1.0_f32, 1.25, 1.5, 1.75, 2.0, 2.25, 3.0] {
            let device = row_pitch(scale) * scale;
            assert!((device - device.round()).abs() < 1e-3, "{scale}: {device}");
            assert!(device <= ROW_HEIGHT * scale + 0.5, "{scale}: {device}");
        }
        assert_eq!(row_pitch(0.0), ROW_HEIGHT);
        assert_eq!(row_pitch(f32::NAN), ROW_HEIGHT);
    }

    /// The streaming case: a search from page 20 of the search fixture adds
    /// 1,624 rows above the first page's rows, 30 at a time; the longest
    /// lists hold 20,000 rows. The row that was on top stays on top, where
    /// the list draws it (rows a measured pitch apart, `f32` as GPUI keeps
    /// them), within a fraction of a pixel.
    #[test]
    fn many_rows_added_above_keep_the_top_row_at_any_scale() {
        for scale in [1.0_f32, 1.25, 1.5, 1.75, 2.0] {
            let pitch = row_pitch(scale);
            for (batches, size) in [(54, 30), (689, 29)] {
                // Scrolled so that row 3 is on top.
                let mut y = -3.0 * pitch;
                let mut top = 3;
                for _ in 0..batches {
                    assert_eq!(top_row(y, pitch), top, "{scale}");
                    y = anchored(y, size, scale);
                    top += size;
                }
                assert_eq!(top_row(y, pitch), top, "{scale} after {top} rows");
                let drawn = y + top as f32 * pitch;
                assert!(drawn.abs() < 0.5, "{scale} after {top} rows: {drawn}");
            }
        }
        // The pitch the list was laid out with matters: anchoring 1,624 rows
        // with 58 px where rows are 57.6 px apart moves the top row 11 rows.
        let y = -1624.0 * ROW_HEIGHT;
        assert_eq!(top_row(y, row_pitch(1.25)), 1635);
        // The view may start partway into the top row; it still does after.
        let y = anchored(-3.0 * row_pitch(1.25) - 20.0, 29, 1.25);
        let drawn = y + 32.0 * row_pitch(1.25);
        assert!((drawn + 20.0).abs() < 0.5, "{drawn}");
    }

    #[test]
    fn rows_are_keyed_by_their_hit() {
        let key = |page, start| row_key(PageIndex::new(page), start);
        assert_ne!(key(1, 0), key(0, 1));
        assert_ne!(key(20, 9), key(20, 10));
        assert_eq!(key(20, 9), key(20, 9));
        assert_ne!(key(u32::MAX, 0), key(u32::MAX - 1, 0));
        assert_ne!(key(0, usize::MAX), key(0, usize::MAX - 1));
    }

    #[test]
    fn invalid_snippet_ranges_get_no_highlight() {
        let snippet = fastpdf_search::Snippet {
            text: "公文".into(),
            hit_start: 1,
            hit_end: 3,
        };
        assert_eq!(snippet.hit(), None);
    }

    #[test]
    fn captions_in_both_languages() {
        let en = crate::i18n::Language::English.strings();
        let zh = crate::i18n::Language::TraditionalChinese.strings();
        assert_eq!(zh.result_page(4, Some("iv")), "iv（第 4 頁）");
        assert_eq!(zh.result_page(20, None), "第 20 頁");
        assert_eq!(en.result_page(4, Some("iv")), "iv (page 4)");
        assert_eq!(en.result_page(20, None), "Page 20");
    }
}
