//! GPUI actions for [`ReaderCommand`]s, bound from the central keymap table
//! (`fastpdf_core::keymap::DEFAULT_BINDINGS`, spec §21). No keystroke is
//! written anywhere else in the UI.

use std::rc::Rc;

use fastpdf_core::keymap::{DEFAULT_BINDINGS, ReaderCommand};
use gpui::{Action, App, DummyKeyboardMapper, KeyBinding, KeyBindingContextPredicate, actions};

/// Key context set on the reader's root element; bindings only apply inside.
pub const KEY_CONTEXT: &str = "FastPdf";

// One unit action per command. The names match `ReaderCommand` variants so
// `route_actions!` can map them back without a lookup table.
actions!(
    fastpdf,
    [
        OpenFile,
        CloseDocument,
        Quit,
        Find,
        FindNext,
        FindPrevious,
        Print,
        ZoomIn,
        ZoomOut,
        ActualSize,
        FitPage,
        FitWidth,
        RotateClockwise,
        RotateCounterClockwise,
        NextPage,
        PreviousPage,
        FirstPage,
        LastPage,
        PageDown,
        PageUp,
        ScrollDown,
        ScrollUp,
        ToggleFullscreen,
        ToggleSidebar,
        ToggleDevOverlay,
    ]
);

/// The action dispatched for `command`. Exhaustive on purpose: a new
/// command does not compile until it has an action.
fn action_for(command: ReaderCommand) -> Box<dyn Action> {
    use ReaderCommand as C;
    match command {
        C::OpenFile => Box::new(OpenFile),
        C::CloseDocument => Box::new(CloseDocument),
        C::Quit => Box::new(Quit),
        C::Find => Box::new(Find),
        C::FindNext => Box::new(FindNext),
        C::FindPrevious => Box::new(FindPrevious),
        C::Print => Box::new(Print),
        C::ZoomIn => Box::new(ZoomIn),
        C::ZoomOut => Box::new(ZoomOut),
        C::ActualSize => Box::new(ActualSize),
        C::FitPage => Box::new(FitPage),
        C::FitWidth => Box::new(FitWidth),
        C::RotateClockwise => Box::new(RotateClockwise),
        C::RotateCounterClockwise => Box::new(RotateCounterClockwise),
        C::NextPage => Box::new(NextPage),
        C::PreviousPage => Box::new(PreviousPage),
        C::FirstPage => Box::new(FirstPage),
        C::LastPage => Box::new(LastPage),
        C::PageDown => Box::new(PageDown),
        C::PageUp => Box::new(PageUp),
        C::ScrollDown => Box::new(ScrollDown),
        C::ScrollUp => Box::new(ScrollUp),
        C::ToggleFullscreen => Box::new(ToggleFullscreen),
        C::ToggleSidebar => Box::new(ToggleSidebar),
        C::ToggleDevOverlay => Box::new(ToggleDevOverlay),
    }
}

/// Registers every binding of the default keymap with GPUI. Invalid
/// keystrokes are logged and skipped rather than panicking at startup.
pub fn bind_keys(cx: &mut App) {
    let context = KeyBindingContextPredicate::parse(KEY_CONTEXT)
        .ok()
        .map(Rc::new);
    let bindings: Vec<KeyBinding> = DEFAULT_BINDINGS
        .iter()
        .filter_map(|binding| {
            KeyBinding::load(
                binding.keystroke,
                action_for(binding.command),
                context.clone(),
                false,
                None,
                &DummyKeyboardMapper,
            )
            .map_err(|e| log::warn!("skipping key binding {:?}: {e}", binding.keystroke))
            .ok()
        })
        .collect();
    log::debug!("bound {} keystrokes", bindings.len());
    cx.bind_keys(bindings);
}

/// Adds an `on_action` listener per command to an element, each forwarding
/// to `ReaderView::run_command`.
macro_rules! route_actions {
    ($element:expr, $cx:expr, [$($name:ident),* $(,)?]) => {
        $element$(.on_action($cx.listener(
            |this: &mut $crate::reader::ReaderView, _: &$crate::actions::$name, window, cx| {
                this.run_command(fastpdf_core::keymap::ReaderCommand::$name, window, cx)
            },
        )))*
    };
}
pub(crate) use route_actions;

/// Every action, for [`route_actions!`].
macro_rules! all_actions {
    ($element:expr, $cx:expr) => {
        $crate::actions::route_actions!(
            $element,
            $cx,
            [
                OpenFile,
                CloseDocument,
                Quit,
                Find,
                FindNext,
                FindPrevious,
                Print,
                ZoomIn,
                ZoomOut,
                ActualSize,
                FitPage,
                FitWidth,
                RotateClockwise,
                RotateCounterClockwise,
                NextPage,
                PreviousPage,
                FirstPage,
                LastPage,
                PageDown,
                PageUp,
                ScrollDown,
                ScrollUp,
                ToggleFullscreen,
                ToggleSidebar,
                ToggleDevOverlay,
            ]
        )
    };
}
pub(crate) use all_actions;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_default_binding_parses() {
        let context = KeyBindingContextPredicate::parse(KEY_CONTEXT)
            .ok()
            .map(Rc::new);
        for binding in DEFAULT_BINDINGS {
            assert!(
                KeyBinding::load(
                    binding.keystroke,
                    action_for(binding.command),
                    context.clone(),
                    false,
                    None,
                    &DummyKeyboardMapper,
                )
                .is_ok(),
                "{} does not parse",
                binding.keystroke
            );
        }
    }
}
