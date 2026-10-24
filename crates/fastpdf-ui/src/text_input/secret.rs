//! The secret mode of [`TextInput`]: a password field. It shows one bullet
//! per character (`crate::text_mask`), so GPUI never shapes or caches the
//! text; copy and cut do nothing; change events carry no text; its buffer is
//! reserved up front and capped, so it never moves; the password leaves it
//! by move (`take_secret`) and whatever is left is wiped on drop.

use std::fmt;
use std::ops::Range;

use fastpdf_engine_api::{Password, wipe_string};
use gpui::{ClipboardEntry, ClipboardItem, Context, SharedString};

use super::{TextChanged, TextInput};
use crate::text_mask;

impl TextInput {
    /// A password field. Its buffer is reserved up front and the text is
    /// capped at [`Password::MAX_BYTES`], so it never moves (and leaves no
    /// copy) while the user types.
    pub(crate) fn secret(
        placeholder: impl Into<SharedString>,
        key_context: &'static str,
        cx: &mut Context<'_, Self>,
    ) -> Self {
        let mut input = Self::with_context(placeholder, key_context, cx);
        input.secret = true;
        input.text = String::with_capacity(Password::MAX_BYTES);
        input
    }

    /// Moves the password out of a secret field without copying it; the
    /// field is left empty.
    pub(crate) fn take_secret(&mut self, cx: &mut Context<'_, Self>) -> Password {
        let fresh = String::with_capacity(Password::MAX_BYTES);
        let text = std::mem::replace(&mut self.text, fresh);
        self.selected = 0..0;
        self.reversed = false;
        self.marked = None;
        cx.emit(self.changed());
        cx.notify();
        Password::from(text)
    }

    /// The change event; a secret field never reports its text.
    pub(super) fn changed(&self) -> TextChanged {
        TextChanged(reported_text(&self.text, self.secret))
    }

    /// The most bytes the field holds: [`Password::MAX_BYTES`] for a
    /// secret, whose reserved buffer must never grow.
    pub(super) fn byte_cap(&self) -> Option<usize> {
        self.secret.then_some(Password::MAX_BYTES)
    }

    /// Offset in what is shown of text offset `offset`.
    pub(super) fn display_offset(&self, offset: usize) -> usize {
        if self.secret {
            text_mask::to_display(&self.text, offset)
        } else {
            offset
        }
    }

    /// Text offset of offset `shown` in what is shown.
    pub(super) fn text_offset(&self, shown: usize) -> usize {
        if self.secret {
            text_mask::from_display(&self.text, shown)
        } else {
            shown
        }
    }

    /// Drops a temporary copy of the field's text, wiped in a secret field.
    pub(super) fn discard(&self, mut copy: String) {
        if self.secret {
            wipe_string(&mut copy);
        }
    }

    /// Drops what was read from the clipboard, its text wiped in a secret
    /// field (the system clipboard itself keeps what the user copied).
    pub(super) fn discard_clipboard(&self, item: ClipboardItem) {
        if !self.secret {
            return;
        }
        for entry in item.into_entries() {
            if let ClipboardEntry::String(mut copy) = entry {
                wipe_string(&mut copy.text);
            }
        }
    }
}

impl Drop for TextInput {
    fn drop(&mut self) {
        if self.secret {
            wipe_string(&mut self.text);
        }
    }
}

/// Replaces `range` of `text` with `new_text` and returns where the
/// inserted text is. With `cap`, only what keeps `text` within `cap` bytes
/// is inserted (never part of a character), so a buffer reserved with that
/// capacity is never reallocated, which would leave a copy behind.
pub(super) fn splice_text(
    text: &mut String,
    range: Range<usize>,
    new_text: &str,
    cap: Option<usize>,
) -> Range<usize> {
    let start = range.start.min(text.len());
    let end = range.end.clamp(start, text.len());
    let new_text = match cap {
        Some(cap) => text_mask::fit(new_text, cap.saturating_sub(text.len() - (end - start))),
        None => new_text,
    };
    text.replace_range(start..end, new_text);
    start..start + new_text.len()
}

/// The text a change event carries: none for a secret.
pub(super) fn reported_text(text: &str, secret: bool) -> String {
    if secret {
        String::new()
    } else {
        text.to_owned()
    }
}

/// What the platform's text input (the IME) may read: bullets for a secret.
pub(super) fn platform_text(text: &str, secret: bool) -> String {
    if secret {
        text_mask::mask(text).to_string()
    } else {
        text.to_owned()
    }
}

/// Pasted text on one line: line breaks become spaces, or vanish from a
/// password (they are never part of it).
pub(super) fn pasted_line(text: &str, secret: bool) -> String {
    text.replace(['\r', '\n'], if secret { "" } else { " " })
}

/// The field's text in `Debug` output: redacted for a secret.
pub(super) struct Shown<'a> {
    text: &'a str,
    secret: bool,
}

impl<'a> Shown<'a> {
    pub(super) fn new(text: &'a str, secret: bool) -> Self {
        Self { text, secret }
    }
}

impl fmt::Debug for Shown<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.secret {
            f.write_str("<redacted>")
        } else {
            fmt::Debug::fmt(self.text, f)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reserved_password_buffer_never_moves_and_stops_at_the_cap() {
        let mut text = String::with_capacity(Password::MAX_BYTES);
        let buffer = text.as_ptr();
        let capacity = text.capacity();
        let cap = Some(Password::MAX_BYTES);
        // Typed one character at a time at the end, as a user would: three
        // bytes each, so the last one that would cross the cap is dropped.
        for _ in 0..400 {
            let end = text.len();
            splice_text(&mut text, end..end, "測", cap);
        }
        assert_eq!(text.len(), Password::MAX_BYTES / 3 * 3);
        assert!(text.chars().all(|c| c == '測'));
        // A paste in the middle keeps only what fits, never half a char.
        let inserted = splice_text(&mut text, 3..3, "😀😀", cap);
        assert_eq!(inserted, 3..3);
        assert_eq!(text.as_ptr(), buffer);
        assert_eq!(text.capacity(), capacity);
        // Replacing a range frees its bytes for the new text.
        let inserted = splice_text(&mut text, 0..6, "a😀", cap);
        assert_eq!(inserted, 0..5);
        assert!(text.starts_with("a😀測"));
        assert!(text.len() <= Password::MAX_BYTES);
        assert_eq!(text.as_ptr(), buffer);
    }

    #[test]
    fn ordinary_fields_take_everything_and_ranges_are_clamped() {
        let mut text = String::from("abc");
        assert_eq!(splice_text(&mut text, 1..2, "XYZ", None), 1..4);
        assert_eq!(text, "aXYZc");
        assert_eq!(splice_text(&mut text, 9..20, "!", None), 5..6);
        assert_eq!(text, "aXYZc!");
        // An IME may report a range whose end is before its start.
        let (start, end) = (4, 2);
        assert_eq!(splice_text(&mut text, start..end, "-", None), 4..5);
        assert_eq!(text, "aXYZ-c!");
        let long = "x".repeat(Password::MAX_BYTES * 2);
        assert_eq!(splice_text(&mut text, 0..0, &long, None).len(), long.len());
    }

    #[test]
    fn a_secret_is_never_reported_shown_or_kept_on_two_lines() {
        assert_eq!(reported_text("zq9", true), "");
        assert_eq!(reported_text("zq9", false), "zq9");
        assert_eq!(platform_text("測試a😀", true), "\u{2022}".repeat(4));
        assert_eq!(platform_text("ab", false), "ab");
        assert_eq!(pasted_line("pw\r\n", true), "pw");
        assert_eq!(pasted_line("a\r\nb", false), "a  b");
        let shown = format!(
            "{:?} {:#?}",
            Shown::new("zq9", true),
            Shown::new("zq9", true)
        );
        assert!(!shown.contains("zq9"), "{shown}");
        assert!(shown.contains("<redacted>"));
        assert_eq!(format!("{:?}", Shown::new("find me", false)), "\"find me\"");
    }
}
