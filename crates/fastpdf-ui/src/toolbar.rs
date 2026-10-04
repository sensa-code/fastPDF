//! The single toolbar row (spec §20: minimal chrome). Buttons dispatch the
//! same [`ReaderCommand`]s as the keymap; their tooltips name the command
//! and its shortcut from the central keymap, in the UI language.

use fastpdf_core::keymap::{ReaderCommand, keystrokes_for};
use gpui::{
    AnyView, App, AppContext, ClickEvent, Context, Div, InteractiveElement, IntoElement,
    ParentElement, Render, SharedString, Stateful, StatefulInteractiveElement, Styled, Window, div,
    px,
};

use crate::i18n::Strings;
use crate::reader::ReaderView;
use crate::theme::Theme;

/// Toolbar height in logical pixels.
pub(crate) const HEIGHT: f32 = 40.0;

pub(crate) fn render(
    view: &ReaderView,
    cx: &mut Context<'_, ReaderView>,
) -> impl IntoElement + use<> {
    let theme = view.theme;
    let strings = view.strings();
    let font = view.language().ui_font();
    let session = view.session();
    let has_doc = session.is_some();
    let (page_text, zoom_text) = match (view.page_indicator(), session) {
        (Some((page, count)), Some(s)) => (
            format!("{} / {}", page.display_number(), count),
            format!("{}%", s.zoom().percent()),
        ),
        _ => ("- / -".to_string(), "-".to_string()),
    };
    let title = match &view.doc {
        crate::reader::DocState::Open(open) => open.name.clone(),
        _ => SharedString::default(),
    };
    let tools = Tools {
        theme,
        strings,
        font,
    };

    use ReaderCommand as C;
    div()
        .flex()
        .flex_row()
        .flex_none()
        .items_center()
        .gap_1()
        .h(px(HEIGHT))
        .px_2()
        .bg(theme.toolbar_bg)
        .border_b_1()
        .border_color(theme.toolbar_border)
        .text_size(px(14.0))
        .child(tools.toggle(
            "sidebar",
            strings.sidebar,
            strings.tip_sidebar,
            C::ToggleSidebar,
            view.sidebar.open,
            cx,
        ))
        .child(tools.button(
            "open",
            strings.open,
            strings.tip_open,
            C::OpenFile,
            true,
            cx,
        ))
        .child(separator(&theme))
        .child(tools.button(
            "prev",
            "\u{2039}",
            strings.tip_previous_page,
            C::PreviousPage,
            has_doc,
            cx,
        ))
        .child(label(page_text, 76.0))
        .child(tools.button(
            "next",
            "\u{203a}",
            strings.tip_next_page,
            C::NextPage,
            has_doc,
            cx,
        ))
        .child(separator(&theme))
        .child(tools.button(
            "zoom-out",
            "\u{2212}",
            strings.tip_zoom_out,
            C::ZoomOut,
            has_doc,
            cx,
        ))
        .child(label(zoom_text, 52.0))
        .child(tools.button("zoom-in", "+", strings.tip_zoom_in, C::ZoomIn, has_doc, cx))
        .child(tools.button(
            "fit-width",
            strings.fit_width,
            strings.tip_fit_width,
            C::FitWidth,
            has_doc,
            cx,
        ))
        .child(tools.button(
            "fit-page",
            strings.fit_page,
            strings.tip_fit_page,
            C::FitPage,
            has_doc,
            cx,
        ))
        .child(separator(&theme))
        .child(tools.button(
            "rotate-ccw",
            "\u{21ba}",
            strings.tip_rotate_left,
            C::RotateCounterClockwise,
            has_doc,
            cx,
        ))
        .child(tools.button(
            "rotate-cw",
            "\u{21bb}",
            strings.tip_rotate_right,
            C::RotateClockwise,
            has_doc,
            cx,
        ))
        .child(separator(&theme))
        .child(tools.toggle(
            "find",
            strings.find,
            strings.tip_find,
            C::Find,
            view.find.open,
            cx,
        ))
        // Night mode (inverted pages) is a setting, so it works without a
        // document too.
        .child(tools.toggle(
            "night",
            strings.night,
            strings.tip_night,
            C::ToggleNightMode,
            view.settings.night_mode,
            cx,
        ))
        .child(div().flex_1())
        .child(
            div()
                .text_color(theme.text_muted)
                .text_size(px(13.0))
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .max_w(px(360.0))
                .child(title),
        )
        .child(
            styled_button("settings", "\u{2699}", true, view.settings_open, &theme)
                .text_size(px(16.0))
                .tooltip(tooltip(strings.tip_settings.into(), theme, font))
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.toggle_settings_panel(cx);
                })),
        )
}

fn separator(theme: &Theme) -> Div {
    div()
        .flex_none()
        .w(px(1.0))
        .h(px(20.0))
        .mx_1()
        .bg(theme.toolbar_border)
}

fn label(text: String, min_width: f32) -> Div {
    div()
        .flex_none()
        .min_w(px(min_width))
        .flex()
        .justify_center()
        .whitespace_nowrap()
        .child(text)
}

/// A small flat button; `selected` shows it pressed (toggles, tabs). It
/// never shrinks: in a narrow window the document title gives way first.
pub(crate) fn styled_button(
    id: impl Into<SharedString>,
    label: impl Into<SharedString>,
    enabled: bool,
    selected: bool,
    theme: &Theme,
) -> Stateful<Div> {
    let base = div()
        .id(id.into())
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .h(px(28.0))
        .min_w(px(28.0))
        .px_2()
        .rounded(px(4.0))
        .whitespace_nowrap()
        .child(label.into());
    if !enabled {
        return base.text_color(theme.text_muted).opacity(0.5);
    }
    let hover = theme.button_hover;
    let active = theme.button_active;
    let base = if selected {
        base.bg(theme.button_active)
    } else {
        base
    };
    base.cursor_pointer()
        .hover(move |style| style.bg(hover))
        .active(move |style| style.bg(active))
}

/// What every toolbar button needs to look and read right.
#[derive(Clone, Copy)]
struct Tools {
    theme: Theme,
    strings: &'static Strings,
    font: &'static str,
}

impl Tools {
    /// The tooltip for a command: what it does and its first shortcut.
    fn tip(&self, tip: &str, command: ReaderCommand) -> SharedString {
        self.strings
            .with_shortcut(tip, keystrokes_for(command).next())
            .into()
    }

    /// A button that runs `command`, like its keyboard shortcut.
    fn button(
        &self,
        id: &'static str,
        label: &'static str,
        tip: &'static str,
        command: ReaderCommand,
        enabled: bool,
        cx: &mut Context<'_, ReaderView>,
    ) -> Stateful<Div> {
        let base = styled_button(id, label, enabled, false, &self.theme).tooltip(tooltip(
            self.tip(tip, command),
            self.theme,
            self.font,
        ));
        if !enabled {
            return base;
        }
        base.on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
            this.run_command(command, window, cx);
        }))
    }

    /// A button showing whether its panel or mode (sidebar, find bar, night
    /// mode) is on.
    fn toggle(
        &self,
        id: &'static str,
        label: &'static str,
        tip: &'static str,
        command: ReaderCommand,
        on: bool,
        cx: &mut Context<'_, ReaderView>,
    ) -> Stateful<Div> {
        styled_button(id, label, true, on, &self.theme)
            .tooltip(tooltip(self.tip(tip, command), self.theme, self.font))
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                this.run_command(command, window, cx);
            }))
    }
}

/// A one-line tooltip in the theme's colors and the UI language's font.
pub(crate) struct Tooltip {
    text: SharedString,
    theme: Theme,
    font: &'static str,
}

impl Render for Tooltip {
    fn render(&mut self, _: &mut Window, _: &mut Context<'_, Self>) -> impl IntoElement {
        div()
            .font_family(self.font)
            .text_size(px(12.0))
            .px_2()
            .py_1()
            .rounded(px(4.0))
            .bg(self.theme.toolbar_bg)
            .border_1()
            .border_color(self.theme.toolbar_border)
            .text_color(self.theme.text)
            .whitespace_nowrap()
            .child(self.text.clone())
    }
}

/// A tooltip builder for [`StatefulInteractiveElement::tooltip`].
pub(crate) fn tooltip(
    text: SharedString,
    theme: Theme,
    font: &'static str,
) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    move |_, cx| {
        let text = text.clone();
        cx.new(|_| Tooltip { text, theme, font }).into()
    }
}
