//! Appearance, night mode, default zoom, language and smooth scrolling:
//! the commands that change them and the small settings panel (toolbar gear
//! button). Every change is saved right away (`crate::settings`).

use gpui::{
    ClickEvent, Context, Div, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, div, px,
};

use crate::i18n::{Language, Strings};
use crate::reader::{ReaderView, apply_default_zoom};
use crate::settings::{Appearance, DefaultZoom, LanguageChoice};
use crate::theme::Theme;
use crate::toolbar::styled_button;

/// Panel width and label column, in logical pixels.
const WIDTH: f32 = 470.0;
const LABEL_WIDTH: f32 = 120.0;

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

    /// UI language: the Windows UI language, or a fixed one.
    pub(crate) fn set_language(&mut self, choice: LanguageChoice, cx: &mut Context<'_, Self>) {
        self.settings.language = choice;
        self.apply_language(cx);
        self.save_settings(cx);
        cx.notify();
    }

    pub(crate) fn set_smooth_scrolling(&mut self, on: bool, cx: &mut Context<'_, Self>) {
        self.settings.smooth_scrolling = on;
        if !on {
            self.smooth_scroll.stop();
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
        let strings = self.strings();
        let settings = &self.settings;

        let mut appearance = row(strings.appearance, &theme);
        for choice in Appearance::ALL {
            appearance = appearance.child(
                styled_button(
                    SharedString::from(format!("appearance-{}", choice.name())),
                    appearance_label(choice, strings),
                    true,
                    settings.appearance == choice,
                    &theme,
                )
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.set_appearance(choice, cx);
                })),
            );
        }

        let night = switch_row(
            "night",
            strings.night_mode,
            settings.night_mode,
            strings,
            &theme,
            cx,
            |this, on, cx| this.set_night_mode(on, cx),
        );

        let mut zoom = row(strings.default_zoom, &theme);
        for choice in DefaultZoom::ALL {
            zoom = zoom.child(
                styled_button(
                    SharedString::from(format!("default-zoom-{}", choice.name())),
                    zoom_label(choice, strings),
                    true,
                    settings.default_zoom == choice,
                    &theme,
                )
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.set_default_zoom(choice, cx);
                })),
            );
        }

        // Languages are named in their own language, so each reader finds
        // theirs whatever the current one is.
        let mut language = row(strings.language_label, &theme);
        let choices = std::iter::once((LanguageChoice::System, strings.follow_system)).chain(
            Language::ALL
                .into_iter()
                .map(|l| (LanguageChoice::Fixed(l), l.native_name())),
        );
        for (choice, label) in choices {
            language = language.child(
                styled_button(
                    SharedString::from(format!("language-{}", choice.name())),
                    label,
                    true,
                    settings.language == choice,
                    &theme,
                )
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.set_language(choice, cx);
                })),
            );
        }

        let smooth = switch_row(
            "smooth",
            strings.smooth_scrolling,
            settings.smooth_scrolling,
            strings,
            &theme,
            cx,
            |this, on, cx| this.set_smooth_scrolling(on, cx),
        );

        let saved = match self.settings_path() {
            Some(path) => strings.saved_in(path),
            None => strings.not_saved.into(),
        };
        let body = div()
            .flex()
            .flex_col()
            .gap_2()
            .child(div().text_size(px(15.0)).child(strings.settings))
            .child(appearance)
            .child(night)
            .child(zoom)
            .child(language)
            .child(smooth)
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
                styled_button("settings-close", strings.close, true, false, &theme).on_click(
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

fn appearance_label(appearance: Appearance, strings: &'static Strings) -> &'static str {
    match appearance {
        Appearance::System => strings.follow_system,
        Appearance::Light => strings.light,
        Appearance::Dark => strings.dark,
    }
}

fn zoom_label(zoom: DefaultZoom, strings: &'static Strings) -> &'static str {
    match zoom {
        DefaultZoom::FitWidth => strings.fit_width,
        DefaultZoom::FitPage => strings.fit_page,
        DefaultZoom::ActualSize => strings.actual_size,
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

/// A labeled Off / On pair.
fn switch_row(
    id: &'static str,
    label: &'static str,
    on: bool,
    strings: &'static Strings,
    theme: &Theme,
    cx: &mut Context<'_, ReaderView>,
    set: fn(&mut ReaderView, bool, &mut Context<'_, ReaderView>),
) -> Div {
    let mut row = row(label, theme);
    for (suffix, text, value) in [("off", strings.off, false), ("on", strings.on, true)] {
        row = row.child(
            styled_button(
                SharedString::from(format!("{id}-{suffix}")),
                text,
                true,
                on == value,
                theme,
            )
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                set(this, value, cx);
            })),
        );
    }
    row
}
