//! The secret mode of [`TextInput`]: a password field. It shows one bullet
//! per character (`crate::text_mask`), so GPUI never shapes or caches the
//! text; copy and cut do nothing; change events carry no text; its buffer is
//! reserved up front and capped, so it never moves; the password leaves it
//! by move (`take_secret`) and whatever is left is wiped on drop.

use fastpdf_engine_api::{Password, wipe_string};
use gpui::{Context, SharedString};

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
        TextChanged(if self.secret {
            String::new()
        } else {
            self.text.clone()
        })
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

    /// What fits of `text` after the `kept` bytes a secret field keeps; all
    /// of it for an ordinary field.
    pub(super) fn fitting<'a>(&self, text: &'a str, kept: usize) -> &'a str {
        if self.secret {
            text_mask::fit(text, Password::MAX_BYTES.saturating_sub(kept))
        } else {
            text
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
