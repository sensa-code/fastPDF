//! Every color of the UI, for the light and dark appearance (spec §33 dark
//! mode). Deliberately tiny: minimal chrome, no theming engine (spec §20).
//!
//! * Chrome (toolbar, sidebar, panels, empty state, overlay) follows the
//!   appearance: the Windows app mode by default, or the user's choice
//!   ([`Appearance`]). The reader view resolves it every frame, so a system
//!   switch (`WM_SETTINGCHANGE` "ImmersiveColorSet", reported by GPUI as a
//!   window appearance change) shows on the next frame.
//! * Page colors follow the pages, not the chrome: unrendered paper and
//!   highlights drawn over page content depend on the color mode (night
//!   mode inverts pages in either appearance).

use fastpdf_engine_api::{ColorMode, Rgba8};
use gpui::{App, Global, Rgba, WindowAppearance};

use crate::settings::Appearance;

/// Fixed-width font for the development overlay. (The UI font depends on
/// the language: `crate::i18n::Language::ui_font`.)
pub(crate) const MONO_FONT: &str = "Consolas";

/// Chrome colors.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Theme {
    pub toolbar_bg: Rgba,
    pub toolbar_border: Rgba,
    pub text: Rgba,
    pub text_muted: Rgba,
    pub button_hover: Rgba,
    pub button_active: Rgba,
    pub canvas_bg: Rgba,
    pub page_border: Rgba,
    pub error_text: Rgba,
    pub overlay_bg: Rgba,
    pub overlay_text: Rgba,
    pub drop_highlight: Rgba,
    pub sidebar_bg: Rgba,
    pub input_bg: Rgba,
    /// Current page in the thumbnail list, focused controls, text caret.
    pub accent: Rgba,
}

impl Theme {
    /// The chrome for the appearance setting; `System` follows the window's
    /// appearance (the Windows app mode).
    pub(crate) fn resolve(setting: Appearance, system: WindowAppearance) -> Self {
        match setting {
            Appearance::Light => Self::LIGHT,
            Appearance::Dark => Self::DARK,
            Appearance::System => match system {
                WindowAppearance::Dark | WindowAppearance::VibrantDark => Self::DARK,
                WindowAppearance::Light | WindowAppearance::VibrantLight => Self::LIGHT,
            },
        }
    }

    pub(crate) const LIGHT: Self = Self {
        toolbar_bg: rgb_const(0xf3f3f3),
        toolbar_border: rgb_const(0xdcdcdc),
        text: rgb_const(0x1b1b1b),
        text_muted: rgb_const(0x616161),
        button_hover: rgb_const(0xe2e2e2),
        button_active: rgb_const(0xd0d0d0),
        canvas_bg: rgb_const(0xe6e6e6),
        page_border: rgb_const(0xb8b8b8),
        error_text: rgb_const(0xb00020),
        overlay_bg: rgba_const(0xf9f9f9e8),
        overlay_text: rgb_const(0x0b5c1f),
        drop_highlight: rgba_const(0x0067c030),
        sidebar_bg: rgb_const(0xececec),
        input_bg: rgb_const(0xffffff),
        accent: rgb_const(0x0067c0),
    };

    pub(crate) const DARK: Self = Self {
        toolbar_bg: rgb_const(0x202020),
        toolbar_border: rgb_const(0x353535),
        text: rgb_const(0xf0f0f0),
        text_muted: rgb_const(0xa8a8a8),
        button_hover: rgb_const(0x2f2f2f),
        button_active: rgb_const(0x3c3c3c),
        canvas_bg: rgb_const(0x2b2b2b),
        page_border: rgb_const(0x101010),
        error_text: rgb_const(0xff6b81),
        overlay_bg: rgba_const(0x000000c0),
        overlay_text: rgb_const(0x9cff9c),
        drop_highlight: rgba_const(0x4cc2ff30),
        sidebar_bg: rgb_const(0x262626),
        input_bg: rgb_const(0x2d2d2d),
        accent: rgb_const(0x4cc2ff),
    };
}

/// The theme of the reader window, for elements that paint outside the
/// reader view's render (text fields). Kept current by the reader view.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ActiveTheme(pub Theme);

impl Global for ActiveTheme {}

impl ActiveTheme {
    pub(crate) fn get(cx: &App) -> Theme {
        cx.try_global::<Self>().map_or(Theme::LIGHT, |t| t.0)
    }
}

/// Highlights drawn over page content: search hits and the text selection.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct PageHighlights {
    pub search_hit: Rgba,
    pub search_active: Rgba,
    pub selection: Rgba,
}

impl PageHighlights {
    /// On inverted (dark) pages the same colors need more opacity to show.
    pub(crate) fn for_mode(mode: ColorMode) -> Self {
        match mode {
            ColorMode::Inverted => Self {
                search_hit: rgba_const(0xffd00070),
                search_active: rgba_const(0xff8c1a98),
                selection: rgba_const(0x3a96dd88),
            },
            _ => Self {
                search_hit: rgba_const(0xffd00060),
                search_active: rgba_const(0xff7a0080),
                selection: rgba_const(0x0078d750),
            },
        }
    }
}

/// Paper painted where a page has no rendered tile yet, so it matches the
/// tiles: the session's paper color, inverted in night mode (straight
/// `255 - c`, which is the guard layer's premultiplied `a - c`).
pub(crate) fn paper(paper: Rgba8, mode: ColorMode) -> Rgba {
    let c = match mode {
        ColorMode::Inverted => paper.inverted(),
        _ => paper,
    };
    Rgba {
        r: f32::from(c.r) / 255.0,
        g: f32::from(c.g) / 255.0,
        b: f32::from(c.b) / 255.0,
        a: f32::from(c.a) / 255.0,
    }
}

/// `gpui::rgb` is not `const`; same conversion for constants.
const fn rgb_const(hex: u32) -> Rgba {
    rgba_const((hex << 8) | 0xff)
}

const fn rgba_const(hex: u32) -> Rgba {
    Rgba {
        r: ((hex >> 24) & 0xff) as f32 / 255.0,
        g: ((hex >> 16) & 0xff) as f32 / 255.0,
        b: ((hex >> 8) & 0xff) as f32 / 255.0,
        a: (hex & 0xff) as f32 / 255.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{rgb, rgba};

    #[test]
    fn const_colors_match_gpui_helpers() {
        assert_eq!(rgb_const(0x123456), rgb(0x123456));
        assert_eq!(rgba_const(0x12345678), rgba(0x12345678));
    }

    #[test]
    fn appearance_setting_overrides_the_system() {
        use WindowAppearance as W;
        assert_eq!(Theme::resolve(Appearance::System, W::Dark), Theme::DARK);
        assert_eq!(
            Theme::resolve(Appearance::System, W::VibrantLight),
            Theme::LIGHT
        );
        assert_eq!(Theme::resolve(Appearance::Light, W::Dark), Theme::LIGHT);
        assert_eq!(Theme::resolve(Appearance::Dark, W::Light), Theme::DARK);
    }

    #[test]
    fn night_paper_is_the_inverted_paper() {
        assert_eq!(paper(Rgba8::WHITE, ColorMode::Normal), rgb(0xffffff));
        assert_eq!(paper(Rgba8::WHITE, ColorMode::Inverted), rgb(0x000000));
        let cream = Rgba8::new(0xff, 0xf8, 0xe0, 0xff);
        assert_eq!(paper(cream, ColorMode::Inverted), rgb(0x00071f));
        assert_ne!(
            PageHighlights::for_mode(ColorMode::Inverted),
            PageHighlights::for_mode(ColorMode::Normal)
        );
    }

    /// Text must stay readable on every surface it is drawn on.
    #[test]
    fn text_contrasts_with_its_backgrounds() {
        fn luminance(c: Rgba) -> f32 {
            let lin = |v: f32| {
                if v <= 0.040_45 {
                    v / 12.92
                } else {
                    ((v + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * lin(c.r) + 0.7152 * lin(c.g) + 0.0722 * lin(c.b)
        }
        fn contrast(a: Rgba, b: Rgba) -> f32 {
            let (la, lb) = (luminance(a), luminance(b));
            (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
        }
        for theme in [Theme::LIGHT, Theme::DARK] {
            for bg in [
                theme.toolbar_bg,
                theme.sidebar_bg,
                theme.input_bg,
                theme.canvas_bg,
            ] {
                assert!(contrast(theme.text, bg) >= 7.0, "{theme:?}");
                assert!(contrast(theme.text_muted, bg) >= 4.5, "{theme:?}");
            }
            assert!(contrast(theme.accent, theme.toolbar_bg) >= 3.0);
        }
    }
}
