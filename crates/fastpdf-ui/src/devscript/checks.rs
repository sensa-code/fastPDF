//! Assertion steps of development scripts: each one passes when the reader
//! is in the expected state and otherwise stops the script with a message
//! saying what it found, so a GUI check's log proves where the view went.

use super::StepState;
use crate::reader::ReaderView;

/// A condition on the reader's state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Check {
    /// `expect-page=N`: the current page (one-based) is N.
    ExpectPage(u32),
    /// `expect-zoom=PERCENT`: the zoom, rounded to a whole percent, is PERCENT.
    ExpectZoom(u32),
}

impl Check {
    /// The check written `name=value`; `None` for another step or a bad value.
    pub(super) fn parse(name: &str, value: Option<&str>) -> Option<Self> {
        let positive = |text: &str| text.parse().ok().filter(|&n: &u32| n >= 1);
        Some(match (name, value?) {
            ("expect-page", n) => Self::ExpectPage(positive(n)?),
            ("expect-zoom", p) => Self::ExpectZoom(positive(p.trim_end_matches('%'))?),
            _ => return None,
        })
    }
}

impl ReaderView {
    /// Done when `check` holds; otherwise the script stops.
    pub(super) fn run_check_step(&self, check: Check) -> StepState {
        let Some(session) = self.session() else {
            return StepState::Abort("no document".into());
        };
        let failure = match check {
            Check::ExpectPage(expected) => {
                let page = session.current_page().display_number();
                (page != expected).then(|| format!("on page {page}, expected {expected}"))
            }
            Check::ExpectZoom(expected) => {
                let zoom = session.zoom().percent();
                (zoom != expected).then(|| format!("zoom {zoom}%, expected {expected}%"))
            }
        };
        failure.map_or(StepState::Done, StepState::Abort)
    }
}

#[cfg(test)]
mod tests {
    use super::Check;
    use crate::devscript::{Step, parse};

    #[test]
    fn checks_parse() {
        assert_eq!(
            parse("expect-page=3; expect-zoom=250%; expect-zoom=80").unwrap(),
            vec![
                Step::Check(Check::ExpectPage(3)),
                Step::Check(Check::ExpectZoom(250)),
                Step::Check(Check::ExpectZoom(80)),
            ]
        );
        for bad in [
            "expect-page=0",
            "expect-page=-1",
            "expect-page",
            "expect-zoom=x",
            "expect-zoom=0%",
            "expect-zoom",
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
    }
}
