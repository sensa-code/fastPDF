//! Dev-script steps for page labels (`crate::page_labels`) and the
//! go-to-page field (`crate::go_to_page`).

use fastpdf_core::go_to::PageInputError;
use gpui::{Context, Window};

use super::StepState;
use crate::reader::ReaderView;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LabelStep {
    /// `wait-labels`: asks for the page labels and waits until the answer
    /// is in (labels, none, or a failure).
    WaitLabels,
    /// `expect-label=TEXT`: the current page is labeled TEXT (waits for the
    /// labels first).
    ExpectLabel(String),
    /// `goto=TEXT`: once the labels are in, opens the page field, types
    /// TEXT and presses Enter; text that names no page leaves the field
    /// open with its message.
    GoTo(String),
}

impl LabelStep {
    /// The step written `name=value`; `None` for another step or a bad value.
    pub(super) fn parse(name: &str, value: Option<&str>) -> Option<Self> {
        match (name, value) {
            ("wait-labels", None) => Some(Self::WaitLabels),
            ("expect-label", Some(text)) if !text.is_empty() => {
                Some(Self::ExpectLabel(text.to_owned()))
            }
            ("goto", Some(text)) if !text.is_empty() => Some(Self::GoTo(text.to_owned())),
            _ => None,
        }
    }
}

impl ReaderView {
    pub(super) fn run_label_step(
        &mut self,
        step: &LabelStep,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> StepState {
        let Some(page) = self.session().map(|s| s.current_page()) else {
            return StepState::Abort("no document".into());
        };
        self.ensure_page_labels(cx);
        if !self.page_labels_settled() {
            return StepState::Pending;
        }
        match step {
            LabelStep::WaitLabels => StepState::Done,
            LabelStep::GoTo(text) => {
                self.start_go_to(window, cx);
                let field = self.go_to.field.clone();
                field.update(cx, |input, cx| input.set_text(text, cx));
                match self.confirm_go_to(window, cx) {
                    Some(Ok(page)) => {
                        log::info!("dev script: go to {text:?}: page {}", page.display_number());
                        StepState::Done
                    }
                    Some(Err(PageInputError::NoSuchPage)) => {
                        log::info!("dev script: go to {text:?}: no such page");
                        StepState::Done
                    }
                    Some(Err(PageInputError::Empty)) => StepState::Abort("nothing typed".into()),
                    None => StepState::Abort("the page field did not open".into()),
                }
            }
            LabelStep::ExpectLabel(expected) => {
                let number = page.display_number();
                match self.page_labels().and_then(|labels| labels.label(page)) {
                    Some(label) if label == *expected => StepState::Done,
                    Some(label) => StepState::Abort(format!(
                        "page {number} is labeled {label:?}, expected {expected:?}"
                    )),
                    None => StepState::Abort(format!(
                        "page {number} has no label, expected {expected:?}"
                    )),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::LabelStep;
    use crate::devscript::{Step, parse};

    #[test]
    fn label_steps_parse() {
        assert_eq!(
            parse(
                "wait-labels; expect-label=App-B; expect-label=附錄-5; goto=iii; goto=#5; goto=２"
            )
            .unwrap(),
            vec![
                Step::Labels(LabelStep::WaitLabels),
                Step::Labels(LabelStep::ExpectLabel("App-B".into())),
                Step::Labels(LabelStep::ExpectLabel("附錄-5".into())),
                Step::Labels(LabelStep::GoTo("iii".into())),
                Step::Labels(LabelStep::GoTo("#5".into())),
                Step::Labels(LabelStep::GoTo("２".into())),
            ]
        );
        for bad in [
            "wait-labels=1",
            "expect-label",
            "expect-label=",
            "goto",
            "goto=",
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
    }
}
