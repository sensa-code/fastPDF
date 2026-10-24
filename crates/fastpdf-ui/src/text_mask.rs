//! What a password field shows instead of its text: one bullet per
//! character, so the text itself never reaches GPUI's text shaping (or its
//! line cache), plus the mapping between offsets in the text and in what is
//! shown. Offsets are UTF-8 byte offsets on both sides.

use gpui::SharedString;

/// Shown for every character of a secret.
pub(crate) const MASK: char = '\u{2022}';

/// One [`MASK`] per character of `text`.
pub(crate) fn mask(text: &str) -> SharedString {
    let mut shown = String::with_capacity(text.chars().count() * MASK.len_utf8());
    shown.extend(text.chars().map(|_| MASK));
    shown.into()
}

/// Offset in the masked text of offset `byte` in `text` (a char boundary;
/// past the end means the end).
pub(crate) fn to_display(text: &str, byte: usize) -> usize {
    text.char_indices().take_while(|&(i, _)| i < byte).count() * MASK.len_utf8()
}

/// Offset in `text` of offset `display` in the masked text, rounded down to
/// a whole bullet.
pub(crate) fn from_display(text: &str, display: usize) -> usize {
    text.char_indices()
        .nth(display / MASK.len_utf8())
        .map_or(text.len(), |(i, _)| i)
}

/// The longest start of `text` that fits in `room` bytes without splitting
/// a character.
pub(crate) fn fit(text: &str, room: usize) -> &str {
    if text.len() <= room {
        return text;
    }
    let mut end = room;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offsets_round_trip_through_cjk_and_astral_text() {
        for text in ["測試密碼2026", "a😀b", ""] {
            let shown = mask(text);
            assert_eq!(shown.chars().count(), text.chars().count(), "{text}");
            assert!(shown.chars().all(|c| c == MASK));
            for (byte, _) in text.char_indices().chain([(text.len(), ' ')]) {
                let display = to_display(text, byte);
                assert!(shown.is_char_boundary(display), "{text} {byte}");
                assert_eq!(from_display(text, display), byte, "{text} {byte}");
            }
            assert_eq!(to_display(text, usize::MAX), shown.len());
            assert_eq!(from_display(text, usize::MAX), text.len());
        }
        // "測" is three bytes, "😀" four; each is one bullet.
        assert_eq!(to_display("測試", 3), 3);
        assert_eq!(to_display("a😀b", 5), 6);
        assert_eq!(from_display("a😀b", 6), 5);
        // Inside a bullet: the start of its character.
        assert_eq!(from_display("a😀b", 4), 1);
    }

    #[test]
    fn fit_never_splits_a_character() {
        assert_eq!(fit("abc", 5), "abc");
        assert_eq!(fit("abc", 2), "ab");
        assert_eq!(fit("測試", 4), "測");
        assert_eq!(fit("測試", 2), "");
        assert_eq!(fit("a😀", 4), "a");
        assert_eq!(fit("", 0), "");
    }
}
