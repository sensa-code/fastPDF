//! Dev-script steps for page labels (`crate::page_labels`).

use gpui::Context;

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
}

impl LabelStep {
    /// The step written `name=value`; `None` for another step or a bad value.
    pub(super) fn parse(name: &str, value: Option<&str>) -> Option<Self> {
        match (name, value) {
            ("wait-labels", None) => Some(Self::WaitLabels),
            ("expect-label", Some(text)) if !text.is_empty() => {
                Some(Self::ExpectLabel(text.to_owned()))
            }
            _ => None,
        }
    }
}

impl ReaderView {
    pub(super) fn run_label_step(
        &mut self,
        step: &LabelStep,
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
            parse("wait-labels; expect-label=App-B; expect-label=附錄-5").unwrap(),
            vec![
                Step::Labels(LabelStep::WaitLabels),
                Step::Labels(LabelStep::ExpectLabel("App-B".into())),
                Step::Labels(LabelStep::ExpectLabel("附錄-5".into())),
            ]
        );
        for bad in ["wait-labels=1", "expect-label", "expect-label="] {
            assert!(parse(bad).is_err(), "{bad}");
        }
    }
}
