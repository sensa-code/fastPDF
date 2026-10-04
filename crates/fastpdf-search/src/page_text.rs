use std::ops::Range;

use fastpdf_engine_api::{CharPos, LaidText, PageRect, TextLayer};

use crate::matcher::{Normalizer, SPACE};

/// A page's text in reading order, as [`crate::Matcher`] compares it, and
/// where each character sits on the page. Small enough to keep next to the
/// text layer in the text cache (a few percent of the layer for Latin
/// text), so repeated searches skip the layout.
#[derive(Debug, Default)]
pub(crate) struct PageText {
    /// Normalized tokens: [`SPACE`] between words, letters folded unless
    /// the search is case-sensitive.
    text: String,
    /// Where the tokens come from, in `text` order.
    runs: Vec<Run>,
}

/// Tokens from one span on one line whose characters follow each other in
/// the span: token `k` of the run is `char` `ch + k` of the span. A
/// [`SPACE`] token counts like the whitespace character it stands for; its
/// position is never used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Run {
    /// Byte offset of its first token in `text`.
    start: usize,
    /// Line in reading order.
    line: usize,
    span: usize,
    ch: usize,
}

impl PageText {
    /// `layer`'s text laid out the way copy writes it (lines rebuilt from
    /// the characters' geometry, `TextLayer::lay_out_lines`), then
    /// normalized for matching.
    pub(crate) fn new(layer: &TextLayer, case_sensitive: bool) -> Self {
        let mut out = Self::default();
        let mut norm = Normalizer::new(case_sensitive);
        let mut line = 0;
        // Tokens in the last run so far.
        let mut run_len = 0;
        layer.lay_out_lines(CharPos::START..CharPos::END, |piece| match piece {
            LaidText::Char { ch, at } if !ch.is_whitespace() => {
                let (space, token) = norm.next(ch);
                if space {
                    out.text.push(SPACE);
                    run_len += 1;
                }
                let continues = out.runs.last().is_some_and(|r| {
                    r.line == line && r.span == at.span && r.ch.checked_add(run_len) == Some(at.ch)
                });
                if !continues {
                    out.runs.push(Run {
                        start: out.text.len(),
                        line,
                        span: at.span,
                        ch: at.ch,
                    });
                    run_len = 0;
                }
                out.text.push(token);
                run_len += 1;
            }
            LaidText::Char { .. } | LaidText::Space => norm.gap(),
            LaidText::Break(_) => {
                norm.gap();
                line += 1;
            }
        });
        out.text.shrink_to_fit();
        out.runs.shrink_to_fit();
        out
    }

    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    /// Bytes on the heap (the text cache weighs entries with it).
    pub(crate) fn heap_bytes(&self) -> usize {
        self.text.capacity() + self.runs.capacity() * std::mem::size_of::<Run>()
    }

    /// Highlight of the tokens at bytes `range` of [`PageText::text`]: one
    /// rectangle per run of characters of one span on one line, the union
    /// of their boxes (an even split of the span box when the engine gives
    /// no per-char boxes). Whitespace between them is covered; line breaks
    /// are not.
    pub(crate) fn rects(&self, layer: &TextLayer, range: Range<usize>) -> Vec<PageRect> {
        let mut rects: Vec<(usize, usize, PageRect)> = Vec::new();
        let Some(tokens) = self.text.get(range.clone()) else {
            return rects.into_iter().map(|(_, _, r)| r).collect();
        };
        // The run of the first token, and the token's index in it.
        let mut run = self.runs.partition_point(|r| r.start <= range.start);
        let mut k = run
            .checked_sub(1)
            .and_then(|i| self.runs.get(i))
            .and_then(|r| self.text.get(r.start..range.start))
            .map_or(0, |before| before.chars().count());
        for (offset, token) in tokens.char_indices() {
            let at = range.start + offset;
            if self.runs.get(run).is_some_and(|r| r.start <= at) {
                run += 1;
                k = 0;
            }
            let Some(r) = run.checked_sub(1).and_then(|i| self.runs.get(i)) else {
                continue;
            };
            let ch = r.ch + k;
            k += 1;
            let Some(span) = layer.spans.get(r.span).filter(|_| token != SPACE) else {
                continue;
            };
            let rect = span.char_rect(ch);
            match rects.last_mut() {
                Some((line, s, union)) if *line == r.line && *s == r.span => {
                    *union = union.union(rect);
                }
                _ => rects.push((r.line, r.span, rect)),
            }
        }
        rects.into_iter().map(|(_, _, r)| r).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fastpdf_engine_api::{PageIndex, TextSpan};

    /// Monospaced test text at (x, y): every character 10 pt square.
    fn span(text: &str, x: f32, y: f32) -> TextSpan {
        let n = text.chars().count() as f32;
        TextSpan {
            text: text.into(),
            bounds: PageRect::new(x, y, x + 10.0 * n, y + 10.0),
            char_bounds: Vec::new(),
        }
    }

    fn layer(spans: Vec<TextSpan>) -> TextLayer {
        TextLayer {
            page: PageIndex::FIRST,
            spans,
        }
    }

    fn runs(t: &PageText) -> Vec<(usize, usize, usize, usize)> {
        t.runs
            .iter()
            .map(|r| (r.start, r.line, r.span, r.ch))
            .collect()
    }

    #[test]
    fn runs_follow_spans_lines_and_dropped_whitespace() {
        let l = layer(vec![
            span("Ab cd", 0.0, 0.0),
            span("主  旨", 0.0, 14.0),
            span("x", 0.0, 28.0),
        ]);
        let t = PageText::new(&l, false);
        // No space next to CJK, not even across a line break.
        assert_eq!(t.text(), "ab cd主旨x");
        // "ab cd" is one run (the space is the span's own), "主" and "旨"
        // are apart in their span, "x" is on the next line.
        assert_eq!(
            runs(&t),
            vec![(0, 0, 0, 0), (5, 1, 1, 0), (8, 1, 1, 3), (11, 2, 2, 0)]
        );
        assert_eq!(PageText::new(&l, true).text(), "Ab cd主旨x");
    }

    #[test]
    fn rects_map_bytes_back_to_characters() {
        let l = layer(vec![span("Ab cd", 0.0, 0.0), span("主  旨", 0.0, 14.0)]);
        let t = PageText::new(&l, false);
        let at = |s: &str| {
            let start = t.text().find(s).unwrap();
            start..start + s.len()
        };
        // Within a run, starting mid-run.
        assert_eq!(
            t.rects(&l, at("b cd")),
            vec![PageRect::new(10.0, 0.0, 50.0, 10.0)]
        );
        // Across the line break, and across a dropped blank in one span.
        assert_eq!(
            t.rects(&l, at("cd主旨")),
            vec![
                PageRect::new(30.0, 0.0, 50.0, 10.0),
                PageRect::new(0.0, 14.0, 40.0, 24.0),
            ]
        );
        // Out of range or not on a char boundary: nothing.
        assert!(t.rects(&l, 100..104).is_empty());
        assert!(t.rects(&l, 6..7).is_empty());
    }

    #[test]
    fn heap_bytes_count_text_and_runs() {
        let l = layer(vec![span("hello world", 0.0, 0.0)]);
        let t = PageText::new(&l, false);
        assert_eq!(
            t.heap_bytes(),
            "hello world".len() + std::mem::size_of::<Run>()
        );
        assert_eq!(PageText::new(&layer(Vec::new()), false).heap_bytes(), 0);
    }
}
