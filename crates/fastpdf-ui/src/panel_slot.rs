//! The panels that float at the top right of the document area: settings,
//! document properties, stamps, signing and text edits. They take the same
//! place, so at most one is open: opening one closes the others (a stamp
//! armed or a line being edited is ended as their own Close would). When a
//! closed panel held the keyboard (the signature's reason or location, the
//! line being edited), the reader takes it back, as their Close does.

use gpui::{Context, Window};

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
    /// Closes every top-right panel but `keep`, which is opening, and gives
    /// the keyboard back to the reader when a closed panel had it.
    pub(crate) fn make_room_for(
        &mut self,
        keep: CornerPanel,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let sign_field = keep != CornerPanel::Sign
            && self.signing.open
            && [&self.signing.reason, &self.signing.location]
                .iter()
                .any(|field| field.read(cx).focus_handle().is_focused(window));
        let line_field = keep != CornerPanel::TextEdits
            && self.text_edits.open
            && self
                .text_edits
                .field
                .read(cx)
                .focus_handle()
                .is_focused(window);
        self.clear_corner(keep, cx);
        if sign_field || line_field {
            window.focus(&self.focus, cx);
        }
    }

    /// Closes every top-right panel but `keep`; [`Self::make_room_for`]
    /// where the window is at hand.
    pub(crate) fn clear_corner(&mut self, keep: CornerPanel, cx: &mut Context<'_, Self>) {
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
