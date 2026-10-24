//! The single source of truth for keyboard shortcuts (spec §21: "集中管理
//! shortcuts，不要 scattered hardcoding").
//!
//! Keystrokes use GPUI's syntax (`ctrl-o`, `shift-f3`, `pagedown`) so the UI
//! can register this table directly; menus and tooltips look shortcuts up
//! here too. Ctrl + mouse wheel zoom is handled by the viewport element.

/// Declares [`ReaderCommand`] together with `ReaderCommand::ALL`, so a new
/// command cannot be left out of the list that dev scripts (`cmd=NAME`)
/// search.
macro_rules! reader_commands {
    (
        $(#[$meta:meta])*
        pub enum ReaderCommand {
            $($(#[$doc:meta])* $command:ident,)*
        }
    ) => {
        $(#[$meta])*
        pub enum ReaderCommand {
            $($(#[$doc])* $command,)*
        }

        impl ReaderCommand {
            /// Every command, in declaration order.
            pub const ALL: &'static [ReaderCommand] = &[$(ReaderCommand::$command),*];
        }
    };
}

reader_commands! {
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
        /// Back to the view before the last jump (go to page, first/last
        /// page, outline item, thumbnail, search hit, link).
        NavigateBack,
        /// Forward again after Back.
        NavigateForward,
        PageDown,
        PageUp,
        ScrollDown,
        ScrollUp,
        ToggleFullscreen,
        ToggleSidebar,
        ToggleDevOverlay,
        /// Night mode: invert page colors.
        ToggleNightMode,
        /// One page at a time.
        LayoutSinglePage,
        /// One page per row, scrolling.
        LayoutContinuous,
        /// Two pages side by side, one pair at a time.
        LayoutTwoPage,
        /// Two pages side by side, scrolling.
        LayoutTwoPageContinuous,
        /// Two-page layouts: show page 1 on its own (a book's cover) or not.
        ToggleCoverPage,
        /// Save the open file under a new name, unchanged (ADR 0013).
        SaveCopy,
        /// Extract, delete, rotate, reorder, split or merge pages into a new
        /// file (ADR 0013).
        PageTools,
        /// The stamp panel: seals placed on pages, saved as a new file.
        StampTool,
        /// Removes the selected stamp.
        RemoveStamp,
        /// The text edit panel: lines of text changed, saved as a new file.
        EditText,
        /// Applies the line being edited (its field's Enter).
        ConfirmEdit,
        /// The signature panel: sign the document with a certificate.
        Sign,
        /// Copy the selected text.
        Copy,
        SelectAll,
        /// Close the find bar / clear the selection.
        Cancel,
        /// Confirms the prompt card in front of the window (its Enter): opens
        /// an encrypted document with the typed password.
        ConfirmPrompt,
        /// Puts the cursor in the toolbar's page field (a page number or
        /// label).
        GoToPage,
        /// Goes to the page typed in the page field (its Enter).
        ConfirmGoToPage,
    }
}

impl ReaderCommand {
    /// The commands that write a new PDF (ADR 0013); refused for documents
    /// opened with a password (ADR 0014).
    pub fn writes_document(self) -> bool {
        use ReaderCommand::*;
        matches!(self, SaveCopy | PageTools | StampTool | EditText | Sign)
    }

    /// The commands that still work while a prompt card blocks the window:
    /// answering it, leaving it (another file, closing, quitting) and the
    /// window itself.
    pub fn allowed_during_prompt(self) -> bool {
        use ReaderCommand::*;
        matches!(
            self,
            Cancel
                | ConfirmPrompt
                | Quit
                | OpenFile
                | CloseDocument
                | ToggleFullscreen
                | ToggleDevOverlay
        )
    }
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
/// Key context of the field that edits a line of a page's text.
pub const LINE_EDITOR: &str = "LineEditor";
/// Key context of the prompt card that blocks the window (a password, a
/// confirmation) and of the text field inside it.
pub const PROMPT: &str = "Prompt";
/// Key context of the toolbar's page field (go to a page).
pub const PAGE_FIELD: &str = "PageField";

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
        bind("ctrl-shift-s", SaveCopy),
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
        bind("alt-left", NavigateBack),
        bind("alt-right", NavigateForward),
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
        bind("delete", RemoveStamp),
        bind("ctrl-g", GoToPage),
        bind_in(FIND_BAR, "enter", FindNext),
        bind_in(FIND_BAR, "shift-enter", FindPrevious),
        bind_in(FIND_BAR, "escape", Cancel),
        bind_in(LINE_EDITOR, "enter", ConfirmEdit),
        bind_in(LINE_EDITOR, "escape", Cancel),
        bind_in(PROMPT, "enter", ConfirmPrompt),
        bind_in(PROMPT, "escape", Cancel),
        bind_in(PAGE_FIELD, "enter", ConfirmGoToPage),
        bind_in(PAGE_FIELD, "escape", Cancel),
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
        assert_eq!(
            command_in(Some(PROMPT), "enter"),
            Some(ReaderCommand::ConfirmPrompt)
        );
        assert_eq!(
            command_in(Some(PROMPT), "escape"),
            Some(ReaderCommand::Cancel)
        );
        assert_eq!(command_for("ctrl-g"), Some(ReaderCommand::GoToPage));
        assert_eq!(
            command_in(Some(PAGE_FIELD), "enter"),
            Some(ReaderCommand::ConfirmGoToPage)
        );
        assert_eq!(
            command_in(Some(PAGE_FIELD), "escape"),
            Some(ReaderCommand::Cancel)
        );
    }

    #[test]
    fn write_and_prompt_command_groups() {
        use ReaderCommand::*;
        let writes: Vec<_> = ReaderCommand::ALL
            .iter()
            .copied()
            .filter(|c| c.writes_document())
            .collect();
        assert_eq!(writes, [SaveCopy, PageTools, StampTool, EditText, Sign]);
        for command in ReaderCommand::ALL {
            // Nothing that writes, moves the view or opens a panel gets past
            // a prompt.
            if command.allowed_during_prompt() {
                assert!(!command.writes_document(), "{command:?}");
            }
        }
        assert!(ConfirmPrompt.allowed_during_prompt() && Cancel.allowed_during_prompt());
        assert!(!Print.allowed_during_prompt() && !NavigateBack.allowed_during_prompt());
        assert!(!GoToPage.allowed_during_prompt() && !ConfirmGoToPage.allowed_during_prompt());
    }

    #[test]
    fn all_lists_every_command_once() {
        for (index, &command) in ReaderCommand::ALL.iter().enumerate() {
            assert_eq!(command as usize, index, "{command:?} is out of place");
        }
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
        assert_eq!(command_for("alt-left"), Some(ReaderCommand::NavigateBack));
        assert_eq!(
            command_for("alt-right"),
            Some(ReaderCommand::NavigateForward)
        );
        assert_eq!(command_for("left"), Some(ReaderCommand::PreviousPage));
        assert_eq!(command_for("right"), Some(ReaderCommand::NextPage));
        assert_eq!(keystrokes_for(ReaderCommand::ZoomIn).next(), Some("ctrl-="));
    }
}
