//! Encrypted documents (ADR 0014). A document that needs a password stays
//! loaded (`document::LockedSource`) while the prompt card asks for it.
//! Each attempt moves the typed password out of the secret field, without a
//! copy, into one open of the same bytes on a background thread; a wrong
//! password brings the bytes back for the next attempt. The password is not
//! kept anywhere in the UI: only the engine keeps the copy the open document
//! needs (for render host restarts). A document opened with a password is
//! never written (no save a copy, page tools, stamps, text edits or
//! signatures): the job file and the writer's command line must not see it.

use std::path::PathBuf;
use std::sync::Arc;

use fastpdf_core::keymap::ReaderCommand;
use gpui::{
    App, AppContext, ClickEvent, Context, Entity, EntityInputHandler, IntoElement, ParentElement,
    SharedString, StatefulInteractiveElement, Styled, Subscription, Task, Window, div,
    prelude::FluentBuilder, px,
};

use crate::document::{LockedSource, OpenFailure, OpenedDocument, open_loaded};
use crate::opening::{APP_TITLE, display_name};
use crate::prompt::{PromptStatus, prompt_card};
use crate::reader::{DocState, ReaderView};
use crate::text_input::{TextChanged, TextInput};
use crate::toolbar::styled_button;
use crate::tools::WriteNotice;

/// Key context of the password field: the editing keys, and Enter and Esc
/// for the prompt.
const FIELD_CONTEXT: &str = "TextInput Prompt";

/// An encrypted document waiting for its password.
pub(crate) struct LockedDoc {
    pub path: PathBuf,
    name: SharedString,
    /// The loaded file; `None` while an attempt has it.
    source: Option<Box<LockedSource>>,
    field: Entity<TextInput>,
    /// An attempt is running.
    pub checking: bool,
    /// The last attempt's password did not open the document.
    pub wrong: bool,
    _attempt: Option<Task<()>>,
    _changes: Subscription,
    _refocus: Subscription,
}

impl ReaderView {
    /// `locked` needs a password: show the prompt for it.
    pub(crate) fn lock_document(
        &mut self,
        locked: Box<LockedSource>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        log::info!(
            "{} is encrypted; asking for its password",
            locked.path.display()
        );
        let placeholder = self.strings().password_placeholder();
        let field = cx.new(|cx| TextInput::secret(placeholder, FIELD_CONTEXT, cx));
        // Typing again clears the "wrong password" line.
        let changes = cx.subscribe(&field, |view: &mut Self, _, _: &TextChanged, cx| {
            if let DocState::Locked(locked) = &mut view.doc {
                locked.wrong = false;
            }
            cx.notify();
        });
        // GPUI gives the keyboard to the reader itself on any mouse press
        // outside the prompt (the toolbar, the sidebar) or when the file
        // dialog of Open closes: hand it back to the field, or typing and
        // Enter would go nowhere.
        let refocus = cx.on_focus(&self.focus, window, |view: &mut Self, window, cx| {
            view.refocus_password_field(window, cx);
        });
        let name: SharedString = display_name(&locked.path).into();
        window.set_window_title(&format!("{name} - {APP_TITLE}"));
        let focus = field.read(cx).focus_handle().clone();
        window.focus(&focus, cx);
        // A toolbar menu would float over the prompt.
        self.menus.open = None;
        self.doc = DocState::Locked(Box::new(LockedDoc {
            path: locked.path.clone(),
            name,
            wrong: locked.wrong_password,
            source: Some(locked),
            field,
            checking: false,
            _attempt: None,
            _changes: changes,
            _refocus: refocus,
        }));
        cx.notify();
    }

    /// The prompt's Open (or Enter): tries the typed password.
    pub(crate) fn unlock_document(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let engine = Arc::clone(&self.options.engine);
        let DocState::Locked(locked) = &mut self.doc else {
            return;
        };
        if locked.checking || !ready_to_try(locked.field.read(cx)) {
            return;
        }
        let Some(source) = locked.source.take() else {
            return;
        };
        let password = locked.field.update(cx, |field, cx| field.take_secret(cx));
        locked.checking = true;
        locked.wrong = false;
        let path = locked.path.clone();
        let attempt = cx
            .background_executor()
            .spawn(async move { open_loaded(engine.as_ref(), *source, Some(password)) });
        locked._attempt = Some(cx.spawn_in(window, async move |this, cx| {
            let result = attempt.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.finish_unlock(path, result, window, cx);
            });
        }));
        cx.notify();
    }

    fn finish_unlock(
        &mut self,
        path: PathBuf,
        result: Result<OpenedDocument, OpenFailure>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let waiting = matches!(&self.doc, DocState::Locked(l) if l.path == path && l.checking);
        if !waiting {
            // Closed or replaced meanwhile. Dropping an opened document may
            // wait for its render threads: not on the UI thread.
            cx.background_executor()
                .spawn(async move { drop(result) })
                .detach();
            return;
        }
        match result {
            Ok(opened) => {
                window.focus(&self.focus, cx);
                self.close_write_panels(cx);
                self.doc = DocState::Loading { path: path.clone() };
                self.finish_open(path, Ok(opened.into()), window, cx);
            }
            Err(OpenFailure::Locked(source)) => {
                log::info!("the password did not open {}", path.display());
                if let DocState::Locked(locked) = &mut self.doc {
                    locked.source = Some(source);
                    locked.checking = false;
                    locked.wrong = true;
                    let focus = locked.field.read(cx).focus_handle().clone();
                    window.focus(&focus, cx);
                }
            }
            Err(failure) => {
                log::warn!("cannot open {}: {failure}", path.display());
                window.focus(&self.focus, cx);
                window.set_window_title(APP_TITLE);
                self.doc = DocState::Failed { path, failure };
            }
        }
        cx.notify();
    }

    pub(crate) fn refocus_password_field(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        if let DocState::Locked(locked) = &self.doc {
            let focus = locked.field.read(cx).focus_handle().clone();
            window.focus(&focus, cx);
        }
    }

    /// The prompt's field has the keyboard focus (development scripts).
    pub(crate) fn password_field_focused(&self, window: &Window, cx: &App) -> bool {
        match &self.doc {
            DocState::Locked(locked) => locked.field.read(cx).focus_handle().is_focused(window),
            _ => false,
        }
    }

    pub(crate) fn render_password_prompt(
        &self,
        cx: &mut Context<'_, Self>,
    ) -> Option<impl IntoElement + use<>> {
        let DocState::Locked(locked) = &self.doc else {
            return None;
        };
        let theme = self.theme;
        let strings = self.strings();
        let can_open = !locked.checking && ready_to_try(locked.field.read(cx));
        let status = if locked.checking {
            Some(PromptStatus {
                text: strings.password_checking().into(),
                error: false,
            })
        } else if locked.wrong {
            Some(PromptStatus {
                text: strings.password_wrong().into(),
                error: true,
            })
        } else {
            None
        };
        let body = div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .text_color(theme.text_muted)
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .child(locked.name.clone()),
            )
            .child(strings.password_hint())
            .child(
                div()
                    .w(px(320.0))
                    .max_w_full()
                    .h(px(32.0))
                    .px_2()
                    .rounded(px(4.0))
                    .bg(theme.input_bg)
                    .border_1()
                    .border_color(if locked.wrong {
                        theme.error_text
                    } else {
                        theme.toolbar_border
                    })
                    .text_size(px(14.0))
                    .child(locked.field.clone()),
            );
        let buttons = div()
            .flex()
            .flex_row()
            .gap_2()
            .child(
                styled_button("password-cancel", strings.cancel, true, false, &theme).on_click(
                    cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.run_command(ReaderCommand::Cancel, window, cx);
                    }),
                ),
            )
            .child(
                styled_button("password-open", strings.open, can_open, false, &theme).when(
                    can_open,
                    |button| {
                        button.on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.run_command(ReaderCommand::ConfirmPrompt, window, cx);
                        }))
                    },
                ),
            );
        Some(prompt_card(
            "password-prompt",
            theme,
            None,
            strings.password_required().into(),
            body,
            status,
            buttons,
        ))
    }

    /// The prompt's field follows a change of the UI language.
    pub(crate) fn relabel_password_field(&self, cx: &mut Context<'_, Self>) {
        if let DocState::Locked(locked) = &self.doc {
            let placeholder = self.strings().password_placeholder();
            locked
                .field
                .update(cx, |field, cx| field.set_placeholder(placeholder, cx));
        }
    }

    /// The open document was opened with a password.
    pub(crate) fn document_protected(&self) -> bool {
        matches!(&self.doc, DocState::Open(open) if open.protected)
    }

    /// Refuses a write of a document opened with a password, saying so in
    /// the write notice; true when refused.
    pub(crate) fn refuse_protected_write(&mut self, cx: &mut Context<'_, Self>) -> bool {
        if !self.document_protected() {
            return false;
        }
        log::info!("writing refused: the document was opened with a password");
        self.menus.open = None;
        // A write of another document may still run: its notice stays, its
        // button is the only way to cancel it.
        if !self.tools.writing() {
            self.tools.notice = Some(WriteNotice::Protected);
        }
        cx.notify();
        true
    }

    /// The panels that write (page tools, stamps, text edits, signature)
    /// close when a document opens with a password: they were left open
    /// from the previous document and would only lead to a refusal.
    fn close_write_panels(&mut self, cx: &mut Context<'_, Self>) {
        self.tools.open = false;
        if self.stamps.open {
            self.toggle_stamp_panel(cx);
        }
        if self.text_edits.open {
            self.hide_text_edit_panel(cx);
        }
        self.signing.open = false;
    }

    /// Development scripts: puts `password` into the prompt's field as
    /// `entry` says. False when no password prompt waits for input.
    pub(crate) fn enter_password(
        &mut self,
        password: &fastpdf_engine_api::Password,
        entry: PasswordEntry,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> bool {
        let field = match &self.doc {
            DocState::Locked(locked) if !locked.checking => locked.field.clone(),
            _ => return false,
        };
        field.update(cx, |field, cx| match entry {
            PasswordEntry::Compose => {
                field.replace_and_mark_text_in_range(None, password.as_str(), None, window, cx);
            }
            PasswordEntry::Confirm | PasswordEntry::Type => field.set_text(password.as_str(), cx),
        });
        if entry == PasswordEntry::Confirm {
            self.unlock_document(window, cx);
        }
        true
    }
}

/// How a development script enters a password.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PasswordEntry {
    /// Typed and confirmed (`password=`).
    Confirm,
    /// Typed only (`password-text=`).
    Type,
    /// Left as an unfinished IME composition (`password-compose=`).
    Compose,
}

/// The field holds a password to try: something typed, and no IME
/// composition in progress (its text is not what the user means yet).
fn ready_to_try(field: &TextInput) -> bool {
    !field.is_empty() && !field.composing()
}

#[cfg(test)]
mod tests {
    use fastpdf_core::keymap::PROMPT;
    use gpui::KeyContext;

    use super::FIELD_CONTEXT;
    use crate::text_input::EDIT_CONTEXT;

    #[test]
    fn the_field_types_and_answers_the_prompt() {
        let context = KeyContext::parse(FIELD_CONTEXT).unwrap();
        assert!(context.contains(EDIT_CONTEXT));
        assert!(context.contains(PROMPT));
    }
}
