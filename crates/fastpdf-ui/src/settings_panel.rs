//! Appearance, night mode and default zoom: the commands that change them
//! and the small settings panel (toolbar gear button). Every change is
//! saved right away (`crate::settings`).

use gpui::{
    ClickEvent, Context, Div, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, div, px,
};

use crate::reader::{ReaderView, apply_default_zoom};
use crate::settings::{Appearance, DefaultZoom};
use crate::theme::Theme;
use crate::toolbar::styled_button;

/// Panel width and label column, in logical pixels.
const WIDTH: f32 = 430.0;
const LABEL_WIDTH: f32 = 110.0;

impl ReaderView {
    pub(crate) fn toggle_settings_panel(&mut self, cx: &mut Context<'_, Self>) {
        self.settings_open = !self.settings_open;
        cx.notify();
    }

    /// Chrome colors: follow Windows, or always light / dark.
    pub(crate) fn set_appearance(&mut self, appearance: Appearance, cx: &mut Context<'_, Self>) {
        self.settings.appearance = appearance;
        crate::reader::apply_window_appearance(appearance, cx);
        self.save_settings(cx);
        cx.notify();
    }

    pub(crate) fn toggle_night_mode(&mut self, cx: &mut Context<'_, Self>) {
        self.set_night_mode(!self.settings.night_mode, cx);
    }

    /// Night mode inverts the pages (tiles and thumbnails re-render in the
    /// other color mode; the previous mode's tiles stay cached for a quick
    /// switch back while the budget allows).
    pub(crate) fn set_night_mode(&mut self, on: bool, cx: &mut Context<'_, Self>) {
        self.settings.night_mode = on;
        let mode = self.color_mode();
        if let Some(session) = self.session_mut() {
            session.set_color_mode(mode);
        }
        self.save_settings(cx);
        cx.notify();
    }

    /// The zoom new documents open with; applied to the open one too, so
    /// the choice shows at once.
    pub(crate) fn set_default_zoom(&mut self, zoom: DefaultZoom, cx: &mut Context<'_, Self>) {
        self.settings.default_zoom = zoom;
        if let Some(session) = self.session_mut() {
            apply_default_zoom(session, zoom);
        }
        self.save_settings(cx);
        cx.notify();
    }

    /// The panel, floating at the top right of the document area.
    pub(crate) fn render_settings_panel(
        &self,
        cx: &mut Context<'_, Self>,
    ) -> impl IntoElement + use<> {
        let theme = self.theme;
        let settings = &self.settings;

        let mut appearance = row("Appearance", &theme);
        for choice in Appearance::ALL {
            appearance = appearance.child(
                styled_button(
                    SharedString::from(format!("appearance-{}", choice.name())),
                    choice.label(),
                    true,
                    settings.appearance == choice,
                    &theme,
                )
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.set_appearance(choice, cx);
                })),
            );
        }

        let mut night = row("Night mode", &theme);
        for (id, label, on) in [("night-off", "Off", false), ("night-on", "On", true)] {
            night = night.child(
                styled_button(id, label, true, settings.night_mode == on, &theme).on_click(
                    cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.set_night_mode(on, cx);
                    }),
                ),
            );
        }

        let mut zoom = row("Default zoom", &theme);
        for choice in DefaultZoom::ALL {
            zoom = zoom.child(
                styled_button(
                    SharedString::from(format!("default-zoom-{}", choice.name())),
                    choice.label(),
                    true,
                    settings.default_zoom == choice,
                    &theme,
                )
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.set_default_zoom(choice, cx);
                })),
            );
        }

        let saved = match self.settings_path() {
            Some(path) => format!("Saved automatically in {}", path.display()),
            None => "Not saved: FASTPDF_SETTINGS_FILE is empty.".into(),
        };
        let body = div()
            .flex()
            .flex_col()
            .gap_2()
            .child(div().text_size(px(15.0)).child("Settings"))
            .child(appearance)
            .child(night)
            .child(zoom)
            .child(
                div()
                    .text_size(px(12.0))
                    .text_color(theme.text_muted)
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .child(saved),
            )
            .child(div().flex().flex_row().justify_end().child(
                styled_button("settings-close", "Close", true, false, &theme).on_click(
                    cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.settings_open = false;
                        cx.notify();
                    }),
                ),
            ));

        div()
            .id("settings-panel")
            .absolute()
            .top(px(8.0))
            .right(px(20.0))
            .occlude()
            .w(px(WIDTH))
            .p_3()
            .rounded(px(6.0))
            .bg(theme.toolbar_bg)
            .border_1()
            .border_color(theme.toolbar_border)
            .text_size(px(13.0))
            .child(body)
    }
}

/// A labeled row of choices.
fn row(label: &'static str, theme: &Theme) -> Div {
    div().flex().flex_row().items_center().gap_1().child(
        div()
            .w(px(LABEL_WIDTH))
            .flex_none()
            .text_color(theme.text_muted)
            .child(label),
    )
}
