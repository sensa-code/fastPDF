//! Generic steps of development scripts that every feature's GUI check can
//! use. Assertions pass when the reader is in the expected state and
//! otherwise stop the script with a message saying what they found, so a
//! GUI check's log proves where the view went; the others drive shared
//! parts of the reader (the outline, the prompt card).

use fastpdf_engine_api::Password;
use gpui::{Context, Window};

use super::StepState;
use crate::prompt::PromptKind;
use crate::reader::{DocState, ReaderView};
use crate::sidebar::OutlineState;

/// A condition on the reader's state, or a shared action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Check {
    /// `expect-page=N`: the current page (one-based) is N.
    ExpectPage(u32),
    /// `expect-zoom=PERCENT`: the zoom, rounded to a whole percent, is PERCENT.
    ExpectZoom(u32),
    /// `expect-history=B,F`: Back and Forward hold B and F views.
    ExpectHistory(usize, usize),
    /// `expect-focus=find`, `=document` or `=prompt`: the find field, the
    /// reader itself or the password prompt's field has the keyboard focus.
    ExpectFocus(FocusTarget),
    /// `focus=document`: gives the keyboard focus to the reader itself, as
    /// GPUI does on a mouse press on the toolbar or the sidebar.
    FocusDocument,
    /// `outline=N`: activates outline row N (one-based, as listed), waiting
    /// while the outline loads.
    Outline(usize),
    /// `wait-prompt=password`: waits until that prompt waits for input.
    WaitPrompt(PromptKind),
    /// `prompt-cancel`: cancels the prompt, like Esc.
    PromptCancel,
    /// `password=TEXT`: types TEXT into the password prompt and confirms it;
    /// `password-text=TEXT` only types it (to see the masked field). For
    /// fixture passwords only; `Debug` (the step log) never shows them.
    Password { text: Password, confirm: bool },
}

/// What `expect-focus` names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FocusTarget {
    Find,
    Document,
    Prompt,
}

impl Check {
    /// The check written `name=value`; `None` for another step or a bad value.
    pub(super) fn parse(name: &str, value: Option<&str>) -> Option<Self> {
        if (name, value) == ("prompt-cancel", None) {
            return Some(Self::PromptCancel);
        }
        let positive = |text: &str| text.parse().ok().filter(|&n: &u32| n >= 1);
        Some(match (name, value?) {
            ("expect-page", n) => Self::ExpectPage(positive(n)?),
            ("expect-zoom", p) => Self::ExpectZoom(positive(p.trim_end_matches('%'))?),
            ("expect-history", depths) => {
                let (back, forward) = depths.split_once(',')?;
                Self::ExpectHistory(back.trim().parse().ok()?, forward.trim().parse().ok()?)
            }
            ("outline", n) => Self::Outline(n.parse().ok().filter(|&n: &usize| n >= 1)?),
            ("expect-focus", "find") => Self::ExpectFocus(FocusTarget::Find),
            ("expect-focus", "document") => Self::ExpectFocus(FocusTarget::Document),
            ("expect-focus", "prompt") => Self::ExpectFocus(FocusTarget::Prompt),
            ("focus", "document") => Self::FocusDocument,
            ("wait-prompt", kind) => Self::WaitPrompt(PromptKind::from_name(kind)?),
            ("password" | "password-text", text) if !text.is_empty() => Self::Password {
                text: Password::from(text),
                confirm: name == "password",
            },
            _ => return None,
        })
    }
}

impl ReaderView {
    /// Done when `check` holds; otherwise the script stops.
    pub(super) fn run_check_step(
        &mut self,
        check: &Check,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> StepState {
        match check {
            Check::Outline(row) => return self.run_outline_step(row - 1, cx),
            Check::ExpectFocus(target) => {
                let focused = match target {
                    FocusTarget::Find => self.find.input.read(cx).focus_handle().is_focused(window),
                    FocusTarget::Document => self.focus.is_focused(window),
                    FocusTarget::Prompt => self.password_field_focused(window, cx),
                };
                return if focused {
                    StepState::Done
                } else {
                    StepState::Abort(format!("{target:?} does not have the keyboard focus"))
                };
            }
            Check::FocusDocument => {
                window.focus(&self.focus, cx);
                return StepState::Done;
            }
            Check::WaitPrompt(kind) => return self.prompt_waiting(*kind),
            Check::PromptCancel => {
                return if self.cancel_prompt(window, cx) {
                    StepState::Done
                } else {
                    StepState::Abort("no prompt to cancel".into())
                };
            }
            Check::Password { text, confirm } => {
                return match &self.doc {
                    // An earlier attempt is still being checked.
                    DocState::Locked(locked) if locked.checking => StepState::Pending,
                    DocState::Locked(_) => {
                        self.enter_password(text, *confirm, window, cx);
                        StepState::Done
                    }
                    _ => StepState::Abort("no password prompt".into()),
                };
            }
            _ => {}
        }
        let Some(session) = self.session() else {
            return StepState::Abort("no document".into());
        };
        let failure = match *check {
            Check::ExpectPage(expected) => {
                let page = session.current_page().display_number();
                (page != expected).then(|| format!("on page {page}, expected {expected}"))
            }
            Check::ExpectZoom(expected) => {
                let zoom = session.zoom().percent();
                (zoom != expected).then(|| format!("zoom {zoom}%, expected {expected}%"))
            }
            Check::ExpectHistory(back, forward) => {
                let depth = session.history_depth();
                (depth != (back, forward))
                    .then(|| format!("history {},{}, expected {back},{forward}", depth.0, depth.1))
            }
            Check::Outline(_)
            | Check::ExpectFocus(_)
            | Check::FocusDocument
            | Check::WaitPrompt(_)
            | Check::PromptCancel
            | Check::Password { .. } => None,
        };
        failure.map_or(StepState::Done, StepState::Abort)
    }

    /// Done once the prompt `kind` waits for input; stops if the document
    /// opened or failed instead.
    fn prompt_waiting(&self, kind: PromptKind) -> StepState {
        match (&self.doc, kind) {
            (DocState::Locked(locked), PromptKind::Password) if !locked.checking => StepState::Done,
            (DocState::Locked(_) | DocState::Loading { .. }, _) => StepState::Pending,
            (DocState::Open(_), _) => StepState::Abort("the document opened, no prompt".into()),
            (DocState::Failed { failure, .. }, _) => StepState::Abort(failure.to_string()),
            (DocState::Empty, _) => StepState::Abort("no document".into()),
        }
    }

    /// Activates outline row `index` (zero-based) like a click on it.
    fn run_outline_step(&mut self, index: usize, cx: &mut Context<'_, Self>) -> StepState {
        if self.session().is_none() {
            return StepState::Abort("no document".into());
        }
        self.ensure_outline(cx);
        let target = match &self.sidebar.outline {
            OutlineState::NotLoaded | OutlineState::Loading(_) => return StepState::Pending,
            OutlineState::Empty => return StepState::Abort("the document has no outline".into()),
            OutlineState::Failed(e) => return StepState::Abort(format!("outline failed: {e}")),
            OutlineState::Ready(rows) => rows.get(index).map(|row| row.destination.is_some()),
        };
        match target {
            Some(true) => {
                self.activate_outline(index, cx);
                StepState::Done
            }
            Some(false) => StepState::Abort(format!("outline row {} has no target", index + 1)),
            None => StepState::Abort(format!("the outline has no row {}", index + 1)),
        }
    }
}

#[cfg(test)]
mod tests {
    use fastpdf_engine_api::Password;

    use super::Check;
    use crate::devscript::{Step, parse};
    use crate::prompt::PromptKind;

    #[test]
    fn checks_parse() {
        assert_eq!(
            parse(
                "expect-page=3; expect-zoom=250%; expect-zoom=80; cmd=NavigateBack; \
                 expect-history=2,0; expect-history= 0 , 12 ; outline=5; \
                 expect-focus=find; expect-focus=document; expect-focus=prompt; \
                 focus=document"
            )
            .unwrap(),
            vec![
                Step::Check(Check::ExpectPage(3)),
                Step::Check(Check::ExpectZoom(250)),
                Step::Check(Check::ExpectZoom(80)),
                Step::Command(fastpdf_core::keymap::ReaderCommand::NavigateBack),
                Step::Check(Check::ExpectHistory(2, 0)),
                Step::Check(Check::ExpectHistory(0, 12)),
                Step::Check(Check::Outline(5)),
                Step::Check(Check::ExpectFocus(super::FocusTarget::Find)),
                Step::Check(Check::ExpectFocus(super::FocusTarget::Document)),
                Step::Check(Check::ExpectFocus(super::FocusTarget::Prompt)),
                Step::Check(Check::FocusDocument),
            ]
        );
        for bad in [
            "expect-page=0",
            "expect-page=-1",
            "expect-page",
            "expect-zoom=x",
            "expect-zoom=0%",
            "expect-zoom",
            "expect-history=1",
            "expect-history=1,x",
            "expect-history=-1,0",
            "outline=0",
            "outline",
            "expect-focus",
            "expect-focus=sidebar",
            "focus",
            "focus=find",
            "wait-prompt",
            "wait-prompt=nothing",
            "prompt-cancel=1",
            "password",
            "password=",
            "password-text=",
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn prompt_steps_parse_and_hide_the_password() {
        let steps =
            parse("wait-prompt=password; password=zq9; prompt-cancel; password-text=測試密碼2026")
                .unwrap();
        assert_eq!(
            steps,
            vec![
                Step::Check(Check::WaitPrompt(PromptKind::Password)),
                Step::Check(Check::Password {
                    text: Password::from("zq9"),
                    confirm: true
                }),
                Step::Check(Check::PromptCancel),
                Step::Check(Check::Password {
                    text: Password::from("測試密碼2026"),
                    confirm: false
                }),
            ]
        );
        // The runner logs every step with Debug.
        let logged = format!("{steps:?} {steps:#?}");
        assert!(
            !logged.contains("zq9") && !logged.contains("測試"),
            "{logged}"
        );
        assert!(logged.contains("<redacted>"));
    }
}
