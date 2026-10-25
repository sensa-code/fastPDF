//! Each toolbar group's controls (`overflow::Group`), and its rows in the
//! "⋯" menu while the window has no room for them.

use fastpdf_core::keymap::ReaderCommand;
use gpui::{
    AnyElement, Context, Div, IntoElement, ParentElement, SharedString, Stateful, Styled, Window,
    div, px,
};

use super::overflow::{Group, Hidden};
use super::{
    GAP, Tools, edit_control, edit_items, label, layout_control, layout_items, menu_divider,
    menu_item,
};
use crate::reader::ReaderView;
use crate::theme::Theme;

/// The document's name: it takes the room the controls leave, down to none.
pub(super) fn title(view: &ReaderView, theme: &Theme) -> Div {
    let name = match &view.doc {
        crate::reader::DocState::Open(open) => open.name.clone(),
        _ => SharedString::default(),
    };
    div()
        .flex_1()
        .min_w_0()
        .flex()
        .justify_end()
        .overflow_hidden()
        .child(
            div()
                .text_color(theme.text_muted)
                .text_size(px(13.0))
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .max_w(px(360.0))
                .child(name),
        )
}

/// The toolbar's controls of `group`.
pub(super) fn group_control(
    group: Group,
    view: &ReaderView,
    tools: Tools,
    cx: &mut Context<'_, ReaderView>,
) -> AnyElement {
    use ReaderCommand as C;
    let strings = tools.strings;
    let session = view.session();
    let has_doc = session.is_some();
    let pair = |first: Stateful<Div>, second: Stateful<Div>| {
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(GAP))
            .child(first)
            .child(second)
            .into_any_element()
    };
    match group {
        Group::Sidebar => tools
            .toggle(
                "sidebar",
                strings.sidebar,
                strings.tip_sidebar,
                C::ToggleSidebar,
                view.sidebar.open,
                cx,
            )
            .into_any_element(),
        Group::Open => tools
            .button(
                "open",
                strings.open,
                strings.tip_open,
                C::OpenFile,
                true,
                cx,
            )
            .into_any_element(),
        Group::History => pair(
            tools.button(
                "back",
                "\u{2190}",
                strings.tip_back(),
                C::NavigateBack,
                session.is_some_and(|s| s.can_go_back()),
                cx,
            ),
            tools.button(
                "forward",
                "\u{2192}",
                strings.tip_forward(),
                C::NavigateForward,
                session.is_some_and(|s| s.can_go_forward()),
                cx,
            ),
        ),
        Group::PrevPage => tools
            .button(
                "prev",
                "\u{2039}",
                strings.tip_previous_page,
                C::PreviousPage,
                has_doc,
                cx,
            )
            .into_any_element(),
        Group::NextPage => tools
            .button(
                "next",
                "\u{203a}",
                strings.tip_next_page,
                C::NextPage,
                has_doc,
                cx,
            )
            .into_any_element(),
        Group::Zoom => {
            let zoom =
                session.map_or_else(|| "-".to_owned(), |s| format!("{}%", s.zoom().percent()));
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap(px(GAP))
                .child(tools.button(
                    "zoom-out",
                    "\u{2212}",
                    strings.tip_zoom_out,
                    C::ZoomOut,
                    has_doc,
                    cx,
                ))
                .child(label(zoom, 40.0))
                .child(tools.button("zoom-in", "+", strings.tip_zoom_in, C::ZoomIn, has_doc, cx))
                .into_any_element()
        }
        Group::FitWidth => tools
            .button(
                "fit-width",
                strings.fit_width,
                strings.tip_fit_width,
                C::FitWidth,
                has_doc,
                cx,
            )
            .into_any_element(),
        Group::FitPage => tools
            .button(
                "fit-page",
                strings.fit_page,
                strings.tip_fit_page,
                C::FitPage,
                has_doc,
                cx,
            )
            .into_any_element(),
        Group::Layout => layout_control(view, tools, cx).into_any_element(),
        Group::Rotate => pair(
            tools.button(
                "rotate-ccw",
                "\u{21ba}",
                strings.tip_rotate_left,
                C::RotateCounterClockwise,
                has_doc,
                cx,
            ),
            tools.button(
                "rotate-cw",
                "\u{21bb}",
                strings.tip_rotate_right,
                C::RotateClockwise,
                has_doc,
                cx,
            ),
        ),
        Group::Find => tools
            .toggle(
                "find",
                strings.find,
                strings.tip_find,
                C::Find,
                view.find.open,
                cx,
            )
            .into_any_element(),
        // Night mode (inverted pages) is a setting, so it works without a
        // document too.
        Group::Night => tools
            .toggle(
                "night",
                strings.night,
                strings.tip_night,
                C::ToggleNightMode,
                view.settings.night_mode,
                cx,
            )
            .into_any_element(),
        Group::Edit => edit_control(view, tools, cx).into_any_element(),
        Group::Properties => tools
            .toggle(
                "properties",
                "\u{24D8}",
                strings.tip_properties(),
                C::DocumentProperties,
                view.properties.open,
                cx,
            )
            .text_size(px(16.0))
            .into_any_element(),
    }
}

/// The overflow menu's rows: each hidden group's controls, in toolbar order.
pub(super) fn menu_rows(
    view: &ReaderView,
    tools: Tools,
    hidden: Hidden,
    cx: &mut Context<'_, ReaderView>,
) -> Vec<AnyElement> {
    use ReaderCommand as C;
    let strings = tools.strings;
    let session = view.session();
    let has_doc = session.is_some();
    let mut rows = Vec::new();
    for group in Group::ALL.into_iter().filter(|g| hidden.hides(*g)) {
        if !rows.is_empty() {
            rows.push(menu_divider(&tools.theme));
        }
        let mut row = |id: &'static str, label, checked, enabled, command| {
            rows.push(
                menu_item(id, label, checked, enabled, command, tools, cx).into_any_element(),
            );
        };
        match group {
            Group::Sidebar => row(
                "more-sidebar",
                strings.sidebar,
                view.sidebar.open,
                true,
                C::ToggleSidebar,
            ),
            Group::Open => row("more-open", strings.open, false, true, C::OpenFile),
            Group::History => {
                let back = session.is_some_and(|s| s.can_go_back());
                let forward = session.is_some_and(|s| s.can_go_forward());
                row(
                    "more-back",
                    strings.tip_back(),
                    false,
                    back,
                    C::NavigateBack,
                );
                row(
                    "more-forward",
                    strings.tip_forward(),
                    false,
                    forward,
                    C::NavigateForward,
                );
            }
            Group::PrevPage => row(
                "more-prev",
                strings.tip_previous_page,
                false,
                has_doc,
                C::PreviousPage,
            ),
            Group::NextPage => row(
                "more-next",
                strings.tip_next_page,
                false,
                has_doc,
                C::NextPage,
            ),
            Group::Zoom => {
                row(
                    "more-zoom-out",
                    strings.tip_zoom_out,
                    false,
                    has_doc,
                    C::ZoomOut,
                );
                row(
                    "more-zoom-in",
                    strings.tip_zoom_in,
                    false,
                    has_doc,
                    C::ZoomIn,
                );
            }
            Group::FitWidth => row(
                "more-fit-width",
                strings.fit_width,
                false,
                has_doc,
                C::FitWidth,
            ),
            Group::FitPage => row(
                "more-fit-page",
                strings.fit_page,
                false,
                has_doc,
                C::FitPage,
            ),
            Group::Rotate => {
                row(
                    "more-rotate-ccw",
                    strings.tip_rotate_left,
                    false,
                    has_doc,
                    C::RotateCounterClockwise,
                );
                row(
                    "more-rotate-cw",
                    strings.tip_rotate_right,
                    false,
                    has_doc,
                    C::RotateClockwise,
                );
            }
            Group::Find => row("more-find", strings.find, view.find.open, true, C::Find),
            Group::Night => row(
                "more-night",
                strings.night_mode,
                view.settings.night_mode,
                true,
                C::ToggleNightMode,
            ),
            Group::Properties => row(
                "more-properties",
                strings.tip_properties(),
                view.properties.open,
                true,
                C::DocumentProperties,
            ),
            Group::Layout => rows.extend(layout_items(view, tools, cx)),
            Group::Edit => rows.extend(edit_items(view, tools, cx)),
        }
    }
    rows
}

impl ReaderView {
    /// A menu row was chosen: the menu closes and the command runs, as its
    /// shortcut would.
    pub(crate) fn run_menu_item(
        &mut self,
        command: ReaderCommand,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        self.menus.open = None;
        self.run_command(command, window, cx);
    }
}
