/// One occurrence of the query, as a range of `char` indices into the
/// searched text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Match {
    pub start: usize,
    pub end: usize,
}

/// Literal matcher with optional case folding and whitespace-insensitive
/// matching: any run of whitespace in the query matches any run of
/// whitespace (including line breaks between spans) in the text.
///
/// Works on `char`s so CJK text, which has no word boundaries, is matched
/// character by character. Case folding uses single-char lowercase mappings
/// only, keeping a 1:1 index map back to the original text.
#[derive(Debug, Clone)]
pub struct Matcher {
    needle: Vec<Token>,
    case_sensitive: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Token {
    Char(char),
    Space,
}

impl Matcher {
    /// Returns `None` for queries that are empty after trimming.
    pub fn new(query: &str, case_sensitive: bool) -> Option<Self> {
        let needle = tokenize(query.trim(), case_sensitive)
            .into_iter()
            .map(|(t, _)| t)
            .collect::<Vec<_>>();
        (!needle.is_empty()).then_some(Self {
            needle,
            case_sensitive,
        })
    }

    /// All non-overlapping matches in `text`.
    pub fn find_all(&self, text: &str) -> Vec<Match> {
        let hay = tokenize(text, self.case_sensitive);
        let n = self.needle.len();
        let mut out = Vec::new();
        let mut i = 0;
        while i + n <= hay.len() {
            if hay[i..i + n].iter().map(|(t, _)| t).eq(self.needle.iter()) {
                let (start, _) = hay[i].1;
                let (_, end) = hay[i + n - 1].1;
                out.push(Match { start, end });
                i += n;
            } else {
                i += 1;
            }
        }
        out
    }
}

/// Tokens with the `char` range they cover in the original text.
fn tokenize(text: &str, case_sensitive: bool) -> Vec<(Token, (usize, usize))> {
    let mut out: Vec<(Token, (usize, usize))> = Vec::with_capacity(text.len());
    for (i, c) in text.chars().enumerate() {
        if c.is_whitespace() {
            match out.last_mut() {
                Some((Token::Space, range)) => range.1 = i + 1,
                _ => out.push((Token::Space, (i, i + 1))),
            }
        } else {
            out.push((Token::Char(fold(c, case_sensitive)), (i, i + 1)));
        }
    }
    out
}

fn fold(c: char, case_sensitive: bool) -> char {
    if case_sensitive {
        return c;
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
}
