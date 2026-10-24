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
    AnyElement, ClickEvent, Context, Div, FocusHandle, FontWeight, HighlightStyle,
    InteractiveElement, IntoElement, ParentElement, ScrollStrategy, SharedString,
    StatefulInteractiveElement, Styled, StyledText, UniformListScrollHandle, Window, div, point,
    prelude::FluentBuilder, px, uniform_list,
};

use crate::hit_list::Inserted;
use crate::reader::ReaderView;
use crate::sidebar::SidebarTab;
use crate::theme::{PageHighlights, Theme};

/// Height of a row: the page line and two lines of context.
pub(crate) const ROW_HEIGHT: f32 = 58.0;
/// Line heights in a row: 4 + 15 + 2 x 17 + 4 = 57 of [`ROW_HEIGHT`].
const CAPTION_LINE: f32 = 15.0;
const CONTEXT_LINE: f32 = 17.0;

/// Scroll position and keyboard focus of the result list.
pub(crate) struct ResultsList {
    pub scroll: UniformListScrollHandle,
    pub focus: FocusHandle,
}

impl ResultsList {
    pub(crate) fn new(focus: FocusHandle) -> Self {
        Self {
            scroll: UniformListScrollHandle::new(),
            focus,
        }
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
        if rows_before > 0 && inserted.at <= top_row(y) {
            state
                .base_handle
                .set_offset(point(offset.x, px(anchored(y, inserted.count))));
        }
    }

    pub(crate) fn is_focused(&self, window: &Window) -> bool {
        self.focus.is_focused(window)
    }
}

/// The first row at least partly in view at scroll offset `y` (zero or
/// negative, as GPUI keeps it).
fn top_row(y: f32) -> usize {
    let scrolled = -y / ROW_HEIGHT;
    if scrolled.is_finite() && scrolled > 0.0 {
        scrolled.floor() as usize
    } else {
        0
    }
}

/// The offset that keeps the rows in view where they are after `rows` rows
/// were added above them.
fn anchored(y: f32, rows: usize) -> f32 {
    y - rows as f32 * ROW_HEIGHT
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
    pub(crate) fn keep_keyboard_in_view(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let shown = self.sidebar.open
            && self.sidebar.tab == SidebarTab::Results
            && self.active_prompt().is_none();
        if !shown && self.find.results.is_focused(window) {
            window.focus(&self.focus, cx);
            self.refocus_password_field(window, cx);
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
                let caption = self.result_caption(hit.page);
                let context = &hit.context;
                let text = StyledText::new(SharedString::from(context.text.clone()))
                    .with_highlights(context.hit().map(|range| (range, highlight)));
                let selected = active == Some(ix);
                let hover = theme.button_hover;
                Some(
                    div()
                        .id(("search-result", ix))
                        .h(px(ROW_HEIGHT))
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
                            this.open_result(ix, cx);
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
        assert_eq!(top_row(0.0), 0);
        assert_eq!(top_row(-57.0), 0);
        assert_eq!(top_row(-58.0), 1);
        assert_eq!(top_row(-600.0), 10);
        // Never negative or nonsense.
        assert_eq!(top_row(30.0), 0);
        assert_eq!(top_row(f32::NAN), 0);
        assert_eq!(top_row(f32::NEG_INFINITY), 0);
        // Three rows added above row 10: row 10 is now row 13, still on top.
        let y = anchored(-600.0, 3);
        assert_eq!(y, -600.0 - 3.0 * ROW_HEIGHT);
        assert_eq!(top_row(y), 13);
        assert_eq!(anchored(0.0, 0), 0.0);
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
