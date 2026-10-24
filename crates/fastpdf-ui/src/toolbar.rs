//! The single toolbar row (spec §20: minimal chrome). Buttons dispatch the
//! same [`ReaderCommand`]s as the keymap; their tooltips name the command
//! and its shortcut from the central keymap, in the UI language.

use fastpdf_core::keymap::{ReaderCommand, keystrokes_for};
use gpui::{
    AnyElement, AnyView, App, AppContext, ClickEvent, Context, Div, ElementId, InteractiveElement,
    IntoElement, MouseDownEvent, ParentElement, Render, SharedString, Stateful,
    StatefulInteractiveElement, Styled, Window, anchored, deferred, div, prelude::FluentBuilder,
    px,
};

use crate::i18n::Strings;
use crate::reader::{Menu, ReaderView};
use crate::settings::PageLayout;
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
    // While a prompt card blocks the window, only what answers or leaves it
    // stays clickable; nothing may open behind it.
    let blocked = view.active_prompt().is_some();
    let tools = Tools {
        theme,
        strings,
        font,
        blocked,
    };

    use ReaderCommand as C;
    // In a narrow window the end of this row (rotation, the title) is cut
    // off, never the find, night, edit and settings buttons after it.
    let row = div()
        .flex()
        .flex_row()
        .flex_1()
        .min_w_0()
        .overflow_hidden()
        .items_center()
        .gap_1()
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
            "back",
            "\u{2190}",
            strings.tip_back(),
            C::NavigateBack,
            session.is_some_and(|s| s.can_go_back()),
            cx,
        ))
        .child(tools.button(
            "forward",
            "\u{2192}",
            strings.tip_forward(),
            C::NavigateForward,
            session.is_some_and(|s| s.can_go_forward()),
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
        .child(label(page_text, 56.0))
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
        .child(label(zoom_text, 40.0))
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
        .child(layout_control(view, tools, cx))
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
        );
    div()
        .flex()
        .flex_row()
        .flex_none()
        .items_center()
        .gap_1()
        .h(px(HEIGHT))
        .px_1()
        .bg(theme.toolbar_bg)
        .border_b_1()
        .border_color(theme.toolbar_border)
        .text_size(px(14.0))
        .child(row)
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
        .child(edit_control(view, tools, cx))
        .child(
            styled_button("settings", "\u{2699}", !blocked, view.settings_open, &theme)
                .text_size(px(16.0))
                .tooltip(tooltip(strings.tip_settings.into(), theme, font))
                .when(!blocked, |button| {
                    button.on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.toggle_settings_panel(cx);
                    }))
                }),
        )
}

/// The page layout button and, while it is open, its menu. The layout is a
/// setting, so it can be chosen without a document too.
fn layout_control(view: &ReaderView, tools: Tools, cx: &mut Context<'_, ReaderView>) -> Div {
    let strings = tools.strings;
    let current = view.settings.page_layout;
    let short = PageLayout::ALL
        .iter()
        .position(|l| *l == current)
        .and_then(|i| strings.layout_button.get(i))
        .copied()
        .unwrap_or_default();
    let label = format!("{short} \u{25be}");
    menu_control(
        Menu::Layout,
        label,
        strings.tip_layout,
        view,
        tools,
        cx,
        |cx| layout_items(view, tools, cx),
    )
}

/// The edit menu: writing new files from the open one (ADR 0013).
fn edit_control(view: &ReaderView, tools: Tools, cx: &mut Context<'_, ReaderView>) -> Div {
    let strings = tools.strings;
    let label = format!("{} \u{25be}", strings.edit);
    menu_control(Menu::Edit, label, strings.tip_edit, view, tools, cx, |cx| {
        // A document opened with a password is never written (ADR 0014).
        let writable = view.session().is_some() && !view.document_protected();
        vec![
            menu_item(
                "edit-copy",
                strings.save_copy,
                false,
                writable,
                ReaderCommand::SaveCopy,
                tools,
                cx,
            )
            .into_any_element(),
            menu_item(
                "edit-pages",
                strings.page_tools,
                view.tools.open,
                writable,
                ReaderCommand::PageTools,
                tools,
                cx,
            )
            .into_any_element(),
            menu_item(
                "edit-stamps",
                strings.stamp_menu,
                view.stamps.open,
                writable,
                ReaderCommand::StampTool,
                tools,
                cx,
            )
            .into_any_element(),
            menu_item(
                "edit-text",
                strings.edit_text_menu,
                view.text_edits.open,
                writable,
                ReaderCommand::EditText,
                tools,
                cx,
            )
            .into_any_element(),
            menu_item(
                "edit-sign",
                strings.sign_menu,
                view.signing.open,
                writable,
                ReaderCommand::Sign,
                tools,
                cx,
            )
            .into_any_element(),
        ]
    })
}

/// A drop-down menu's button and, while it is open, the menu below it.
fn menu_control(
    menu: Menu,
    label: String,
    tip: &'static str,
    view: &ReaderView,
    tools: Tools,
    cx: &mut Context<'_, ReaderView>,
    items: impl FnOnce(&mut Context<'_, ReaderView>) -> Vec<AnyElement>,
) -> Div {
    let theme = tools.theme;
    let open = view.menus.is_open(menu) && !tools.blocked;
    let id = match menu {
        Menu::Layout => "layout",
        Menu::Edit => "edit",
    };
    let button = styled_button(id, label, !tools.blocked, open, &theme)
        .tooltip(tooltip(tip.into(), theme, tools.font))
        .when(!tools.blocked, |button| {
            button.on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                this.toggle_menu(menu, event, cx);
            }))
        });
    let control = div().relative().flex_none().child(button);
    if !open {
        return control;
    }
    let panel = div()
        .id((id, 1usize))
        .occlude()
        .flex()
        .flex_col()
        .py_1()
        .min_w(px(180.0))
        .rounded(px(6.0))
        .bg(theme.toolbar_bg)
        .border_1()
        .border_color(theme.toolbar_border)
        .shadow_md()
        .font_family(tools.font)
        .text_color(theme.text)
        .text_size(px(14.0))
        .on_mouse_down_out(cx.listener(move |this, event: &MouseDownEvent, _, cx| {
            this.dismiss_menu(menu, event, cx);
        }))
        .children(items(cx));
    control.child(div().absolute().top(px(32.0)).left(px(0.0)).child(
        deferred(anchored().snap_to_window_with_margin(px(8.0)).child(panel)).with_priority(1),
    ))
}

fn layout_name(strings: &Strings, layout: PageLayout) -> &'static str {
    match layout {
        PageLayout::SinglePage => strings.layout_single_page,
        PageLayout::Continuous => strings.layout_continuous,
        PageLayout::TwoPage => strings.layout_two_page,
        PageLayout::TwoPageContinuous => strings.layout_two_page_continuous,
    }
}

fn layout_command(layout: PageLayout) -> ReaderCommand {
    match layout {
        PageLayout::SinglePage => ReaderCommand::LayoutSinglePage,
        PageLayout::Continuous => ReaderCommand::LayoutContinuous,
        PageLayout::TwoPage => ReaderCommand::LayoutTwoPage,
        PageLayout::TwoPageContinuous => ReaderCommand::LayoutTwoPageContinuous,
    }
}

/// The layout menu's rows: the four layouts, then the cover page option.
fn layout_items(
    view: &ReaderView,
    tools: Tools,
    cx: &mut Context<'_, ReaderView>,
) -> Vec<AnyElement> {
    let theme = tools.theme;
    let strings = tools.strings;
    let current = view.settings.page_layout;
    let mut items: Vec<AnyElement> = PageLayout::ALL
        .into_iter()
        .enumerate()
        .map(|(i, layout)| {
            menu_item(
                ("layout-item", i),
                layout_name(strings, layout),
                layout == current,
                true,
                layout_command(layout),
                tools,
                cx,
            )
            .into_any_element()
        })
        .collect();
    items.push(
        div()
            .h(px(1.0))
            .mx_2()
            .my_1()
            .bg(theme.toolbar_border)
            .into_any_element(),
    );
    items.push(
        menu_item(
            "layout-cover",
            strings.cover_page,
            view.settings.cover_page,
            current.is_two_page(),
            ReaderCommand::ToggleCoverPage,
            tools,
            cx,
        )
        .into_any_element(),
    );
    items
}

/// A menu row with a check mark column; a disabled row is shown muted.
fn menu_item(
    id: impl Into<ElementId>,
    label: &'static str,
    checked: bool,
    enabled: bool,
    command: ReaderCommand,
    tools: Tools,
    cx: &mut Context<'_, ReaderView>,
) -> Stateful<Div> {
    let theme = tools.theme;
    let row = div()
        .id(id)
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .h(px(28.0))
        .px_3()
        .whitespace_nowrap()
        .child(
            div()
                .flex_none()
                .w(px(14.0))
                .child(if checked { "\u{2713}" } else { "" }),
        )
        .child(label);
    if !enabled {
        return row.text_color(theme.text_muted);
    }
    let hover = theme.button_hover;
    row.cursor_pointer()
        .hover(move |style| style.bg(hover))
        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
            this.run_command(command, window, cx);
        }))
}

fn separator(theme: &Theme) -> Div {
    div()
        .flex_none()
        .w(px(1.0))
        .h(px(20.0))
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
        .px(px(6.0))
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
    /// A prompt card blocks the window (`ReaderView::active_prompt`).
    blocked: bool,
}

impl Tools {
    /// The tooltip for a command: what it does and its first shortcut.
    fn tip(&self, tip: &str, command: ReaderCommand) -> SharedString {
        self.strings
            .with_shortcut(tip, keystrokes_for(command).next())
            .into()
    }

    /// Whether `command` may run now: not while a prompt blocks the window,
    /// unless it answers or leaves the prompt.
    fn allows(&self, command: ReaderCommand) -> bool {
        !self.blocked || command.allowed_during_prompt()
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
        let enabled = enabled && self.allows(command);
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
        let enabled = self.allows(command);
        styled_button(id, label, enabled, on, &self.theme)
            .tooltip(tooltip(self.tip(tip, command), self.theme, self.font))
            .when(enabled, |button| {
                button.on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.run_command(command, window, cx);
                }))
            })
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
