//! The prompt card: a question that blocks the window until it is answered.
//! A scrim over the document area dims it and takes the mouse; the centered
//! card holds the keyboard context [`keymap::PROMPT`] (Enter confirms, Esc
//! cancels), and while a prompt shows, `ReaderView::run_command` lets only
//! the commands through that answer or leave it
//! (`ReaderCommand::allowed_during_prompt`). Each feature keeps its own
//! state (the locked document for a password, `crate::password`); this
//! module only knows which prompt is showing. Nothing is created before a
//! prompt is needed, and nothing animates.

use fastpdf_core::keymap;
use gpui::{
    AnyElement, Context, FocusHandle, InteractiveElement, IntoElement, ParentElement, SharedString,
    Styled, Window, div, prelude::FluentBuilder, px,
};

use crate::reader::{DocState, ReaderView};
use crate::theme::Theme;

const CARD_WIDTH: f32 = 420.0;
/// The status line keeps its height while empty, so the card does not move
/// when a problem shows up.
const STATUS_HEIGHT: f32 = 18.0;

/// Which prompt is showing. A tag only: it never carries the prompt's data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PromptKind {
    /// The password of an encrypted document.
    Password,
}

impl PromptKind {
    /// The name development scripts use (`wait-prompt=password`).
    pub(crate) fn from_name(name: &str) -> Option<Self> {
        match name {
            "password" => Some(Self::Password),
            _ => None,
        }
    }
}

/// A line under the body: a problem (`error`) or progress.
pub(crate) struct PromptStatus {
    pub text: SharedString,
    pub error: bool,
}

/// The card with `title`, `body`, a status line and `buttons`, centered on
/// a scrim that covers the document area. With `focus`, the card itself
/// takes the keyboard (a prompt without a text field); otherwise a field in
/// `body` holds it.
pub(crate) fn prompt_card(
    id: &'static str,
    theme: Theme,
    focus: Option<&FocusHandle>,
    title: SharedString,
    body: impl IntoElement,
    status: Option<PromptStatus>,
    buttons: impl IntoElement,
) -> impl IntoElement {
    let status_line = div()
        .h(px(STATUS_HEIGHT))
        .when_some(status, |line, status| {
            line.text_color(if status.error {
                theme.error_text
            } else {
                theme.text_muted
            })
            .child(status.text)
        });
    let card = div()
        .id(id)
        .key_context(keymap::PROMPT)
        .when_some(focus, |card, focus| card.track_focus(focus))
        .w(px(CARD_WIDTH))
        .max_w_full()
        .flex()
        .flex_col()
        .gap_2()
        .p_4()
        .rounded(px(6.0))
        .bg(theme.toolbar_bg)
        .border_1()
        .border_color(theme.toolbar_border)
        .text_size(px(13.0))
        .child(div().text_size(px(20.0)).child(title))
        .child(body)
        .child(status_line)
        .child(div().flex().flex_row().justify_end().gap_2().child(buttons));
    // The scrim occludes the document and the panels below it; clicks on
    // it leave the keyboard focus where it is.
    div()
        .id("prompt-scrim")
        .absolute()
        .size_full()
        .occlude()
        .flex()
        .items_center()
        .justify_center()
        .p_4()
        .bg(theme.modal_scrim)
        .child(card)
}

impl ReaderView {
    /// The prompt in front of the window, if any.
    pub(crate) fn active_prompt(&self) -> Option<PromptKind> {
        matches!(self.doc, DocState::Locked(_)).then_some(PromptKind::Password)
    }

    /// Enter in a prompt.
    pub(crate) fn confirm_prompt(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if let Some(PromptKind::Password) = self.active_prompt() {
            self.unlock_document(window, cx);
        }
    }

    /// Esc or Cancel in a prompt: it closes. False when none was showing.
    pub(crate) fn cancel_prompt(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> bool {
        match self.active_prompt() {
            // Back to the empty window; opening the file again asks again.
            Some(PromptKind::Password) => {
                self.close_document(window, cx);
                true
            }
            None => false,
        }
    }

    /// The prompt card, the topmost child of the document area.
    pub(crate) fn render_prompt(&self, cx: &mut Context<'_, Self>) -> Option<AnyElement> {
        match self.active_prompt()? {
            PromptKind::Password => self
                .render_password_prompt(cx)
                .map(IntoElement::into_any_element),
        }
    }
}
