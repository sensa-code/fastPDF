//! Page labels in the reader (the document's own page numbering, such as
//! `iii` or `A-1`): the toolbar shows `iii (3 / 120)`, thumbnails `iii (3)`,
//! the scroll-bar drag label the same as the toolbar.
//!
//! Labels are asked for once per document, after its first page is fully
//! rendered (never while it opens), or when something needs them (go to
//! page, a dev-script step). Documents whose labels are just their page
//! numbers count as having none, so nothing extra is shown; a label equal
//! to the page number is not repeated either.

use std::sync::Arc;
use std::time::Duration;

use fastpdf_engine_api::{EngineDocument, PageIndex, PageLabels, is_cjk};
use gpui::{Context, Div, ParentElement, Styled, div, px};

use crate::doc_queries::Lazy;
use crate::reader::{DocState, ReaderView};

/// Width of a label in a caption, counting CJK characters twice; longer
/// labels are cut and end in an ellipsis.
const CAPTION_WIDTH: usize = 24;

impl ReaderView {
    /// Asks for the open document's page labels unless they are known or
    /// on their way.
    pub(crate) fn ensure_page_labels(&mut self, cx: &mut Context<'_, Self>) {
        self.load_doc_query(
            |queries| &mut queries.labels,
            |doc| {
                let labels = doc.page_labels()?;
                Ok((!labels.is_plain_numbers()).then(|| Arc::new(labels)))
            },
            log_labels,
            cx,
        );
    }

    /// On every wake-up (cheap): once the first page is fully rendered, the
    /// labels are asked for, once.
    pub(crate) fn poll_page_labels(&mut self, cx: &mut Context<'_, Self>) {
        let due = match &mut self.doc {
            DocState::Open(open) if open.exact_reported && !open.queries.labels_requested => {
                open.queries.labels_requested = true;
                true
            }
            _ => false,
        };
        if due {
            self.ensure_page_labels(cx);
        }
    }

    /// The labels, once loaded (documents with plain numbering have none).
    pub(crate) fn page_labels(&self) -> Option<&PageLabels> {
        match &self.doc_queries()?.labels {
            Lazy::Ready(labels) => Some(labels),
            _ => None,
        }
    }

    /// Whether the labels question is answered (or can no longer be).
    pub(crate) fn page_labels_settled(&self) -> bool {
        self.doc_queries().is_some_and(|q| q.labels.is_settled())
    }

    /// The label of `page` when it says something its number does not.
    pub(crate) fn page_label(&self, page: PageIndex) -> Option<String> {
        let label = self.page_labels()?.label(page)?;
        (label != page.display_number().to_string()).then_some(label)
    }

    /// The toolbar's page text: `iii (3 / 120)`, `3 / 120`, or `- / -`
    /// without a document.
    pub(crate) fn page_position_text(&self) -> String {
        match self.page_indicator() {
            Some((page, count)) => position_text(
                self.page_label(page).as_deref(),
                page.display_number(),
                count,
            ),
            None => "- / -".to_owned(),
        }
    }

    /// A thumbnail's caption: `iii (3)` or `3`, marked when it failed.
    pub(crate) fn thumbnail_caption(&self, page: PageIndex, failed: bool) -> String {
        let number = page.display_number();
        let caption = match self.page_label(page) {
            Some(label) => format!("{} ({number})", shorten(&label, CAPTION_WIDTH)),
            None => number.to_string(),
        };
        if failed {
            self.strings().thumbnail_failed(&caption)
        } else {
            caption
        }
    }
}

/// `label (number / count)`, or `number / count` without a label; the
/// label is cut to [`CAPTION_WIDTH`]. Language-neutral, like the plain
/// numbers it replaces.
pub(crate) fn position_text(label: Option<&str>, number: u32, count: u32) -> String {
    match label {
        Some(label) => format!("{} ({number} / {count})", shorten(label, CAPTION_WIDTH)),
        None => format!("{number} / {count}"),
    }
}

/// `text` cut to `width` columns (CJK characters take two) with an ellipsis.
fn shorten(text: &str, width: usize) -> String {
    let columns = |c: char| if is_cjk(c) { 2 } else { 1 };
    if text.chars().map(columns).sum::<usize>() <= width {
        return text.to_owned();
    }
    let mut out = String::new();
    let mut used = 1; // the ellipsis
    for c in text.chars() {
        used += columns(c);
        if used > width {
            break;
        }
        out.push(c);
    }
    out.push('\u{2026}');
    out
}

/// The toolbar's page indicator, `iii (3 / 120)` or `3 / 120`, at least as
/// wide as `12 / 345`. A label never pushes buttons out of the row: when
/// the row is short of room (after its end, rotation and the title, gave
/// way), the numbers wrap under the label onto a second line, and a label
/// still too wide is cut with an ellipsis. The page numbers stay whole.
pub(crate) fn indicator(view: &ReaderView) -> Div {
    let Some((page, count)) = view.page_indicator() else {
        return numbers_box("- / -".to_owned());
    };
    let numbers = format!("{} / {count}", page.display_number());
    let Some(label) = view.page_label(page) else {
        return numbers_box(numbers);
    };
    div()
        .flex()
        .flex_row()
        .flex_wrap()
        .justify_center()
        .content_center()
        .gap_x_1()
        .min_w(px(56.0))
        .max_w(px(220.0))
        .overflow_hidden()
        .whitespace_nowrap()
        .text_size(px(13.0))
        .line_height(px(16.0))
        .child(
            div()
                .min_w_0()
                .overflow_hidden()
                .text_ellipsis()
                .child(shorten(&label, CAPTION_WIDTH)),
        )
        .child(div().flex_none().child(format!("({numbers})")))
}

fn numbers_box(numbers: String) -> Div {
    div()
        .flex_none()
        .min_w(px(56.0))
        .flex()
        .justify_center()
        .whitespace_nowrap()
        .child(numbers)
}

fn log_labels(state: &Lazy<Arc<PageLabels>>, since_open: Duration) {
    let ms = since_open.as_secs_f64() * 1000.0;
    match state {
        Lazy::Ready(labels) => log::info!(
            "page labels: {} ranges, ready {ms:.1} ms after the session started",
            labels.ranges().len()
        ),
        Lazy::Absent => log::info!("page labels: none, ready {ms:.1} ms after the session started"),
        Lazy::Failed(e) => log::warn!("page labels failed: {e}"),
        Lazy::NotLoaded => log::debug!("page labels are not available yet"),
        Lazy::Loading(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn position_text_formats() {
        assert_eq!(position_text(Some("iii"), 3, 120), "iii (3 / 120)");
        assert_eq!(position_text(None, 3, 120), "3 / 120");
        assert_eq!(position_text(Some("附錄-5"), 9, 12), "附錄-5 (9 / 12)");
        let long = position_text(Some(&"x".repeat(100)), 1, 2);
        assert_eq!(long, format!("{}\u{2026} (1 / 2)", "x".repeat(23)));
    }

    #[test]
    fn labels_are_shortened_by_width() {
        assert_eq!(shorten("App-B", 24), "App-B");
        assert_eq!(shorten(&"附".repeat(12), 24), "附".repeat(12));
        // 13 wide characters do not fit in 24 columns.
        assert_eq!(
            shorten(&"附".repeat(13), 24),
            format!("{}\u{2026}", "附".repeat(11))
        );
        assert_eq!(shorten("abcdef", 4), "abc\u{2026}");
    }
}
