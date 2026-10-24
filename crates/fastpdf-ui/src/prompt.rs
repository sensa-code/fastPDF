//! The prompt card: a question that blocks the window until it is answered.
//! A scrim over the document area dims it and takes the mouse; the centered
//! card holds the keyboard context [`keymap::PROMPT`] (Enter confirms, Esc
//! cancels), and while a prompt shows, `ReaderView::run_command` lets only
//! the commands through that answer or leave it
//! (`ReaderCommand::allowed_during_prompt`). Each feature keeps its own
//! state (the locked document for a password, `crate::password`; the
//! address of a link to open, `crate::links`); this module only knows
//! which prompt is showing. Nothing is created before a prompt is needed,
//! and nothing animates.

use fastpdf_core::keymap;
use gpui::{
    AnyElement, Context, FocusHandle, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, Window, div, prelude::FluentBuilder, px,
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
    /// Whether to open an external link (Enter does not answer it).
    OpenLink,
}

impl PromptKind {
    /// The name development scripts use (`wait-prompt=password`, `=link`).
    pub(crate) fn from_name(name: &str) -> Option<Self> {
        match name {
            "password" => Some(Self::Password),
            "link" => Some(Self::OpenLink),
            _ => None,
        }
    }
}

/// Where the card sits in the document area.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CardPlacement {
    Center,
    /// Near the top or the bottom: away from where the pointer was, so a
    /// second click of the click that opened it cannot land on a button.
    Top,
    Bottom,
}

/// A line under the body: a problem (`error`) or progress.
pub(crate) struct PromptStatus {
    pub text: SharedString,
    pub error: bool,
}

/// The card with `title`, `body`, a status line and `buttons`, placed on a
/// scrim that covers the document area. With `focus`, the card itself
/// takes the keyboard (a prompt without a text field); otherwise a field in
/// `body` holds it.
#[allow(clippy::too_many_arguments)]
pub(crate) fn prompt_card(
    id: &'static str,
    theme: Theme,
    placement: CardPlacement,
    focus: Option<&FocusHandle>,
    title: SharedString,
    body: impl IntoElement,
    status: Option<PromptStatus>,
    buttons: impl IntoElement,
) -> impl IntoElement {
    let status_line = div()
        .flex_none()
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
        // A small window: the card fits the document area, the body
        // scrolls and the title and buttons stay.
        .max_h_full()
        .flex()
        .flex_col()
        .gap_2()
        .p_4()
        .rounded(px(6.0))
        .bg(theme.toolbar_bg)
        .border_1()
        .border_color(theme.toolbar_border)
        .text_size(px(13.0))
        .child(div().flex_none().text_size(px(20.0)).child(title))
        .child(
            div()
                .id("prompt-body")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .child(body),
        )
        .child(status_line)
        .child(
            div()
                .flex_none()
                .flex()
                .flex_row()
                .justify_end()
                .gap_2()
                .child(buttons),
        );
    // The scrim occludes the document and the panels below it; clicks on
    // it leave the keyboard focus where it is.
    div()
        .id("prompt-scrim")
        .absolute()
        .size_full()
        .occlude()
        .flex()
        .map(|scrim| match placement {
            CardPlacement::Center => scrim.items_center(),
            CardPlacement::Top => scrim.items_start(),
            CardPlacement::Bottom => scrim.items_end(),
        })
        .justify_center()
        .p_4()
        .bg(theme.modal_scrim)
        .child(card)
}

impl ReaderView {
    /// The prompt in front of the window, if any.
    pub(crate) fn active_prompt(&self) -> Option<PromptKind> {
        if matches!(self.doc, DocState::Locked(_)) {
            Some(PromptKind::Password)
        } else if self.links.prompt.is_some() {
            Some(PromptKind::OpenLink)
        } else {
            None
        }
    }

    /// Enter in a prompt. A link opens only on a click on its Open button:
    /// Enter, typed on and on, must not open what a document suggests.
    pub(crate) fn confirm_prompt(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        match self.active_prompt() {
            Some(PromptKind::Password) => self.unlock_document(window, cx),
            Some(PromptKind::OpenLink) | None => {}
        }
    }

    /// Gives the keyboard back to the prompt that shows (its field, or the
    /// card itself).
    pub(crate) fn refocus_prompt(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        match self.active_prompt() {
            Some(PromptKind::Password) => self.refocus_password_field(window, cx),
            Some(PromptKind::OpenLink) => self.refocus_link_prompt(window, cx),
            None => {}
        }
    }

    /// The prompt that shows has the keyboard (development scripts).
    pub(crate) fn prompt_focused(&self, window: &Window, cx: &gpui::App) -> bool {
        match self.active_prompt() {
            Some(PromptKind::Password) => self.password_field_focused(window, cx),
            Some(PromptKind::OpenLink) => self.link_prompt_focused(window),
            None => false,
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
            Some(PromptKind::OpenLink) => {
                self.answer_link_prompt(false, window, cx);
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
            PromptKind::OpenLink => self
                .render_link_prompt(cx)
                .map(IntoElement::into_any_element),
        }
    }
}
