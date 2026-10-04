//! GPUI actions for [`ReaderCommand`]s, bound from the central keymap table
//! (`fastpdf_core::keymap::DEFAULT_BINDINGS`, spec §21). No keystroke is
//! written anywhere else in the UI, except the provisional bindings below
//! for commands the keymap does not have yet.

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
        Copy,
        SelectAll,
        Cancel,
    ]
);

// Commands the central keymap does not define yet. Each is bound in the
// reader's context by `provisional_bindings` until `fastpdf_core::keymap`
// gains it as a `ReaderCommand` (then it moves into the list above and the
// provisional binding is deleted).
actions!(fastpdf, [ToggleNightMode]);

/// Keystrokes for the commands above, proposed for `fastpdf_core::keymap`.
/// `ctrl-i`: night mode inverts the page colors (mnemonic "invert").
pub(crate) fn provisional_bindings() -> Vec<(&'static str, Box<dyn Action>)> {
    vec![("ctrl-i", Box::new(ToggleNightMode))]
}

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
        C::Copy => Box::new(Copy),
        C::SelectAll => Box::new(SelectAll),
        C::Cancel => Box::new(Cancel),
    }
}

/// Builds a binding; invalid keystrokes are logged and skipped rather than
/// panicking at startup.
fn binding(keystroke: &str, action: Box<dyn Action>, context: &str) -> Option<KeyBinding> {
    let predicate = KeyBindingContextPredicate::parse(context).ok().map(Rc::new);
    KeyBinding::load(
        keystroke,
        action,
        predicate,
        false,
        None,
        &DummyKeyboardMapper,
    )
    .map_err(|e| log::warn!("skipping key binding {keystroke:?}: {e}"))
    .ok()
}

/// Registers every binding of the default keymap with GPUI — each in its
/// own context (the reader view, or e.g. the find bar) — plus the text
/// field's editing keys (`crate::text_input`).
pub fn bind_keys(cx: &mut App) {
    let mut bindings: Vec<KeyBinding> = DEFAULT_BINDINGS
        .iter()
        .filter_map(|b| {
            binding(
                b.keystroke,
                action_for(b.command),
                b.context.unwrap_or(KEY_CONTEXT),
            )
        })
        .collect();
    bindings.extend(
        provisional_bindings()
            .into_iter()
            .filter_map(|(keys, action)| binding(keys, action, KEY_CONTEXT)),
    );
    bindings.extend(
        crate::text_input::bindings()
            .into_iter()
            .filter_map(|(keys, action)| binding(keys, action, crate::text_input::EDIT_CONTEXT)),
    );
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
                Copy,
                SelectAll,
                Cancel,
            ]
        )
    };
}
pub(crate) use all_actions;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_binding_parses() {
        for b in DEFAULT_BINDINGS {
            assert!(
                binding(
                    b.keystroke,
                    action_for(b.command),
                    b.context.unwrap_or(KEY_CONTEXT)
                )
                .is_some(),
                "{} does not parse",
                b.keystroke
            );
        }
        for (keys, action) in crate::text_input::bindings() {
            assert!(
                binding(keys, action, crate::text_input::EDIT_CONTEXT).is_some(),
                "{keys} does not parse"
            );
        }
        for (keys, action) in provisional_bindings() {
            assert!(
                binding(keys, action, KEY_CONTEXT).is_some(),
                "{keys} does not parse"
            );
        }
    }

    /// A provisional binding must never take a keystroke from the central
    /// keymap; once the keymap binds it, the provisional one goes.
    #[test]
    fn provisional_bindings_do_not_shadow_the_keymap() {
        for (keys, _) in provisional_bindings() {
            assert_eq!(
                fastpdf_core::keymap::command_for(keys),
                None,
                "{keys} is in the keymap now: drop the provisional binding"
            );
        }
    }
}
