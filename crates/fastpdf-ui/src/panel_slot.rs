//! The panels that float at the top right of the document area: settings,
//! document properties, stamps, signing and text edits. They take the same
//! place, so at most one is open: opening one closes the others (a stamp
//! armed or a line being edited is ended as their own Close would).

use gpui::Context;

use crate::reader::ReaderView;

/// A panel of the top-right slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CornerPanel {
    Settings,
    Properties,
    Stamps,
    Sign,
    TextEdits,
}

impl ReaderView {
    /// Closes every top-right panel but `keep`, which is opening.
    pub(crate) fn make_room_for(&mut self, keep: CornerPanel, cx: &mut Context<'_, Self>) {
        if keep != CornerPanel::Settings {
            self.settings_open = false;
        }
        if keep != CornerPanel::Properties && self.properties.open {
            self.close_properties_panel();
        }
        if keep != CornerPanel::Stamps && self.stamps.open {
            self.toggle_stamp_panel(cx);
        }
        if keep != CornerPanel::Sign {
            self.signing.open = false;
        }
        if keep != CornerPanel::TextEdits && self.text_edits.open {
            self.hide_text_edit_panel(cx);
        }
    }
}
