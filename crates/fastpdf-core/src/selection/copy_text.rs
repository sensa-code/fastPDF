//! Copy text: the selected characters, written the way the page lays them
//! out (spec §8: Copy).
//!
//! A selection is a range of characters in content order, and its
//! highlight shows exactly that range. Copy keeps those characters and only
//! decides their order and the whitespace between them, in the reading
//! order that search matches against as well: `TextLayer::lay_out` in
//! `fastpdf-engine-api` (`text/layout.rs`, which documents the rules) puts
//! lines together from the characters' geometry, reads them in the text's
//! own direction and each column top to bottom, puts running headers and
//! footers first and last, and adds a space between words drawn apart
//! (never between two CJK characters set solid). Copy writes `\n` between
//! lines and an empty line between paragraphs. Hyphenated words are not
//! joined: telling a line-end hyphen from a real one is too unreliable.

use fastpdf_engine_api::{CharPos, LaidText, LineBreak, TextLayer};
#[cfg(test)]
use fastpdf_engine_api::{PageRect, TextSpan, is_cjk};

use super::TextPos;

/// Text of the characters from `start` (inclusive) to `end` (exclusive).
pub(super) fn copy_text(layer: &TextLayer, start: TextPos, end: TextPos) -> String {
    let at = |p: TextPos| CharPos {
        span: p.span,
        ch: p.ch,
    };
    let mut out = String::new();
    layer.lay_out(at(start)..at(end), |piece| match piece {
        LaidText::Char { ch, .. } => out.push(ch),
        LaidText::Space => out.push(' '),
        LaidText::Break(LineBreak::Line) => out.push('\n'),
        LaidText::Break(LineBreak::Paragraph) => out.push_str("\n\n"),
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::selection::{PageSelection, select_all, select_range, selected_text};
    use fastpdf_engine_api::PageIndex;

    /// Font size of the test text: CJK characters are this wide, others
    /// half as wide.
    const EM: f32 = 10.0;

    fn advance(c: char) -> f32 {
        if is_cjk(c) { EM } else { EM / 2.0 }
    }

    fn span_of(text: &str, char_bounds: Vec<PageRect>) -> TextSpan {
        let bounds = char_bounds
            .iter()
            .copied()
            .reduce(PageRect::union)
            .unwrap_or_default();
        TextSpan {
            text: text.into(),
            bounds,
            char_bounds,
        }
    }

    /// Horizontal text with its top-left corner at (x, y), characters
    /// `tracking` points apart (letter spacing).
    fn tracked(text: &str, x: f32, y: f32, tracking: f32) -> TextSpan {
        let mut cx = x;
        let boxes = text
            .chars()
            .map(|c| {
                let r = PageRect::new(cx, y, cx + advance(c), y + EM);
                cx = r.x1 + tracking;
                r
            })
            .collect();
        span_of(text, boxes)
    }

    fn span(text: &str, x: f32, y: f32) -> TextSpan {
        tracked(text, x, y, 0.0)
    }

    /// Text running down from (x, y): vertical writing, or text turned a
    /// quarter clockwise. The next line is to the left.
    fn down(text: &str, x: f32, y: f32) -> TextSpan {
        let mut cy = y;
        let boxes = text
            .chars()
            .map(|c| {
                let r = PageRect::new(x, cy, x + EM, cy + advance(c));
                cy = r.y1;
                r
            })
            .collect();
        span_of(text, boxes)
    }

    /// Text running up from (x, y), turned a quarter counter-clockwise as
    /// on a page shown with `/Rotate 90`. The next line is to the right.
    fn up(text: &str, x: f32, y: f32) -> TextSpan {
        let mut cy = y;
        let boxes = text
            .chars()
            .map(|c| {
                let r = PageRect::new(x, cy - advance(c), x + EM, cy);
                cy = r.y0;
                r
            })
            .collect();
        span_of(text, boxes)
    }

    /// Text running right to left from (x, y): upside down. The next line
    /// is above.
    fn upside_down(text: &str, x: f32, y: f32) -> TextSpan {
        let mut cx = x;
        let boxes = text
            .chars()
            .map(|c| {
                let r = PageRect::new(cx - advance(c), y, cx, y + EM);
                cx = r.x0;
                r
            })
            .collect();
        span_of(text, boxes)
    }

    fn layer(spans: Vec<TextSpan>) -> TextLayer {
        TextLayer {
            page: PageIndex::FIRST,
            spans,
        }
    }

    /// Select all, then copy.
    fn copy(spans: Vec<TextSpan>) -> String {
        let l = layer(spans);
        selected_text(&l, &select_all(&l))
    }

    fn selection(start: (usize, usize), end: (usize, usize)) -> PageSelection {
        PageSelection {
            page: PageIndex::FIRST,
            start: TextPos {
                span: start.0,
                ch: start.1,
            },
            end: TextPos {
                span: end.0,
                ch: end.1,
            },
            rects: Vec::new(),
        }
    }

    #[test]
    fn a_latin_line_split_into_spans_is_one_line() {
        // "The quick brown fox": the last part is drawn first, and the parts
        // are a word space (3 pt) apart.
        let spans = vec![
            span("brown fox", 46.0, 0.0),
            span("The", 0.0, 0.0),
            span("quick", 18.0, 0.0),
            span("jumps over", 0.0, 14.0),
        ];
        assert_eq!(copy(spans), "The quick brown fox\njumps over");

        // Superscripts and subscripts stay on their line, unspaced.
        let small =
            |text: &str, x: f32, y: f32| span_of(text, vec![PageRect::new(x, y, x + 3.5, y + 6.0)]);
        let formula = vec![
            span("E=mc", 0.0, 30.0),
            small("2", 20.0, 27.0),
            span(" and x", 24.0, 30.0),
            small("2", 54.0, 27.0),
            small("i", 57.5, 36.0),
        ];
        assert_eq!(copy(formula), "E=mc2 and x2i");
    }

    #[test]
    fn letter_spacing_and_word_gaps() {
        // Letter spacing (here 0.15 em) is not a word space.
        assert_eq!(copy(vec![tracked("Tracked", 0.0, 0.0, 1.5)]), "Tracked");
        // Words drawn apart without a space character get one.
        assert_eq!(
            copy(vec![span("Hello", 0.0, 0.0), span("world", 28.0, 0.0)]),
            "Hello world"
        );
        // A space already there is not doubled.
        assert_eq!(
            copy(vec![span("Hello ", 0.0, 0.0), span("world", 33.0, 0.0)]),
            "Hello world"
        );
        // Kerning pulls letters together.
        let kerned = span_of(
            "AV",
            vec![
                PageRect::new(0.0, 0.0, 6.0, 10.0),
                PageRect::new(5.0, 0.0, 11.0, 10.0),
            ],
        );
        assert_eq!(copy(vec![kerned]), "AV");
        // A gap inside one span counts too (engines that add no spaces).
        let apart = span_of(
            "ab",
            vec![
                PageRect::new(0.0, 0.0, 5.0, 10.0),
                PageRect::new(9.0, 0.0, 14.0, 10.0),
            ],
        );
        assert_eq!(copy(vec![apart]), "a b");
        // Trailing whitespace is dropped; leading (indentation) is kept.
        assert_eq!(copy(vec![span("  code();   ", 0.0, 0.0)]), "  code();");
    }

    #[test]
    fn mixed_chinese_and_latin() {
        let spans = vec![
            span("中文", 0.0, 0.0),
            span("PDF", 23.0, 0.0),
            span("閱讀器", 41.0, 0.0),
        ];
        assert_eq!(copy(spans), "中文 PDF 閱讀器");
        // Set solid: no space where there is no gap.
        assert_eq!(
            copy(vec![span("使用FastPDF閱讀，", 0.0, 0.0)]),
            "使用FastPDF閱讀，"
        );
    }

    #[test]
    fn chinese_has_no_spaces_but_table_cells_stay_apart() {
        // Justified: characters 0.2 em apart.
        assert_eq!(copy(vec![tracked("政府公文", 0.0, 0.0, 2.0)]), "政府公文");
        // Separate spans less than a character apart (a font switch).
        assert_eq!(
            copy(vec![span("中華", 0.0, 0.0), span("民國", 24.0, 0.0)]),
            "中華民國"
        );
        // A space an engine put into such a gap is dropped...
        let inserted = span_of(
            "龘、 倰",
            vec![
                PageRect::new(0.0, 0.0, 10.0, 10.0),
                PageRect::new(10.0, 0.0, 15.0, 10.0),
                PageRect::new(15.0, 2.0, 20.0, 8.0),
                PageRect::new(20.0, 0.0, 30.0, 10.0),
            ],
        );
        assert_eq!(copy(vec![inserted]), "龘、倰");
        // ...but ideographic spaces are the document's own.
        assert_eq!(copy(vec![span("檔　　號：", 0.0, 0.0)]), "檔　　號：");
        // Full-width punctuation counts as CJK.
        assert_eq!(
            copy(vec![
                span("（全形）", 0.0, 0.0),
                span("「引號」", 43.0, 0.0)
            ]),
            "（全形）「引號」"
        );
        // Table cells, a character width or more apart, keep a separator.
        let row = vec![
            span("項次", 0.0, 0.0),
            span("檢測項目", 40.0, 0.0),
            span("判定", 100.0, 0.0),
        ];
        assert_eq!(copy(row), "項次 檢測項目 判定");
        assert_eq!(
            copy(vec![span("第一行", 0.0, 0.0), span("第二行", 0.0, 14.0)]),
            "第一行\n第二行"
        );
    }

    /// A heading over two columns of three lines each (same baselines),
    /// drawn column by column.
    fn two_columns() -> Vec<TextSpan> {
        let mut spans = vec![span(
            "A heading that runs over both columns of the page",
            0.0,
            0.0,
        )];
        for (x, col) in [(0.0, 1), (150.0, 2)] {
            for line in 1..=3 {
                let y = 8.0 + 12.0 * line as f32;
                spans.push(span(&format!("column {col} line {line}"), x, y));
            }
        }
        spans
    }

    const TWO_COLUMNS: &str = "A heading that runs over both columns of the page\n\n\
        column 1 line 1\ncolumn 1 line 2\ncolumn 1 line 3\n\
        column 2 line 1\ncolumn 2 line 2\ncolumn 2 line 3";

    #[test]
    fn two_columns_read_column_by_column() {
        assert_eq!(copy(two_columns()), TWO_COLUMNS);

        // From the middle of the first column into the second.
        let l = layer(two_columns());
        let sel = select_range(&l, TextPos { span: 2, ch: 0 }, TextPos { span: 4, ch: 14 });
        assert_eq!(
            selected_text(&l, &sel),
            "column 1 line 2\ncolumn 1 line 3\ncolumn 2 line 1"
        );
        assert_eq!(sel.rects.len(), 3, "the highlight is unchanged");

        // A page number centred under both columns, drawn first, and the
        // heading drawn last.
        let mut spans = two_columns();
        let heading = spans.remove(0);
        spans.insert(0, span("1", 110.0, 70.0));
        spans.push(heading);
        assert_eq!(copy(spans), format!("{TWO_COLUMNS}\n1"));
    }

    #[test]
    fn lines_of_a_column_read_top_to_bottom() {
        let spans = vec![
            span("third", 0.0, 24.0),
            span("first", 0.0, 0.0),
            span("second", 0.0, 12.0),
        ];
        assert_eq!(copy(spans), "first\nsecond\nthird");
    }

    #[test]
    fn rotated_text_reads_in_its_own_direction() {
        // Up the page, as on pages shown with /Rotate 90; drawn in either
        // order.
        let first = up("First line", 100.0, 300.0);
        let second = up("Second line", 114.0, 300.0);
        assert_eq!(
            copy(vec![first.clone(), second.clone()]),
            "First line\nSecond line"
        );
        assert_eq!(copy(vec![second, first]), "First line\nSecond line");
        // Down the page (turned clockwise): the next line is to the left.
        assert_eq!(
            copy(vec![
                down("Second line", 186.0, 0.0),
                down("First line", 200.0, 0.0)
            ]),
            "First line\nSecond line"
        );
        // Vertical writing, columns right to left, under a horizontal title.
        let page = vec![
            span("Vertical", 0.0, 0.0),
            down("直排文字", 300.0, 20.0),
            down("第二行", 286.0, 20.0),
        ];
        assert_eq!(copy(page), "Vertical\n直排文字\n第二行");
        // Upside down: right to left, the next line above.
        assert_eq!(
            copy(vec![
                upside_down("Hello", 300.0, 500.0),
                upside_down("world", 300.0, 486.0)
            ]),
            "Hello\nworld"
        );
    }

    #[test]
    fn paragraphs_are_separated_by_an_empty_line() {
        let lines: Vec<TextSpan> = [0.0, 12.0, 24.0, 48.0, 60.0]
            .into_iter()
            .enumerate()
            .map(|(i, y)| span(&format!("line {}", i + 1), 0.0, y))
            .collect();
        assert_eq!(copy(lines), "line 1\nline 2\nline 3\n\nline 4\nline 5");
        // Two lines tell nothing about the usual spacing.
        assert_eq!(
            copy(vec![span("one", 0.0, 0.0), span("two", 0.0, 24.0)]),
            "one\ntwo"
        );
        // A larger heading over its text.
        let title = span_of(
            "Title",
            (0..5)
                .map(|i| {
                    let x = 10.0 * i as f32;
                    PageRect::new(x, 0.0, x + 10.0, 20.0)
                })
                .collect(),
        );
        let body = vec![title, span("body 1", 0.0, 30.0), span("body 2", 0.0, 42.0)];
        assert_eq!(copy(body), "Title\n\nbody 1\nbody 2");
    }

    #[test]
    fn empty_and_blank_selections_copy_nothing() {
        let empty = TextLayer::new(PageIndex::FIRST);
        assert_eq!(selected_text(&empty, &select_all(&empty)), "");
        let l = layer(vec![span("a   b", 0.0, 0.0)]);
        assert_eq!(selected_text(&l, &selection((0, 2), (0, 2))), "");
        assert_eq!(
            selected_text(&l, &selection((0, 1), (0, 4))),
            "",
            "only spaces"
        );
        assert_eq!(
            selected_text(&l, &selection((3, 0), (9, 2))),
            "",
            "past the end"
        );
        assert_eq!(
            selected_text(&l, &selection((0, 4), (0, 1))),
            "",
            "reversed"
        );
    }

    #[test]
    fn hostile_geometry_and_crowded_pages_finish() {
        let bad = PageRect {
            x0: f32::NAN,
            y0: f32::INFINITY,
            x1: f32::NEG_INFINITY,
            y1: f32::NAN,
        };
        let spans = vec![
            span_of("ab", vec![bad, bad]),
            span_of("c", vec![PageRect::default()]),
            span("ok", 0.0, 0.0),
            span_of("huge", vec![PageRect::new(-1e30, -1e30, 1e30, 1e30); 4]),
        ];
        let text = copy(spans);
        assert!(text.contains("ok"), "{text:?}");

        // Thousands of scattered characters (labels of a chart): every one
        // is kept, and ordering them stays fast enough.
        let mut seed = 7u32;
        let mut next = move || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed >> 8) as f32 / (1u32 << 24) as f32
        };
        let spans: Vec<TextSpan> = (0..6000)
            .map(|_| {
                let (x, y) = (next() * 600.0, next() * 800.0);
                span("x", x, y)
            })
            .collect();
        let text = copy(spans);
        assert_eq!(text.chars().filter(|&c| c == 'x').count(), 6000);
    }

    #[test]
    fn cjk_ranges() {
        for c in "中、。「』，：Ａｶかカㄅ㈱…‧\u{20000}".chars() {
            assert!(is_cjk(c), "{c}");
        }
        // Korean separates words with spaces.
        for c in "aZ1.,éЯ한 ".chars() {
            assert!(!is_cjk(c), "{c}");
        }
    }
}
