use std::ops::Range;

use fastpdf_engine_api::{TextLayer, is_cjk};

/// One occurrence of the query, as a range of `char` indices into the
/// searched text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Match {
    pub start: usize,
    pub end: usize,
}

/// The token that stands for a gap between words (whitespace characters
/// never become tokens of their own).
pub(crate) const SPACE: char = ' ';

/// Literal matcher over normalized text. The query and the searched text
/// are normalized the same way ([`Normalizer`]):
///
/// * Whitespace, line breaks and the gap between words drawn apart are one
///   space between two non-CJK characters, however long the run.
/// * Next to a CJK character (Han, kana, bopomofo, full-width punctuation)
///   they are nothing: CJK is written without word spaces, so a word broken
///   over two lines (「公」 / 「文」), a spaced-out title (「申　請　書」)
///   and a Latin word set with or without spaces around it (「使用 PDF」,
///   「使用PDF」) all match the query written either way.
/// * Letters match case-insensitively unless asked otherwise, with
///   single-char lowercase mappings only.
///
/// Hyphens are kept: a word hyphenated at a line end only matches with the
/// hyphen and a space (「inter- national」).
#[derive(Debug, Clone)]
pub struct Matcher {
    /// Normalized query: never empty, never starts or ends with `SPACE`.
    needle: String,
    /// Its characters, sorted, without repeats or `SPACE`: what
    /// [`Matcher::may_match`] looks for.
    distinct: Vec<char>,
    case_sensitive: bool,
}

impl Matcher {
    /// Returns `None` for queries without any non-whitespace character.
    pub fn new(query: &str, case_sensitive: bool) -> Option<Self> {
        let mut needle = String::with_capacity(query.len());
        let mut norm = Normalizer::new(case_sensitive);
        for c in query.chars() {
            if c.is_whitespace() {
                norm.gap();
                continue;
            }
            let (space, token) = norm.next(c);
            if space {
                needle.push(SPACE);
            }
            needle.push(token);
        }
        let mut distinct: Vec<char> = needle.chars().filter(|&c| c != SPACE).collect();
        distinct.sort_unstable();
        distinct.dedup();
        (!needle.is_empty()).then_some(Self {
            needle,
            distinct,
            case_sensitive,
        })
    }

    pub fn case_sensitive(&self) -> bool {
        self.case_sensitive
    }

    /// All non-overlapping matches in `text`, from left to right; ranges
    /// start at the first matched character and end after the last one.
    pub fn find_all(&self, text: &str) -> Vec<Match> {
        let mut tokens = String::with_capacity(text.len());
        // Byte offset in `tokens` and `char` index in `text` of each token.
        let mut at: Vec<(usize, usize)> = Vec::with_capacity(text.len());
        let mut norm = Normalizer::new(self.case_sensitive);
        for (i, c) in text.chars().enumerate() {
            if c.is_whitespace() {
                norm.gap();
                continue;
            }
            let (space, token) = norm.next(c);
            if space {
                at.push((tokens.len(), i));
                tokens.push(SPACE);
            }
            at.push((tokens.len(), i));
            tokens.push(token);
        }
        let char_at = |byte: usize| {
            let i = at.partition_point(|&(b, _)| b <= byte).checked_sub(1)?;
            at.get(i).map(|&(_, c)| c)
        };
        self.find_in(&tokens)
            .filter_map(|r| {
                Some(Match {
                    start: char_at(r.start)?,
                    end: char_at(r.end.checked_sub(1)?)? + 1,
                })
            })
            .collect()
    }

    /// Non-overlapping matches in normalized `tokens`, from left to right,
    /// as byte ranges.
    pub(crate) fn find_in<'a>(
        &'a self,
        tokens: &'a str,
    ) -> impl Iterator<Item = Range<usize>> + 'a {
        tokens
            .match_indices(self.needle.as_str())
            .map(|(start, found)| start..start + found.len())
    }

    /// Whether every character of the query occurs on the page. A page
    /// where one is missing cannot match, and needs no layout.
    pub(crate) fn may_match(&self, layer: &TextLayer) -> bool {
        let mut found = vec![false; self.distinct.len()];
        let mut left = self.distinct.len();
        for span in &layer.spans {
            for c in span.text.chars() {
                let Ok(i) = self.distinct.binary_search(&fold(c, self.case_sensitive)) else {
                    continue;
                };
                if let Some(seen) = found.get_mut(i)
                    && !*seen
                {
                    *seen = true;
                    left -= 1;
                    if left == 0 {
                        return true;
                    }
                }
            }
        }
        left == 0
    }
}

/// Turns text into the tokens queries and pages are matched as (see
/// [`Matcher`]): characters case-folded, and one [`SPACE`] for a gap
/// between two non-CJK characters. Feed it the text in order: whitespace
/// and breaks as [`Normalizer::gap`], other characters as
/// [`Normalizer::next`].
#[derive(Debug, Clone)]
pub(crate) struct Normalizer {
    case_sensitive: bool,
    /// Whitespace or a break since the last character.
    gap: bool,
    /// The last character (not folded).
    prev: Option<char>,
}

impl Normalizer {
    pub(crate) fn new(case_sensitive: bool) -> Self {
        Self {
            case_sensitive,
            gap: false,
            prev: None,
        }
    }

    /// Whitespace, a line break, or a gap between words.
    pub(crate) fn gap(&mut self) {
        self.gap = true;
    }

    /// The next non-whitespace character: whether a [`SPACE`] token goes
    /// before it, and its own token.
    pub(crate) fn next(&mut self, c: char) -> (bool, char) {
        let space = self.gap && self.prev.is_some_and(|p| !is_cjk(p) && !is_cjk(c));
        self.gap = false;
        self.prev = Some(c);
        (space, fold(c, self.case_sensitive))
    }
}

fn fold(c: char, case_sensitive: bool) -> char {
    if case_sensitive {
        return c;
    }
    if c.is_ascii() {
        return c.to_ascii_lowercase();
    }
    let mut lower = c.to_lowercase();
    match (lower.next(), lower.next()) {
        (Some(l), None) => l,
        _ => c, // multi-char mappings (e.g. 'İ') stay as-is to keep indices 1:1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn find(q: &str, text: &str, cs: bool) -> Vec<(usize, usize)> {
        Matcher::new(q, cs)
            .map(|m| {
                m.find_all(text)
                    .into_iter()
                    .map(|m| (m.start, m.end))
                    .collect()
            })
            .unwrap_or_default()
    }

    #[test]
    fn case_insensitive_by_default() {
        assert_eq!(
            find("pdf", "FastPDF reads PDFs", false),
            vec![(4, 7), (14, 17)]
        );
        assert_eq!(find("pdf", "FastPDF reads PDFs", true), vec![]);
    }

    #[test]
    fn whitespace_runs_match_line_breaks() {
        let text = "Never render\n  what the user\ncannot see";
        assert_eq!(find("render what", text, false), vec![(6, 19)]);
        assert_eq!(find("user cannot", text, false), vec![(24, 35)]);
    }

    #[test]
    fn cjk_matches_by_character() {
        let text = "受文者：衛生福利部\n主旨：檢送醫療報告";
        assert_eq!(find("醫療報告", text, false), vec![(15, 19)]);
        assert_eq!(find("衛生", text, false), vec![(4, 6)]);
    }

    #[test]
    fn matches_do_not_overlap_and_empty_queries_are_rejected() {
        assert_eq!(find("aa", "aaaa", false), vec![(0, 2), (2, 4)]);
        assert!(Matcher::new("   ", false).is_none());
    }

    #[test]
    fn multi_char_lowercase_keeps_indices() {
        // 'İ' lowercases to two chars; it must not shift later matches.
        assert_eq!(find("x", "İx", false), vec![(1, 2)]);
    }

    #[test]
    fn breaks_and_spaces_between_cjk_are_nothing() {
        // A word broken over two lines, and the query typed either way.
        assert_eq!(find("公文", "並以公\n文或電子郵件", false), vec![(2, 5)]);
        assert_eq!(find("公 文", "並以公文", false), vec![(2, 4)]);
        // Spaced-out titles and headings of official letters.
        assert_eq!(find("申請書", "申　請　書", false), vec![(0, 5)]);
        assert_eq!(find("主旨", "主　　旨：", false), vec![(0, 4)]);
        // Full-width punctuation counts as CJK.
        assert_eq!(find("。下一句", "結束。\n下一句", false), vec![(2, 7)]);
    }

    #[test]
    fn latin_gaps_are_one_space() {
        let text = "the quick brown\n   fox";
        assert_eq!(find("brown fox", text, false), vec![(10, 22)]);
        // The query's whitespace is normalized the same way.
        assert_eq!(find("  brown \t\n  fox ", text, false), vec![(10, 22)]);
        // A gap is a word boundary: it neither disappears nor appears.
        assert_eq!(find("brownfox", text, false), vec![]);
        assert_eq!(find("bro wn", text, false), vec![]);
    }

    #[test]
    fn mixed_scripts_match_with_or_without_spaces() {
        // Latin next to CJK across a line break, and set solid or spaced.
        for text in [
            "本系統使用\nPDF格式",
            "本系統使用 PDF 格式",
            "本系統使用PDF格式",
        ] {
            assert_eq!(find("使用PDF格式", text, false).len(), 1, "{text:?}");
            assert_eq!(find("使用 PDF 格式", text, false).len(), 1, "{text:?}");
        }
        assert_eq!(find("第12條", "第5條及第12\n條規定", false), vec![(4, 9)]);
        // Two Latin words still need their space.
        assert_eq!(find("FastPDF測試", "FastPDF 測試", false), vec![(0, 10)]);
        assert_eq!(find("Fast PDF", "FastPDF", false), vec![]);
    }

    #[test]
    fn hyphens_are_not_joined() {
        let text = "inter-\nnational";
        assert_eq!(find("international", text, false), vec![]);
        assert_eq!(find("inter-national", text, false), vec![]);
        assert_eq!(find("inter- national", text, false), vec![(0, 15)]);
    }

    #[test]
    fn queries_normalize_like_text() {
        let m = |q: &str| Matcher::new(q, false).map(|m| m.needle);
        assert_eq!(m("  Foo \n\t BAR  "), m("foo bar"));
        assert_eq!(m("申 請　書"), m("申請書"));
        assert_eq!(m("使用 PDF"), m("使用PDF"));
        assert_ne!(m("foo bar"), m("foobar"));
        assert_eq!(m("\u{3000}\n"), None);
    }
}
