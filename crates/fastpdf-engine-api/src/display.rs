//! Making document text safe to show (spec §24).
//!
//! Text that comes out of a PDF (page-label prefixes, search snippets,
//! document properties) is hostile: it can carry control characters that
//! break a one-line layout, bidi overrides and zero-width characters that
//! make one text look like another, and absurd lengths. These helpers are
//! the one place where such text is turned into something a toolbar or a
//! list row can show. They are pure and std only.

/// Bidi controls, zero-width characters and the byte order mark: invisible
/// characters that change how the text around them reads.
pub fn is_format_control(c: char) -> bool {
    matches!(
        c,
        '\u{061C}'
            | '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{2069}'
            | '\u{FEFF}'
    )
}

/// Marks text that was cut by [`clean_display`].
const ELLIPSIS: char = '\u{2026}';

/// How [`clean_display`] shows one character: format controls
/// ([`is_format_control`]) not at all, C0/C1 controls and line or
/// paragraph separators as a space, everything else as it is. For text
/// that is cut or marked up elsewhere (search snippets).
pub fn display_char(c: char) -> Option<char> {
    if is_format_control(c) {
        None
    } else if c.is_control() || matches!(c, '\u{2028}' | '\u{2029}') {
        Some(' ')
    } else {
        Some(c)
    }
}

/// `text` made safe for a one-line display: C0/C1 controls and line or
/// paragraph separators become spaces, format controls
/// ([`is_format_control`]) are removed, and the result is at most
/// `max_bytes` long. Cut text ends with an ellipsis (counted in
/// `max_bytes`) and is always cut at a character boundary.
///
/// Cleaning is idempotent: cleaning the result again with the same limit
/// returns it unchanged.
pub fn clean_display(text: &str, max_bytes: usize) -> String {
    let mut out = String::with_capacity(text.len().min(max_bytes));
    let mut cut = false;
    for c in text.chars().filter_map(display_char) {
        if out.len() + c.len_utf8() > max_bytes {
            cut = true;
            break;
        }
        out.push(c);
    }
    if cut {
        let room = max_bytes.saturating_sub(ELLIPSIS.len_utf8());
        while out.len() > room {
            out.pop();
        }
        if out.len() + ELLIPSIS.len_utf8() <= max_bytes {
            out.push(ELLIPSIS);
        }
    }
    out
}

/// Full-width ASCII forms (U+FF01..=U+FF5E, typed by CJK input methods) and
/// the ideographic space folded to their ASCII counterparts; every other
/// character is returned unchanged. One character in, one character out, so
/// folding never moves text offsets measured in characters.
pub fn fold_full_width(c: char) -> char {
    match c {
        '\u{FF01}'..='\u{FF5E}' => char::from_u32(u32::from(c) - 0xFEE0).unwrap_or(c),
        '\u{3000}' => ' ',
        _ => c,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn controls_become_spaces_and_format_controls_vanish() {
        assert_eq!(clean_display("a\nb\tc\u{0}d\u{85}e", 64), "a b c d e");
        assert_eq!(clean_display("x\u{2028}y\u{2029}z", 64), "x y z");
        // Right-to-left override, zero-width space, isolates, BOM, ALM.
        assert_eq!(
            clean_display("\u{202E}cod.exe\u{200B}\u{2066}\u{FEFF}\u{061C}!", 64),
            "cod.exe!"
        );
        assert_eq!(clean_display("附錄-", 64), "附錄-");
        assert_eq!(display_char('\u{202E}'), None);
        assert_eq!(display_char('\u{0}'), Some(' '));
        assert_eq!(display_char('\u{2028}'), Some(' '));
        assert_eq!(display_char('公'), Some('公'));
        assert!(is_format_control('\u{200F}'));
        assert!(!is_format_control('a'));
    }

    #[test]
    fn long_text_is_cut_at_a_character_boundary() {
        assert_eq!(clean_display("abcdef", 6), "abcdef");
        assert_eq!(clean_display("abcdefg", 6), "abc\u{2026}");
        // Three bytes per character: never split one.
        let cut = clean_display(&"附".repeat(50), 128);
        assert!(cut.len() <= 128, "{}", cut.len());
        assert!(cut.ends_with('\u{2026}'));
        assert_eq!(cut.chars().filter(|c| *c == '附').count(), 41);
        // Limits below the ellipsis itself still never exceed the limit.
        assert_eq!(clean_display("abcdef", 2), "");
        assert_eq!(clean_display("abcdef", 3), "\u{2026}");
        assert_eq!(clean_display("", 0), "");
    }

    #[test]
    fn cleaning_is_idempotent() {
        for text in [
            "plain",
            "\u{202E}evil\u{0}text that is long enough to be cut somewhere",
            &"附錄".repeat(100),
        ] {
            for max in [0, 3, 7, 20, 128] {
                let once = clean_display(text, max);
                assert!(once.len() <= max);
                assert_eq!(clean_display(&once, max), once, "{text:?} at {max}");
            }
        }
    }

    #[test]
    fn full_width_forms_fold_to_ascii() {
        let folded: String = "１２＃３ＡＢｃ！～\u{3000}x"
            .chars()
            .map(fold_full_width)
            .collect();
        assert_eq!(folded, "12#3ABc!~ x");
        assert_eq!(fold_full_width('頁'), '頁');
        assert_eq!(fold_full_width('\u{FF00}'), '\u{FF00}');
        assert_eq!(fold_full_width('\u{FF5F}'), '\u{FF5F}');
    }
}
