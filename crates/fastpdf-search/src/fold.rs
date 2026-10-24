//! How characters compare in a search: full-width ASCII forms as their
//! ASCII counterparts, letters without case unless asked, and which
//! characters make up words (whole-word matching).
//!
//! Every fold maps one `char` to one `char`, so the tokens a page is
//! matched as stay 1:1 with its characters and highlight rectangles cover
//! exactly the glyphs found (a full-width 「ＰＤＦ」 found by `PDF` is
//! highlighted as drawn).

use fastpdf_engine_api::display::fold_full_width;

/// The token `c` is compared as: full-width ASCII forms (U+FF01..=U+FF5E,
/// typed by CJK input methods) and the ideographic space as their ASCII
/// counterparts, then, unless the search is case-sensitive, lowercase
/// (single-char mappings only), with the Greek final sigma as σ (Σ
/// lowercases to σ, so 「ΟΔΟΣ」 and 「οδος」 match).
pub(crate) fn fold(c: char, case_sensitive: bool) -> char {
    if c.is_ascii() {
        return if case_sensitive {
            c
        } else {
            c.to_ascii_lowercase()
        };
    }
    let c = fold_full_width(c);
    if case_sensitive {
        return c;
    }
    if c.is_ascii() {
        return c.to_ascii_lowercase();
    }
    let mut lower = c.to_lowercase();
    match (lower.next(), lower.next()) {
        (Some('ς'), None) => 'σ',
        (Some(l), None) => l,
        _ => c, // multi-char mappings (e.g. 'İ') stay as-is to keep indices 1:1
    }
}

/// Whether `c` is part of a word for whole-word matching: ASCII letters,
/// digits and `_` (full-width ones too); the letters of Latin-1, Latin
/// Extended (and Extended Additional: Vietnamese), Greek (and Greek
/// Extended: polytonic), Cyrillic (and its supplement) and Hangul
/// syllables; and combining marks, which belong to the letter before them
/// (a café written as e plus a combining acute is one word). CJK
/// ideographs and kana, punctuation and spaces are not, so a word next to
/// Chinese text (「使用PDF格式」) is a whole word, and a query of only such
/// characters is never constrained.
pub fn is_word_char(c: char) -> bool {
    let c = fold_full_width(c);
    c == '_'
        || c.is_ascii_alphanumeric()
        || is_combining_mark(c)
        || (matches!(
            c,
            '\u{00C0}'..='\u{024F}'
                | '\u{0370}'..='\u{052F}'
                | '\u{1E00}'..='\u{1FFF}'
                | '\u{AC00}'..='\u{D7A3}'
        ) && c.is_alphabetic())
}

/// Combining diacritical marks (and their extended, supplement, for-symbols
/// and half-mark blocks).
fn is_combining_mark(c: char) -> bool {
    matches!(
        c,
        '\u{0300}'..='\u{036F}'
            | '\u{1AB0}'..='\u{1AFF}'
            | '\u{1DC0}'..='\u{1DFF}'
            | '\u{20D0}'..='\u{20FF}'
            | '\u{FE20}'..='\u{FE2F}'
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_width_ascii_folds_one_to_one() {
        assert_eq!(fold('Ｐ', true), 'P');
        assert_eq!(fold('０', true), '0');
        assert_eq!(fold('～', true), '~');
        assert_eq!(fold('！', false), '!');
        // Neighbours of the block, and half-width katakana, stay as they are.
        assert_eq!(fold('\u{FF00}', false), '\u{FF00}');
        assert_eq!(fold('\u{FF5F}', false), '\u{FF5F}');
        assert_eq!(fold('ｶ', false), 'ｶ');
        assert_eq!(fold('公', false), '公');
    }

    #[test]
    fn width_then_case() {
        assert_eq!(fold('Ａ', false), 'a');
        assert_eq!(fold('Ａ', true), 'A');
        assert_eq!(fold('ａ', true), 'a');
        assert_eq!(fold('Ж', false), 'ж');
        assert_eq!(fold('Ж', true), 'Ж');
        // Multi-char lowercase mappings keep the character.
        assert_eq!(fold('İ', false), 'İ');
        // The final sigma is a sigma, one character for one.
        assert_eq!(fold('ς', false), 'σ');
        assert_eq!(fold('Σ', false), 'σ');
        assert_eq!(fold('σ', false), 'σ');
        assert_eq!(fold('ς', true), 'ς');
    }

    #[test]
    fn ideographic_space_folds_to_space() {
        assert_eq!(fold('\u{3000}', false), ' ');
        assert_eq!(fold('\u{3000}', true), ' ');
    }

    #[test]
    fn word_chars() {
        for c in [
            'a', 'Z', '0', '_', 'é', 'ž', 'ж', 'Ω', 'Ａ', '９', 'ệ', 'Ḁ', 'ἄ', 'ᾧ', 'Ԁ', 'ԯ', '한',
            '가', '힣', '\u{0301}', '\u{0308}', '\u{0323}', '\u{1AB0}', '\u{1DC0}', '\u{20D7}',
            '\u{FE20}',
        ] {
            assert!(is_word_char(c), "{c:?}");
        }
        for c in [
            '公', '、', '-', ' ', '，', '×', '÷', '(', '\u{3000}', 'ｶ', 'か', '\u{1FBD}',
            '\u{1FEF}', '\u{D7A4}', 'ㄱ',
        ] {
            assert!(!is_word_char(c), "{c:?}");
        }
    }
}
