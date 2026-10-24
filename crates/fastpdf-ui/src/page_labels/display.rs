//! How page labels look: the toolbar's indicator (`iii (3 / 120)`), the
//! scroll-bar drag label (the same text) and thumbnail captions
//! (`iii (3)`), all cut to fit so the page numbers always show.

use fastpdf_engine_api::{PageIndex, is_cjk};
use gpui::{
    Div, Font, ParentElement, Pixels, SharedString, Styled, TextRun, TruncateFrom, Window, div, px,
};

use crate::reader::ReaderView;

/// Width of a label in a caption, counting CJK characters twice; longer
/// labels are cut and end in an ellipsis.
const CAPTION_WIDTH: usize = 24;

/// Text sizes of the indicator with a label: two lines fit the toolbar, and
/// the numbers in parentheses take no more room than plain numbers.
const LABEL_TEXT: f32 = 13.0;
const NUMBERS_TEXT: f32 = 12.0;

impl ReaderView {
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

    /// A thumbnail's caption, `iii (3)` or `3` (marked when it failed),
    /// fitted into `width` in `font` at `size`: a label too wide is cut with
    /// an ellipsis so the page number and the mark always show.
    pub(crate) fn thumbnail_caption(
        &self,
        page: PageIndex,
        failed: bool,
        font: &Font,
        size: Pixels,
        width: Pixels,
        window: &Window,
    ) -> String {
        let number = page.display_number();
        let caption = |label: &str| {
            let caption = if label.is_empty() {
                number.to_string()
            } else {
                format!("{label} ({number})")
            };
            if failed {
                self.strings().thumbnail_failed(&caption)
            } else {
                caption
            }
        };
        let Some(label) = self.page_label(page).map(|l| shorten(&l, CAPTION_WIDTH)) else {
            return caption("");
        };
        let full = caption(&label);
        let run = |text: &str| TextRun {
            len: text.len(),
            font: font.clone(),
            color: gpui::black(),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let measure = |text: &str| {
            window
                .text_system()
                .shape_line(
                    SharedString::from(text.to_owned()),
                    size,
                    &[run(text)],
                    None,
                )
                .width
        };
        if measure(&full) <= width {
            return full;
        }
        // The label comes first; everything after it must stay.
        let rest = full.get(label.len()..).unwrap_or_default();
        let room = (width - measure(rest)).max(px(0.0));
        let (cut, _) = window
            .text_system()
            .line_wrapper(font.clone(), size)
            .truncate_line(
                SharedString::from(label.clone()),
                room,
                "\u{2026}",
                &[run(&label)],
                TruncateFrom::End,
            );
        format!("{cut}{rest}")
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

/// The toolbar's page indicator, `iii (3 / 120)` or `3 / 120` (at least as
/// wide as `12 / 345`). A label never pushes buttons out of the row: when
/// the row is short of room (after its end, rotation and the title, gave
/// way), the numbers wrap under the label onto a second line, and a label
/// still too wide is cut with an ellipsis. The indicator never gets
/// narrower than its numbers, so they stay whole.
pub(crate) fn indicator(view: &ReaderView, window: &Window) -> Div {
    let Some((page, count)) = view.page_indicator() else {
        return numbers_box("- / -".to_owned());
    };
    let numbers = format!("{} / {count}", page.display_number());
    let Some(label) = view.page_label(page) else {
        return numbers_box(numbers);
    };
    let numbers = format!("({numbers})");
    // The label's own width must not hold the indicator open (the row
    // would push buttons out instead): its narrowest is its numbers.
    let size = px(NUMBERS_TEXT);
    let font = gpui::font(view.language().ui_font());
    let run = TextRun {
        len: numbers.len(),
        font,
        color: gpui::black(),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let narrowest = window
        .text_system()
        .shape_line(SharedString::from(numbers.clone()), size, &[run], None)
        .width
        .ceil();
    div()
        .flex()
        .flex_row()
        .flex_wrap()
        .justify_center()
        .content_center()
        .gap_x_1()
        .min_w(narrowest + px(1.0))
        .max_w(px(220.0))
        .overflow_hidden()
        .whitespace_nowrap()
        .text_size(px(LABEL_TEXT))
        .line_height(px(16.0))
        .child(
            div()
                .min_w_0()
                .overflow_hidden()
                .text_ellipsis()
                .child(shorten(&label, CAPTION_WIDTH)),
        )
        .child(
            div()
                .flex_none()
                .text_size(size)
                .text_color(view.theme.text_muted)
                .child(numbers),
        )
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
