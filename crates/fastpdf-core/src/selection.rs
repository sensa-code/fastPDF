//! Text selection and copy over engine text layers (spec §8: Text
//! Selection, Copy).
//!
//! Selection follows content order (the order spans appear in the text
//! layer), which matches reading order for the vast majority of
//! single-column documents. Points are in page space ([`PageRect`]
//! coordinates), so selection is independent of zoom and rotation.
//!
//! Copy writes the selected characters by their geometry instead
//! (`copy_text`): lines rebuilt from baselines, read in the text's own
//! direction, top to bottom within a column, with word spaces and paragraph
//! breaks where the layout shows them.

mod copy_text;

use fastpdf_engine_api::{PageIndex, PageRect, TextLayer};

/// A position in a page's text: span index and `char` index within it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct TextPos {
    pub span: usize,
    pub ch: usize,
}

/// Selected text on one page.
#[derive(Debug, Clone, PartialEq)]
pub struct PageSelection {
    pub page: PageIndex,
    /// Inclusive start, exclusive end.
    pub start: TextPos,
    pub end: TextPos,
    /// Highlight rectangles, one per touched span.
    pub rects: Vec<PageRect>,
}

/// Finds the character at (or nearest to) `(x, y)` on a page.
///
/// Returns `None` only for pages without text.
pub fn hit_test(layer: &TextLayer, x: f32, y: f32) -> Option<TextPos> {
    let mut best: Option<(f32, TextPos)> = None;
    for (si, span) in layer.spans.iter().enumerate() {
        let n = span.text.chars().count();
        if n == 0 {
            continue;
        }
        for ci in 0..n {
            let rect = char_rect(layer, si, ci);
            let d = distance(rect, x, y);
            // Clicking the right half of a glyph places the caret after it.
            let pos = if d == 0.0 && x > (rect.x0 + rect.x1) / 2.0 && ci + 1 == n {
                TextPos { span: si, ch: n }
            } else {
                TextPos { span: si, ch: ci }
            };
            if d == 0.0 {
                return Some(pos);
            }
            if best.is_none_or(|(bd, _)| d < bd) {
                best = Some((d, pos));
            }
        }
    }
    best.map(|(_, p)| p)
}

/// Selection between two hit positions on the same page (any order).
pub fn select_range(layer: &TextLayer, a: TextPos, b: TextPos) -> PageSelection {
    let (start, end) = if a <= b { (a, b) } else { (b, a) };
    // Make the end exclusive and include the character under it.
    let end = advance(layer, end);
    PageSelection {
        page: layer.page,
        start,
        end,
        rects: rects(layer, start, end),
    }
}

/// Everything on the page from `from` (inclusive) to the end.
pub fn select_to_end(layer: &TextLayer, from: TextPos) -> PageSelection {
    let end = end_pos(layer);
    PageSelection {
        page: layer.page,
        start: from,
        end,
        rects: rects(layer, from, end),
    }
}

/// Everything on the page up to and including `to`.
pub fn select_from_start(layer: &TextLayer, to: TextPos) -> PageSelection {
    let start = TextPos { span: 0, ch: 0 };
    let end = advance(layer, to);
    PageSelection {
        page: layer.page,
        start,
        end,
        rects: rects(layer, start, end),
    }
}

/// The whole page.
pub fn select_all(layer: &TextLayer) -> PageSelection {
    select_to_end(layer, TextPos { span: 0, ch: 0 })
}

/// Text for the clipboard: the selected characters in reading order, one
/// line per line of text, a space between words that are apart on the page
/// (never between two CJK characters), and an empty line between
/// paragraphs. See `copy_text` for how lines and their order are found.
pub fn selected_text(layer: &TextLayer, sel: &PageSelection) -> String {
    copy_text::copy_text(layer, sel.start, sel.end)
}

/// Text of a multi-page selection, pages separated by line breaks.
pub fn selected_text_pages<'a>(
    pages: impl IntoIterator<Item = (&'a TextLayer, &'a PageSelection)>,
) -> String {
    let parts: Vec<String> = pages
        .into_iter()
        .map(|(layer, sel)| selected_text(layer, sel))
        .filter(|s| !s.is_empty())
        .collect();
    parts.join("\n")
}

fn char_rect(layer: &TextLayer, span: usize, ch: usize) -> PageRect {
    layer.spans[span].char_rect(ch)
}

fn distance(r: PageRect, x: f32, y: f32) -> f32 {
    let dx = (r.x0 - x).max(0.0).max(x - r.x1);
    let dy = (r.y0 - y).max(0.0).max(y - r.y1);
    // Vertical distance dominates: a click beside a line (in the margin or
    // past its end) picks that line before any other.
    dy * 1000.0 + dx
}

fn advance(layer: &TextLayer, p: TextPos) -> TextPos {
    let n = layer
        .spans
        .get(p.span)
        .map_or(0, |s| s.text.chars().count());
    TextPos {
        span: p.span,
        ch: (p.ch + 1).min(n),
    }
}

fn end_pos(layer: &TextLayer) -> TextPos {
    match layer.spans.len() {
        0 => TextPos { span: 0, ch: 0 },
        n => TextPos {
            span: n - 1,
            ch: layer.spans[n - 1].text.chars().count(),
        },
    }
}

fn rects(layer: &TextLayer, start: TextPos, end: TextPos) -> Vec<PageRect> {
    let mut out = Vec::new();
    for si in start.span..=end.span.min(layer.spans.len().saturating_sub(1)) {
        let Some(span) = layer.spans.get(si) else {
            break;
        };
        let n = span.text.chars().count();
        let from = if si == start.span { start.ch } else { 0 };
        let to = if si == end.span { end.ch.min(n) } else { n };
        let rect = (from..to)
            .map(|ci| char_rect(layer, si, ci))
            .reduce(PageRect::union);
        if let Some(r) = rect {
            out.push(r);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use fastpdf_engine_api::TextSpan;

    /// Two lines: "Hello world" (two spans with a gap) and "第二行".
    fn layer() -> TextLayer {
        let span = |text: &str, x: f32, y: f32| {
            let n = text.chars().count();
            TextSpan {
                text: text.into(),
                bounds: PageRect::new(x, y, x + n as f32 * 10.0, y + 12.0),
                char_bounds: (0..n)
                    .map(|i| {
                        PageRect::new(x + i as f32 * 10.0, y, x + (i + 1) as f32 * 10.0, y + 12.0)
                    })
                    .collect(),
            }
        };
        TextLayer {
            page: PageIndex::FIRST,
            spans: vec![
                span("Hello", 0.0, 0.0),
                span("world", 60.0, 0.0),
                span("第二行", 0.0, 20.0),
            ],
        }
    }

    #[test]
    fn hit_test_finds_characters_and_nearest_text() {
        let l = layer();
        assert_eq!(hit_test(&l, 15.0, 5.0), Some(TextPos { span: 0, ch: 1 }));
        assert_eq!(hit_test(&l, 75.0, 5.0), Some(TextPos { span: 1, ch: 1 }));
        // Far right of the second line: nearest is its last character.
        assert_eq!(hit_test(&l, 200.0, 25.0), Some(TextPos { span: 2, ch: 2 }));
        assert_eq!(hit_test(&TextLayer::new(PageIndex::FIRST), 0.0, 0.0), None);
    }

    #[test]
    fn range_selection_copies_with_spaces_and_line_breaks() {
        let l = layer();
        let a = hit_test(&l, 15.0, 5.0).unwrap(); // 'e'
        let b = hit_test(&l, 15.0, 25.0).unwrap(); // '二'
        // Drag direction does not matter.
        let sel = select_range(&l, b, a);
        assert_eq!(selected_text(&l, &sel), "ello world\n第二");
        assert_eq!(sel.rects.len(), 3);
        assert_eq!(sel.rects[0], PageRect::new(10.0, 0.0, 50.0, 12.0));
    }

    #[test]
    fn whole_page_and_multi_page_copy() {
        let l = layer();
        let all = select_all(&l);
        assert_eq!(selected_text(&l, &all), "Hello world\n第二行");
        let tail = select_to_end(&l, TextPos { span: 2, ch: 1 });
        let head = select_from_start(&l, TextPos { span: 0, ch: 1 });
        assert_eq!(selected_text_pages([(&l, &tail), (&l, &head)]), "二行\nHe");
    }

    #[test]
    fn span_level_geometry_is_split_evenly() {
        let mut l = layer();
        l.spans[0].char_bounds.clear();
        assert_eq!(hit_test(&l, 25.0, 5.0), Some(TextPos { span: 0, ch: 2 }));
        let sel = select_range(&l, TextPos { span: 0, ch: 1 }, TextPos { span: 0, ch: 2 });
        assert_eq!(sel.rects, vec![PageRect::new(10.0, 0.0, 30.0, 12.0)]);
    }
}
