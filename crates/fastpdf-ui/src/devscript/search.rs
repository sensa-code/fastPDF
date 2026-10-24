//! Dev-script steps for search: the find options (`crate::find_options`).

use gpui::Context;

use super::StepState;
use crate::find_options::FindOption;
use crate::reader::ReaderView;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SearchStep {
    /// `find-case=on|off`, `find-word=on|off`: sets a find option (and
    /// searches again when the find bar is open), like its toggle.
    Option(FindOption, bool),
}

impl SearchStep {
    /// The step written `name=value`; `None` for another step or a bad value.
    pub(super) fn parse(name: &str, value: Option<&str>) -> Option<Self> {
        let on = |value: Option<&str>| match value? {
            "on" => Some(true),
            "off" => Some(false),
            _ => None,
        };
        match name {
            "find-case" => Some(Self::Option(FindOption::MatchCase, on(value)?)),
            "find-word" => Some(Self::Option(FindOption::WholeWord, on(value)?)),
            _ => None,
        }
    }
}

impl ReaderView {
    pub(super) fn run_search_step(
        &mut self,
        step: &SearchStep,
        cx: &mut Context<'_, Self>,
    ) -> StepState {
        match *step {
            SearchStep::Option(option, on) => {
                self.set_find_option(option, on, cx);
                StepState::Done
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::devscript::{Step, parse};

    #[test]
    fn search_steps_parse() {
        assert_eq!(
            parse("find-case=on; find-word=off; find-case=off").unwrap(),
            vec![
                Step::Search(SearchStep::Option(FindOption::MatchCase, true)),
                Step::Search(SearchStep::Option(FindOption::WholeWord, false)),
                Step::Search(SearchStep::Option(FindOption::MatchCase, false)),
            ]
        );
        for bad in ["find-case", "find-case=yes", "find-word=", "find-word=ON"] {
            assert!(parse(bad).is_err(), "{bad}");
        }
    }
}
