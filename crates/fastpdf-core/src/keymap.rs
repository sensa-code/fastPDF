//! The single source of truth for keyboard shortcuts (spec §21: "集中管理
//! shortcuts，不要 scattered hardcoding").
//!
//! Keystrokes use GPUI's syntax (`ctrl-o`, `shift-f3`, `pagedown`) so the UI
//! can register this table directly; menus and tooltips look shortcuts up
//! here too. Ctrl + mouse wheel zoom is handled by the viewport element.

/// Everything a shortcut can trigger.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReaderCommand {
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
    /// Night mode: invert page colors.
    ToggleNightMode,
    /// Copy the selected text.
    Copy,
    SelectAll,
    /// Close the find bar / clear the selection.
    Cancel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Binding {
    pub keystroke: &'static str,
    pub command: ReaderCommand,
    /// Where the binding applies. `None`: the reader view; otherwise a
    /// focused component (e.g. [`FIND_BAR`]) whose binding wins there.
    pub context: Option<&'static str>,
}

/// Key context of the find bar's text field.
pub const FIND_BAR: &str = "FindBar";

const fn bind(keystroke: &'static str, command: ReaderCommand) -> Binding {
    Binding {
        keystroke,
        command,
        context: None,
    }
}

const fn bind_in(
    context: &'static str,
    keystroke: &'static str,
    command: ReaderCommand,
) -> Binding {
    Binding {
        keystroke,
        command,
        context: Some(context),
    }
}

/// Default bindings. Zoom shortcuts follow Adobe Reader / SumatraPDF
/// conventions (Ctrl+0 fit page, Ctrl+1 actual size, Ctrl+2 fit width).
pub const DEFAULT_BINDINGS: &[Binding] = {
    use ReaderCommand::*;
    &[
        bind("ctrl-o", OpenFile),
        bind("ctrl-w", CloseDocument),
        bind("ctrl-q", Quit),
        bind("ctrl-f", Find),
        bind("f3", FindNext),
        bind("shift-f3", FindPrevious),
        bind("ctrl-p", Print),
        bind("ctrl-=", ZoomIn),
        bind("ctrl-+", ZoomIn),
        bind("ctrl--", ZoomOut),
        bind("ctrl-0", FitPage),
        bind("ctrl-1", ActualSize),
        bind("ctrl-2", FitWidth),
        bind("ctrl-shift-=", RotateClockwise),
        bind("ctrl-shift--", RotateCounterClockwise),
        bind("right", NextPage),
        bind("left", PreviousPage),
        bind("home", FirstPage),
        bind("end", LastPage),
        bind("pagedown", PageDown),
        bind("space", PageDown),
        bind("pageup", PageUp),
        bind("shift-space", PageUp),
        bind("down", ScrollDown),
        bind("up", ScrollUp),
        bind("f11", ToggleFullscreen),
        bind("f4", ToggleSidebar),
        bind("ctrl-shift-d", ToggleDevOverlay),
        bind("ctrl-i", ToggleNightMode),
        bind("ctrl-c", Copy),
        bind("ctrl-a", SelectAll),
        bind("escape", Cancel),
        bind_in(FIND_BAR, "enter", FindNext),
        bind_in(FIND_BAR, "shift-enter", FindPrevious),
        bind_in(FIND_BAR, "escape", Cancel),
    ]
};

/// The command bound to `keystroke` in the reader view (no context).
pub fn command_for(keystroke: &str) -> Option<ReaderCommand> {
    command_in(None, keystroke)
}

/// The command bound to `keystroke` in `context`.
pub fn command_in(context: Option<&str>, keystroke: &str) -> Option<ReaderCommand> {
    DEFAULT_BINDINGS
        .iter()
        .find(|b| b.context == context && b.keystroke.eq_ignore_ascii_case(keystroke))
        .map(|b| b.command)
}

/// Keystrokes bound to `command`, primary first (for menus and tooltips).
pub fn keystrokes_for(command: ReaderCommand) -> impl Iterator<Item = &'static str> {
    DEFAULT_BINDINGS
        .iter()
        .filter(move |b| b.command == command)
        .map(|b| b.keystroke)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn keystrokes_are_unique_per_context() {
        let mut seen = HashSet::new();
        for b in DEFAULT_BINDINGS {
            assert!(
                seen.insert((b.context, b.keystroke.to_ascii_lowercase())),
                "duplicate {} in {:?}",
                b.keystroke,
                b.context
            );
        }
        assert_eq!(
            command_in(Some(FIND_BAR), "enter"),
            Some(ReaderCommand::FindNext)
        );
        assert_eq!(command_for("enter"), None);
    }

    #[test]
    fn spec_required_shortcuts_exist() {
        // spec §21: Ctrl+O, Ctrl+F, Ctrl+P, Ctrl++, Ctrl+-, Ctrl+0,
        // Page Up, Page Down, Home, End, F11.
        for key in [
            "ctrl-o", "ctrl-f", "ctrl-p", "ctrl-+", "ctrl--", "ctrl-0", "pageup", "pagedown",
            "home", "end", "f11",
        ] {
            assert!(command_for(key).is_some(), "{key} is unbound");
        }
        assert_eq!(command_for("CTRL-O"), Some(ReaderCommand::OpenFile));
        assert_eq!(keystrokes_for(ReaderCommand::ZoomIn).next(), Some("ctrl-="));
    }
}
