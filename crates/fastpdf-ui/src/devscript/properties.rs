//! Dev-script steps for the document properties panel
//! (`crate::properties`).

use std::path::PathBuf;

use gpui::Context;

use super::StepState;
use crate::reader::ReaderView;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PropertiesStep {
    /// `wait-properties`: asks for the document properties and waits until
    /// the answer is in (or none came); done at once without a document.
    Wait,
    /// `properties-save=PATH`: writes what Copy all would put on the
    /// clipboard to PATH (UTF-8), leaving the clipboard alone.
    Save(PathBuf),
}

impl PropertiesStep {
    /// The step written `name=value`; `None` for another step or a bad value.
    pub(super) fn parse(name: &str, value: Option<&str>) -> Option<Self> {
        match (name, value) {
            ("wait-properties", None) => Some(Self::Wait),
            ("properties-save", Some(path)) if !path.trim().is_empty() => {
                Some(Self::Save(PathBuf::from(path.trim())))
            }
            _ => None,
        }
    }
}

impl ReaderView {
    pub(super) fn run_properties_step(
        &mut self,
        step: &PropertiesStep,
        cx: &mut Context<'_, Self>,
    ) -> StepState {
        match step {
            // Settled first: a failure of the moment is an answer too, and
            // asking again would start over.
            PropertiesStep::Wait => {
                if self.properties_settled() {
                    return StepState::Done;
                }
                self.ensure_properties(cx);
                StepState::Pending
            }
            PropertiesStep::Save(path) => match std::fs::write(path, self.properties_text()) {
                Ok(()) => {
                    log::info!("dev script: properties saved to {}", path.display());
                    StepState::Done
                }
                Err(e) => StepState::Abort(format!("cannot write {}: {e}", path.display())),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::PropertiesStep;
    use crate::devscript::{Step, parse};

    #[test]
    fn properties_steps_parse() {
        assert_eq!(
            parse(r"cmd=DocumentProperties; wait-properties; properties-save=C:\x.txt").unwrap(),
            vec![
                Step::Command(fastpdf_core::keymap::ReaderCommand::DocumentProperties),
                Step::Properties(PropertiesStep::Wait),
                Step::Properties(PropertiesStep::Save(PathBuf::from(r"C:\x.txt"))),
            ]
        );
        for bad in ["wait-properties=1", "properties-save", "properties-save= "] {
            assert!(parse(bad).is_err(), "{bad}");
        }
    }
}
