//! Dev-script steps for search: the find options (`crate::find_options`)
//! and the result list (`crate::search_results`).

use gpui::Context;

use super::StepState;
use crate::find_options::FindOption;
use crate::reader::ReaderView;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SearchStep {
    /// `find-case=on|off`, `find-word=on|off`: sets a find option (and
    /// searches again when the find bar is open), like its toggle.
    Option(FindOption, bool),
    /// `find-result=N`: shows result N (one-based), as a click on its row.
    OpenResult(usize),
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
            "find-result" => Some(Self::OpenResult(
                value?.parse().ok().filter(|&n: &usize| n >= 1)?,
            )),
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
            SearchStep::OpenResult(n) => {
                let found = self.find.hits.len();
                if found >= n {
                    self.open_result(n - 1, cx);
                    StepState::Done
                } else if self.find.progress.finished || self.find.query.is_empty() {
                    StepState::Abort(format!("the search found {found} results, not {n}"))
                } else {
                    StepState::Pending
                }
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
        assert_eq!(
            parse("sidebar=results; find-result=3").unwrap(),
            vec![
                Step::Sidebar(Some(crate::sidebar::SidebarTab::Results)),
                Step::Search(SearchStep::OpenResult(3)),
            ]
        );
        for bad in [
            "find-case",
            "find-case=yes",
            "find-word=",
            "find-word=ON",
            "find-result=0",
            "find-result=-1",
            "find-result",
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
    }
}
