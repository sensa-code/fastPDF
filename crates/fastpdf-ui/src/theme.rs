//! Colors for the light and dark system appearance (spec §33 dark mode).
//! Deliberately tiny: minimal chrome, no theming engine (spec §20).

use gpui::{Rgba, WindowAppearance};

/// UI font: the Windows 11 system font; DirectWrite falls back for CJK.
pub(crate) const UI_FONT: &str = "Segoe UI";
/// Fixed-width font for the development overlay.
pub(crate) const MONO_FONT: &str = "Consolas";

#[derive(Debug, Clone, Copy)]
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
}

impl Theme {
    pub(crate) fn for_appearance(appearance: WindowAppearance) -> Self {
        match appearance {
            WindowAppearance::Dark | WindowAppearance::VibrantDark => Self::DARK,
            WindowAppearance::Light | WindowAppearance::VibrantLight => Self::LIGHT,
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
        overlay_bg: rgba_const(0x000000c0),
        overlay_text: rgb_const(0x9cff9c),
        drop_highlight: rgba_const(0x0067c030),
        sidebar_bg: rgb_const(0xececec),
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
    };
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
}
