//! The single toolbar row (spec §20: minimal chrome). Buttons dispatch the
//! same [`ReaderCommand`]s as the keymap; their tooltips name the command
//! and its shortcut from the central keymap, in the UI language. What does
//! not fit a narrow window moves into a "⋯" menu (`overflow`).

mod controls;
mod overflow;

use fastpdf_core::keymap::{ReaderCommand, keystrokes_for};
use gpui::{
    AnyElement, AnyView, App, AppContext, ClickEvent, Context, Div, ElementId, InteractiveElement,
    IntoElement, MouseDownEvent, ParentElement, Pixels, Render, SharedString, Stateful,
    StatefulInteractiveElement, Styled, Window, anchored, deferred, div, prelude::FluentBuilder,
    px,
};

use crate::i18n::{Strings, display_keystroke};
use crate::reader::{Menu, ReaderView};
use crate::settings::PageLayout;
use crate::theme::Theme;
use overflow::Slot;
pub(crate) use overflow::{Group, Hidden, ToolbarFit};

/// Toolbar height in logical pixels.
pub(crate) const HEIGHT: f32 = 40.0;
/// Space between the toolbar's controls: the buttons carry their own
/// padding.
const GAP: f32 = 2.0;
/// Space at both ends of the toolbar.
const BAR_PADDING: f32 = 4.0;

pub(crate) fn render(
    view: &ReaderView,
    window: &Window,
    cx: &mut Context<'_, ReaderView>,
) -> impl IntoElement + use<> {
    let theme = view.theme;
    let strings = view.strings();
    let font = view.language().ui_font();
    // While a prompt card blocks the window, only what answers or leaves it
    // stays clickable; nothing may open behind it.
    let blocked = view.active_prompt().is_some();
    let tools = Tools {
        theme,
        strings,
        font,
        blocked,
        menu_height: window.viewport_size().height - px(HEIGHT + 16.0),
    };
    // What does not fit the window waits in the "⋯" menu (`overflow`).
    let hidden = view.toolbar_fit.hidden();
    let editing = view.go_to.editing;
    let (page, page_min) = crate::go_to_page::page_control(view, blocked, window, cx);
    let mut page = Some(page);
    let (slots, len) = overflow::slots(hidden, editing);
    let mut bar = div()
        .flex()
        .flex_row()
        .flex_none()
        .items_center()
        .overflow_hidden()
        .gap(px(GAP))
        .h(px(HEIGHT))
        .px(px(BAR_PADDING))
        .bg(theme.toolbar_bg)
        .border_b_1()
        .border_color(theme.toolbar_border)
        .text_size(px(14.0));
    for slot in &slots[..len] {
        bar = match *slot {
            Slot::Group(group) => bar.child(controls::group_control(group, view, tools, cx)),
            Slot::Separator => bar.child(separator(&theme)),
            Slot::Page => bar.children(page.take()),
            Slot::Title => bar.child(controls::title(view, &theme)),
            Slot::More => bar.child(menu_control(
                Menu::Overflow,
                "\u{22ef}".to_owned(),
                strings.tip_more(),
                view,
                tools,
                cx,
                |cx| controls::menu_rows(view, tools, hidden, cx),
            )),
            Slot::Settings => bar.child(
                styled_button("settings", "\u{2699}", !blocked, view.settings_open, &theme)
                    .text_size(px(16.0))
                    .tooltip(tooltip(strings.tip_settings.into(), theme, font))
                    .when(!blocked, |button| {
                        button.on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.toggle_settings_panel(window, cx);
                        }))
                    }),
            ),
        };
    }
    bar.on_children_prepainted(overflow::measure(
        cx.entity(),
        hidden,
        editing,
        page_min.map(f32::from),
        view.language(),
    ))
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
    let label = format!("{} \u{25be}", tools.strings.edit);
    menu_control(
        Menu::Edit,
        label,
        tools.strings.tip_edit,
        view,
        tools,
        cx,
        |cx| edit_items(view, tools, cx),
    )
}

/// The edit menu's rows.
fn edit_items(
    view: &ReaderView,
    tools: Tools,
    cx: &mut Context<'_, ReaderView>,
) -> Vec<AnyElement> {
    let strings = tools.strings;
    // A document opened with a password is never written (ADR 0014).
    let writable = view.session().is_some() && !view.document_protected();
    [
        (
            "edit-copy",
            strings.save_copy,
            false,
            ReaderCommand::SaveCopy,
        ),
        (
            "edit-pages",
            strings.page_tools,
            view.tools.open,
            ReaderCommand::PageTools,
        ),
        (
            "edit-stamps",
            strings.stamp_menu,
            view.stamps.open,
            ReaderCommand::StampTool,
        ),
        (
            "edit-text",
            strings.edit_text_menu,
            view.text_edits.open,
            ReaderCommand::EditText,
        ),
        (
            "edit-sign",
            strings.sign_menu,
            view.signing.open,
            ReaderCommand::Sign,
        ),
    ]
    .into_iter()
    .map(|(id, label, checked, command)| {
        menu_item(id, label, checked, writable, command, tools, cx).into_any_element()
    })
    .collect()
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
        Menu::Overflow => "more",
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
        .max_h(tools.menu_height)
        .overflow_y_scroll()
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
    items.push(menu_divider(&theme));
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

/// A thin line between groups of menu rows.
fn menu_divider(theme: &Theme) -> AnyElement {
    div()
        .h(px(1.0))
        .mx_2()
        .my_1()
        .bg(theme.toolbar_border)
        .into_any_element()
}

/// A menu row with a check mark column and the command's shortcut; a
/// disabled row is shown muted. Choosing it closes the menu.
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
        .child(label)
        .when_some(keystrokes_for(command).next(), |row, keys| {
            row.child(div().flex_1().min_w(px(16.0))).child(
                div()
                    .flex_none()
                    .text_size(px(12.0))
                    .text_color(theme.text_muted)
                    .child(display_keystroke(keys)),
            )
        });
    if !(enabled && tools.allows(command)) {
        return row.text_color(theme.text_muted);
    }
    let hover = theme.button_hover;
    row.cursor_pointer()
        .hover(move |style| style.bg(hover))
        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
            this.run_menu_item(command, window, cx);
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
    /// The tallest a drop-down menu may get; a longer one scrolls.
    menu_height: Pixels,
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
