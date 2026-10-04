//! The single toolbar row (spec §20: minimal chrome). Buttons dispatch the
//! same [`ReaderCommand`]s as the keymap.

use fastpdf_core::keymap::ReaderCommand;
use gpui::{
    ClickEvent, Context, Div, InteractiveElement, IntoElement, ParentElement, SharedString,
    Stateful, StatefulInteractiveElement, Styled, div, px,
};

use crate::reader::ReaderView;
use crate::theme::Theme;

/// Toolbar height in logical pixels.
pub(crate) const HEIGHT: f32 = 40.0;

pub(crate) fn render(
    view: &ReaderView,
    cx: &mut Context<'_, ReaderView>,
) -> impl IntoElement + use<> {
    let theme = view.theme;
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

    use ReaderCommand as C;
    let mut bar = div()
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
        .text_size(px(14.0));
    bar = bar
        .child(button("open", "Open", C::OpenFile, true, &theme, cx))
        .child(separator(&theme))
        .child(button(
            "prev",
            "\u{2039}",
            C::PreviousPage,
            has_doc,
            &theme,
            cx,
        ))
        .child(label(page_text, 76.0))
        .child(button("next", "\u{203a}", C::NextPage, has_doc, &theme, cx))
        .child(separator(&theme))
        .child(button(
            "zoom-out",
            "\u{2212}",
            C::ZoomOut,
            has_doc,
            &theme,
            cx,
        ))
        .child(label(zoom_text, 52.0))
        .child(button("zoom-in", "+", C::ZoomIn, has_doc, &theme, cx))
        .child(button(
            "fit-width",
            "Fit width",
            C::FitWidth,
            has_doc,
            &theme,
            cx,
        ))
        .child(button(
            "fit-page",
            "Fit page",
            C::FitPage,
            has_doc,
            &theme,
            cx,
        ))
        .child(separator(&theme))
        .child(button(
            "rotate-ccw",
            "\u{21ba}",
            C::RotateCounterClockwise,
            has_doc,
            &theme,
            cx,
        ))
        .child(button(
            "rotate-cw",
            "\u{21bb}",
            C::RotateClockwise,
            has_doc,
            &theme,
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
    bar
}

fn separator(theme: &Theme) -> Div {
    div().w(px(1.0)).h(px(20.0)).mx_1().bg(theme.toolbar_border)
}

fn label(text: String, min_width: f32) -> Div {
    div()
        .min_w(px(min_width))
        .flex()
        .justify_center()
        .whitespace_nowrap()
        .child(text)
}

/// A toolbar button that runs `command`, like its keyboard shortcut.
fn button(
    id: &'static str,
    label: &'static str,
    command: ReaderCommand,
    enabled: bool,
    theme: &Theme,
    cx: &mut Context<'_, ReaderView>,
) -> Stateful<Div> {
    let base = div()
        .id(SharedString::new_static(id))
        .flex()
        .items_center()
        .justify_center()
        .h(px(28.0))
        .min_w(px(28.0))
        .px_2()
        .rounded(px(4.0))
        .whitespace_nowrap()
        .child(label);
    if !enabled {
        return base.text_color(theme.text_muted).opacity(0.5);
    }
    let hover = theme.button_hover;
    let active = theme.button_active;
    base.cursor_pointer()
        .hover(move |style| style.bg(hover))
        .active(move |style| style.bg(active))
        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
            this.run_command(command, window, cx);
        }))
}
