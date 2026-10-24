//! A line of context around a hit, for a list of results: the page's own
//! characters (case and width as printed), a few words before the hit and
//! the text after it: at most [`MAX_CHARS`] characters of the page, and a
//! space where the page has one or a line ends next to Latin text (matching
//! drops those next to CJK). Snippets are made of hostile document text, so
//! they are made safe to show (`fastpdf_engine_api::display`) and bounded
//! in length, which also bounds the cost of shaping them.

use std::ops::Range;
use std::sync::Arc;

use fastpdf_engine_api::display::display_char;
use fastpdf_engine_api::{TextLayer, is_cjk};

use crate::fold::{fold, is_word_char};
use crate::matcher::SPACE;
use crate::page_text::{PageText, Source};

/// Context before a hit, in ems: a CJK character counts 1, any other 0.55
/// (about eleven Latin letters or six Chinese characters).
const BEFORE_EMS: f32 = 6.0;
/// Characters of a snippet, context included; a longer hit is cut.
pub(crate) const MAX_CHARS: usize = 80;
/// Marks text cut off before or after the snippet.
const ELLIPSIS: char = '\u{2026}';
/// Characters [`Originals`] may step over to find the ones of one snippet;
/// past it, the snippet shows the matched tokens instead.
const STEP_BUDGET: usize = 8192;

/// A hit in its context: `text` with the hit at bytes `hit_start..hit_end`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Snippet {
    pub text: Arc<str>,
    pub hit_start: u16,
    pub hit_end: u16,
}

impl Snippet {
    /// The hit's byte range in `text`, when it is one: not empty, in bounds
    /// and on character boundaries (safe to slice and to highlight).
    pub fn hit(&self) -> Option<Range<usize>> {
        let range = usize::from(self.hit_start)..usize::from(self.hit_end);
        (range.start < range.end
            && self.text.is_char_boundary(range.start)
            && self.text.is_char_boundary(range.end))
        .then_some(range)
    }

    /// Bytes on the heap: the text and the `Arc`'s counters.
    pub fn heap_bytes(&self) -> usize {
        self.text.len() + 2 * std::mem::size_of::<usize>()
    }
}

/// Reads a page's original characters by span and `char` index, moving
/// forward from where the last one was found. Hits come in reading order,
/// so finding the characters of all snippets of a page usually walks each
/// span once; [`STEP_BUDGET`] bounds the work for one snippet on pages
/// whose reading order runs backwards through long spans.
pub(crate) struct Originals<'a> {
    layer: &'a TextLayer,
    span: usize,
    ch: usize,
    byte: usize,
    steps: usize,
}

impl<'a> Originals<'a> {
    pub(crate) fn new(layer: &'a TextLayer) -> Self {
        Self {
            layer,
            span: 0,
            ch: 0,
            byte: 0,
            steps: 0,
        }
    }

    /// `char` `ch` of span `span`, if it exists and is found within budget.
    fn get(&mut self, span: usize, ch: usize) -> Option<char> {
        let text = self.layer.spans.get(span)?.text.as_str();
        if span != self.span || ch < self.ch.saturating_sub(ch) {
            self.span = span;
            self.ch = 0;
            self.byte = 0;
        }
        // Back to an earlier character (the context of the next hit on the
        // same line usually starts before the end of the last snippet).
        while self.ch > ch {
            if self.steps >= STEP_BUDGET {
                return None;
            }
            self.steps += 1;
            let c = text.get(..self.byte)?.chars().next_back()?;
            self.byte -= c.len_utf8();
            self.ch -= 1;
        }
        while self.ch < ch {
            if self.steps >= STEP_BUDGET {
                return None;
            }
            self.steps += 1;
            let c = text.get(self.byte..)?.chars().next()?;
            self.byte += c.len_utf8();
            self.ch += 1;
        }
        text.get(self.byte..)?.chars().next()
    }
}

/// The snippet of the hit at bytes `hit` of `text`; `originals` reads the
/// layer `text` was made of, and is shared by the snippets of a page.
pub(crate) fn snippet(
    text: &PageText,
    originals: &mut Originals<'_>,
    hit: Range<usize>,
) -> Snippet {
    let tokens = text.text();
    if hit.start >= hit.end || tokens.get(hit.clone()).is_none() {
        return Snippet::default();
    }
    let (start, cut_before) = context_start(tokens, hit.start);
    let end = start
        + tokens
            .get(start..)
            .unwrap_or_default()
            .chars()
            .take(MAX_CHARS)
            .map(char::len_utf8)
            .sum::<usize>();
    let cut_after = end < tokens.len();

    originals.steps = 0;
    let mut out = String::with_capacity(end - start + 2 * ELLIPSIS.len_utf8());
    if cut_before {
        out.push(ELLIPSIS);
    }
    let (mut hit_start, mut hit_end) = (None, None);
    // The last character shown, and where it came from.
    let mut last: Option<(Source, char)> = None;
    for source in text.sources(start..end) {
        if hit_end.is_none() && source.at >= hit.end {
            hit_end = Some(out.len());
        }
        let shown = if source.token == SPACE {
            ' '
        } else {
            let original = originals
                .get(source.span, source.ch)
                // The character the token was made of; anything else (a
                // layer that changed shape) shows the token.
                .filter(|&c| fold(c, false) == fold(source.token, false))
                .unwrap_or(source.token);
            if let Some((before, shown_before)) = last
                && spaced(originals, (before, shown_before), (source, original))
            {
                push_space(&mut out);
            }
            last = Some((source, original));
            original
        };
        if hit_start.is_none() && source.at >= hit.start {
            hit_start = Some(out.len());
        }
        match display_char(shown) {
            Some(' ') => push_space(&mut out),
            Some(c) => out.push(c),
            None => {}
        }
    }
    let hit_end = hit_end.unwrap_or(out.len());
    let hit_start = hit_start.unwrap_or(hit_end);
    if cut_after {
        out.push(ELLIPSIS);
    }
    Snippet {
        hit_start: u16::try_from(hit_start).unwrap_or(0),
        hit_end: u16::try_from(hit_end).unwrap_or(0),
        text: Arc::from(out),
    }
}

/// One space, never two, never first.
fn push_space(out: &mut String) {
    if !out.is_empty() && !out.ends_with(' ') {
        out.push(' ');
    }
}

/// Whether the page separates the characters `before` and `next` (shown as
/// `a` and `b`) where matching sees nothing between them: a line ends
/// between them and one is not CJK, or the page has whitespace between
/// them in their span (「SEARCH 搜尋」). Two CJK characters a line break
/// apart stay together (「公」 / 「文」).
fn spaced(originals: &mut Originals<'_>, before: (Source, char), next: (Source, char)) -> bool {
    let ((before, a), (next, b)) = (before, next);
    if before.line != next.line {
        return !(is_cjk(a) && is_cjk(b));
    }
    before.span == next.span
        && next.ch > before.ch.saturating_add(1)
        && originals
            .get(before.span, before.ch.saturating_add(1))
            .is_some_and(char::is_whitespace)
}

/// Where the context before the hit at byte `hit` starts, and whether text
/// before it is left out: up to [`BEFORE_EMS`] back, then, when that cuts
/// a word, forward to the start of the next one.
fn context_start(tokens: &str, hit: usize) -> (usize, bool) {
    let Some(before) = tokens.get(..hit) else {
        return (hit, false);
    };
    let mut start = hit;
    let mut first = None;
    let mut ems = 0.0;
    // Just after the earliest token of the context that is not part of a
    // word.
    let mut word_start = None;
    for (at, c) in before.char_indices().rev() {
        ems += if is_cjk(c) { 1.0 } else { 0.55 };
        if ems > BEFORE_EMS {
            let inside_word = is_word_char(c) && first.is_some_and(is_word_char);
            let start = if inside_word {
                word_start.unwrap_or(start)
            } else {
                start
            };
            // Nor does it start with the punctuation after the cut.
            let start = before
                .get(start..)
                .and_then(|rest| rest.char_indices().find(|(_, c)| c.is_alphanumeric()))
                .map_or(hit, |(offset, _)| start + offset);
            return (start, true);
        }
        start = at;
        first = Some(c);
        if !is_word_char(c) {
            word_start = Some(at + c.len_utf8());
        }
    }
    (start, false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Matcher;
    use fastpdf_engine_api::{PageIndex, PageRect, TextSpan};

    fn line(text: &str, y: f32) -> TextSpan {
        let n = text.chars().count() as f32;
        TextSpan {
            text: text.into(),
            bounds: PageRect::new(0.0, y, 10.0 * n, y + 10.0),
            char_bounds: Vec::new(),
        }
    }

    fn layer(spans: Vec<TextSpan>) -> TextLayer {
        TextLayer {
            page: PageIndex::FIRST,
            spans,
        }
    }

    /// The snippets of every hit of `query`, as (text, hit text).
    fn snippets(layer: &TextLayer, query: &str) -> Vec<(String, String)> {
        let matcher = Matcher::new(query, false).unwrap();
        let text = PageText::new(layer, false);
        let mut originals = Originals::new(layer);
        matcher
            .find_in(text.text(), text.splits())
            .map(|hit| {
                let s = snippet(&text, &mut originals, hit);
                let found = s.hit().map(|r| s.text[r].to_string()).unwrap_or_default();
                (s.text.to_string(), found)
            })
            .collect()
    }

    #[test]
    fn snippets_keep_the_original_characters() {
        let l = layer(vec![line("請使用ＦａｓｔＰＤＦ閱讀", 0.0)]);
        assert_eq!(
            snippets(&l, "fastpdf"),
            vec![("請使用ＦａｓｔＰＤＦ閱讀".into(), "ＦａｓｔＰＤＦ".into())]
        );
        let l = layer(vec![line("FastPDF Reads PDF", 0.0)]);
        assert_eq!(
            snippets(&l, "pdf"),
            vec![
                ("FastPDF Reads PDF".into(), "PDF".into()),
                ("\u{2026}Reads PDF".into(), "PDF".into())
            ]
        );
    }

    #[test]
    fn snippets_keep_the_spaces_of_mixed_text() {
        // Matching drops the space next to CJK; the snippet shows it, and a
        // line break between CJK and Latin text as a space.
        let l = layer(vec![
            line("search SEARCH 搜尋結果列表測試", 0.0),
            line("search Search", 14.0),
        ]);
        assert_eq!(
            snippets(&l, "search")[1],
            (
                "search SEARCH 搜尋結果列表測試 search Search".into(),
                "SEARCH".into()
            )
        );
        assert_eq!(
            snippets(&l, "SEARCH搜尋")[0].1,
            "SEARCH 搜尋",
            "the space is shown inside the hit"
        );
        // Spaced-out titles show as printed.
        let l = layer(vec![line("申 請 書", 0.0)]);
        assert_eq!(snippets(&l, "申請書")[0].0, "申 請 書");
    }

    #[test]
    fn snippets_collapse_breaks_and_spaces() {
        let l = layer(vec![
            line("the quick brown", 0.0),
            line("   fox jumps", 14.0),
        ]);
        assert_eq!(
            snippets(&l, "fox"),
            vec![("\u{2026}brown fox jumps".into(), "fox".into())]
        );
        // A line break between Chinese characters is nothing.
        let l = layer(vec![line("並以公", 0.0), line("文或電子郵件", 14.0)]);
        assert_eq!(
            snippets(&l, "公文"),
            vec![("並以公文或電子郵件".into(), "公文".into())]
        );
    }

    #[test]
    fn context_starts_at_a_word_and_is_marked_when_cut() {
        let l = layer(vec![line(
            "one two three four five six seven eight needle after",
            0.0,
        )]);
        let found = snippets(&l, "needle");
        assert_eq!(found[0].0, "\u{2026}eight needle after");
        assert_eq!(found[0].1, "needle");
        // Chinese: six characters before.
        let l = layer(vec![line("一二三四五六七八九十目標之後", 0.0)]);
        assert_eq!(snippets(&l, "目標")[0].0, "\u{2026}五六七八九十目標之後");
        // Punctuation ends a word too; a cut between words stays, without
        // the punctuation after it.
        let l = layer(vec![line(
            "research, searching, search-engine, (search)",
            0.0,
        )]);
        assert_eq!(snippets(&l, "search")[3].0, "\u{2026}engine, (search)");
    }

    #[test]
    fn snippets_are_bounded() {
        let long = format!("{}needle{}", "a".repeat(5000), "b".repeat(5000));
        let l = layer(vec![line(&long, 0.0)]);
        let found = snippets(&l, "needle");
        let (text, hit) = &found[0];
        assert!(text.chars().count() <= 2 * MAX_CHARS + 2, "{}", text.len());
        assert!(text.starts_with('\u{2026}') && text.ends_with('\u{2026}'));
        assert_eq!(hit, "needle");
        // A hit longer than a snippet is cut, and still marked.
        let l = layer(vec![line(&"x".repeat(300), 0.0)]);
        let s = snippets(&l, &"x".repeat(200));
        assert!(s[0].0.chars().count() <= MAX_CHARS + 2);
        assert_eq!(s[0].1.chars().count(), MAX_CHARS);
    }

    #[test]
    fn snippets_are_safe_to_show() {
        let l = layer(vec![line("abc\u{202E}def\u{0}\u{1}needle\u{200B}gh", 0.0)]);
        let found = snippets(&l, "needle");
        let (text, hit) = &found[0];
        assert!(
            !text.contains(['\u{202E}', '\u{0}', '\u{1}', '\u{200B}']),
            "{text:?}"
        );
        assert_eq!(text, "abcdef needlegh");
        assert_eq!(hit, "needle");
    }

    #[test]
    fn hostile_layers_do_not_panic() {
        let bad = PageRect {
            x0: f32::NAN,
            y0: f32::INFINITY,
            x1: f32::NEG_INFINITY,
            y1: f32::NAN,
        };
        let l = layer(vec![
            TextSpan {
                text: "needle".into(),
                bounds: bad,
                char_bounds: vec![bad; 3],
            },
            TextSpan::default(),
            line("needle", 20.0),
        ]);
        assert_eq!(snippets(&l, "needle").len(), 2);
        // Ranges that are no hit: nothing, never a panic.
        let text = PageText::new(&l, false);
        let mut originals = Originals::new(&l);
        for (start, end) in [(0, 0), (3, 1), (2, 9999), (9999, 10000)] {
            let s = snippet(&text, &mut originals, start..end);
            assert_eq!(s.hit(), None);
        }
        // Original characters that cannot be found show the tokens.
        let mut originals = Originals::new(&l);
        assert_eq!(originals.get(99, 0), None);
        assert_eq!(originals.get(0, 99), None);
        assert_eq!(originals.get(0, 2), Some('e'));
        // Forward, then back a little.
        assert_eq!(originals.get(0, 5), Some('e'));
        assert_eq!(originals.get(0, 3), Some('d'));
        assert_eq!(originals.get(0, 0), Some('n'));
    }

    #[test]
    fn hit_ranges_are_checked() {
        let s = Snippet {
            text: Arc::from("公文"),
            hit_start: 0,
            hit_end: 3,
        };
        assert_eq!(s.hit(), Some(0..3));
        for (start, end) in [(0, 2), (1, 3), (3, 3), (3, 0), (0, 7)] {
            let s = Snippet {
                hit_start: start,
                hit_end: end,
                ..s.clone()
            };
            assert_eq!(s.hit(), None, "{start}..{end}");
        }
        assert_eq!(s.heap_bytes(), 6 + 2 * std::mem::size_of::<usize>());
    }
}
