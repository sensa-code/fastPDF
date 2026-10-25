//! Dev-script steps for reading positions (`crate::reading`).

use gpui::Context;

use super::StepState;
use crate::reader::ReaderView;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PositionsStep {
    /// `remember=on` or `=off`: the settings panel's "Remember position".
    Remember(bool),
}

impl PositionsStep {
    /// The step written `name=value`; `None` for another step or a bad value.
    pub(super) fn parse(name: &str, value: Option<&str>) -> Option<Self> {
        match (name, value) {
            ("remember", Some("on")) => Some(Self::Remember(true)),
            ("remember", Some("off")) => Some(Self::Remember(false)),
            _ => None,
        }
    }
}

impl ReaderView {
    pub(super) fn run_positions_step(
        &mut self,
        step: &PositionsStep,
        cx: &mut Context<'_, Self>,
    ) -> StepState {
        match *step {
            PositionsStep::Remember(on) => self.set_remember_position(on, cx),
        }
        StepState::Done
    }
}

#[cfg(test)]
mod tests {
    use super::PositionsStep;
    use crate::devscript::{Step, parse};

    #[test]
    fn positions_steps_parse() {
        assert_eq!(
            parse("remember=off; remember=on; expect-page=12; expect-zoom=250%").unwrap(),
            vec![
                Step::Positions(PositionsStep::Remember(false)),
                Step::Positions(PositionsStep::Remember(true)),
                Step::Check(super::super::checks::Check::ExpectPage(12)),
                Step::Check(super::super::checks::Check::ExpectZoom(250)),
            ]
        );
        for bad in ["remember=maybe", "remember", "remember=", "expect-page=0"] {
            assert!(parse(bad).is_err(), "{bad}");
        }
    }
}
