//! Dev-script steps for the toolbar's fit to the window and its "⋯" menu
//! (`crate::toolbar`).

use std::time::Instant;

use fastpdf_core::keymap::ReaderCommand;
use gpui::{Context, Window, px, size};

use super::{StepState, command_named};
use crate::reader::{Menu, ReaderView};
use crate::toolbar::{Group, Hidden};

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum ToolbarStep {
    /// `window-size=W,H`: resizes the window's content to W x H logical
    /// pixels and waits until it has that size.
    WindowSize(f32, f32),
    /// `wait-toolbar`: waits until a frame drawn after the step started
    /// needs no other one for the toolbar to fit.
    WaitToolbar,
    /// `expect-hidden=none` or `=rotate,night,...`: stops the script unless
    /// exactly these groups are in the "⋯" menu.
    ExpectHidden(Hidden),
    /// `more-menu`: opens (or closes) the "⋯" menu, like its button.
    MoreMenu,
    /// `more-run=COMMAND`: chooses that command's row in the open "⋯" menu.
    MoreRun(ReaderCommand),
}

impl ToolbarStep {
    /// The step written `name=value`; `None` for another step or a bad value.
    pub(super) fn parse(name: &str, value: Option<&str>) -> Option<Self> {
        Some(match (name, value) {
            ("window-size", Some(spec)) => {
                let (width, height) = spec.split_once(',')?;
                let side = |text: &str| {
                    text.trim()
                        .parse()
                        .ok()
                        .filter(|v: &f32| v.is_finite() && *v >= 1.0)
                };
                Self::WindowSize(side(width)?, side(height)?)
            }
            ("wait-toolbar", None) => Self::WaitToolbar,
            ("expect-hidden", Some("none")) => Self::ExpectHidden(Hidden::default()),
            ("expect-hidden", Some(names)) => Self::ExpectHidden(
                names
                    .split(',')
                    .map(|name| Group::from_name(name.trim()))
                    .try_fold(Hidden::default(), |hidden, group| Some(hidden.with(group?)))?,
            ),
            ("more-menu", None) => Self::MoreMenu,
            ("more-run", Some(name)) => Self::MoreRun(command_named(name)?),
            _ => return None,
        })
    }
}

impl ReaderView {
    pub(super) fn run_toolbar_step(
        &mut self,
        step: &ToolbarStep,
        first: bool,
        started: Instant,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> StepState {
        let hidden = self.toolbar_fit.hidden();
        match *step {
            ToolbarStep::WindowSize(width, height) => {
                // The platform resizes asynchronously: wait for the new size.
                if first {
                    window.resize(size(px(width), px(height)));
                }
                let now = window.viewport_size();
                let reached = (f32::from(now.width) - width).abs() < 1.0
                    && (f32::from(now.height) - height).abs() < 1.0;
                if reached {
                    StepState::Done
                } else {
                    StepState::Pending
                }
            }
            ToolbarStep::WaitToolbar => {
                if first {
                    cx.notify();
                }
                let drawn = self.render_started.is_some_and(|at| at > started);
                if drawn && self.toolbar_fit.settled() {
                    log::info!("dev script: toolbar settled, {hidden:?} in the overflow menu");
                    StepState::Done
                } else {
                    StepState::Pending
                }
            }
            ToolbarStep::ExpectHidden(expected) if expected == hidden => StepState::Done,
            ToolbarStep::ExpectHidden(expected) => StepState::Abort(format!(
                "{hidden:?} in the overflow menu, expected {expected:?}"
            )),
            ToolbarStep::MoreMenu if hidden.is_empty() => {
                StepState::Abort("everything fits; there is no overflow menu".into())
            }
            ToolbarStep::MoreMenu => {
                self.flip_menu(Menu::Overflow);
                cx.notify();
                StepState::Done
            }
            ToolbarStep::MoreRun(command) => {
                if !self.menus.is_open(Menu::Overflow) {
                    return StepState::Abort("the overflow menu is not open".into());
                }
                let listed = Group::ALL
                    .into_iter()
                    .any(|g| hidden.hides(g) && g.commands().contains(&command));
                if !listed {
                    return StepState::Abort(format!("{command:?} is not in the overflow menu"));
                }
                self.run_menu_item(command, window, cx);
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
    fn toolbar_steps_parse() {
        assert_eq!(
            parse(
                "window-size=826, 900; wait-toolbar; expect-hidden=none; \
                 expect-hidden=rotate, night; more-menu; more-run=RotateClockwise; \
                 expect-rotation=90"
            )
            .unwrap(),
            vec![
                Step::Toolbar(ToolbarStep::WindowSize(826.0, 900.0)),
                Step::Toolbar(ToolbarStep::WaitToolbar),
                Step::Toolbar(ToolbarStep::ExpectHidden(Hidden::default())),
                Step::Toolbar(ToolbarStep::ExpectHidden(
                    Hidden::default().with(Group::Rotate).with(Group::Night)
                )),
                Step::Toolbar(ToolbarStep::MoreMenu),
                Step::Toolbar(ToolbarStep::MoreRun(ReaderCommand::RotateClockwise)),
                Step::Check(super::super::checks::Check::ExpectRotation(90)),
            ]
        );
        for bad in [
            "window-size=826",
            "window-size=0,900",
            "window-size=x,900",
            "wait-toolbar=1",
            "expect-hidden",
            "expect-hidden=rotate,wheel",
            "more-menu=1",
            "more-run=Explode",
            "expect-rotation=45",
            "expect-rotation=450",
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
    }
}
