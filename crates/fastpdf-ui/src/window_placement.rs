//! Where the reader window opens and how its placement is kept: the saved
//! placement while its title bar is still on a display, else a default
//! window centered on the primary display; the placement is saved shortly
//! after the window stops moving (`crate::settings`).

use gpui::{App, Bounds, Context, Pixels, WindowAppearance, WindowBounds, px, size};

use crate::reader::ReaderView;
use crate::settings::{Appearance, WindowPlacement};
use crate::startup::default_window_size;

/// How long the window must stay put before its placement is saved.
const PLACEMENT_SETTLE: std::time::Duration = std::time::Duration::from_millis(600);
/// Part of a restored window's top edge that must lie on a display, so the
/// title bar can still be grabbed (a monitor may have been unplugged).
const GRIP: (f32, f32) = (120.0, 24.0);

/// Asks the platform to draw native window chrome in the chosen appearance.
/// GPUI implements this on macOS only; on Windows (this GPUI revision) the
/// title bar keeps following the system, while everything FastPDF paints
/// follows the setting.
pub(crate) fn apply_window_appearance(appearance: Appearance, cx: &mut App) {
    cx.set_window_appearance(match appearance {
        Appearance::System => None,
        Appearance::Light => Some(WindowAppearance::Light),
        Appearance::Dark => Some(WindowAppearance::Dark),
    });
}

/// The saved placement when it is still on a display, else a default
/// window centered on the primary display.
pub(crate) fn initial_window_bounds(saved: Option<WindowPlacement>, cx: &App) -> WindowBounds {
    if let Some(p) = saved.filter(WindowPlacement::is_plausible) {
        let bounds = Bounds::new(
            gpui::point(px(p.x), px(p.y)),
            size(px(p.width), px(p.height)),
        );
        let displays: Vec<Bounds<Pixels>> = cx.displays().iter().map(|d| d.bounds()).collect();
        if grip_visible(bounds, &displays) {
            return if p.maximized {
                WindowBounds::Maximized(bounds)
            } else {
                WindowBounds::Windowed(bounds)
            };
        }
        log::info!("the saved window placement is off screen; using the default");
    }
    WindowBounds::Windowed(Bounds::centered(None, default_window_size_on(cx), cx))
}

/// Whether enough of the window's top edge is on one of `displays`.
fn grip_visible(window: Bounds<Pixels>, displays: &[Bounds<Pixels>]) -> bool {
    let top = Bounds::new(window.origin, size(window.size.width, px(GRIP.1)));
    displays.iter().any(|d| {
        let seen = d.intersect(&top);
        seen.size.width >= px(GRIP.0) && seen.size.height >= px(GRIP.1)
    })
}

/// What to save for the window's current bounds (the restore bounds when
/// maximized; full screen is saved as its windowed bounds).
fn placement_of(bounds: WindowBounds) -> WindowPlacement {
    let b = bounds.get_bounds();
    WindowPlacement {
        x: f32::from(b.origin.x),
        y: f32::from(b.origin.y),
        width: f32::from(b.size.width),
        height: f32::from(b.size.height),
        maximized: matches!(bounds, WindowBounds::Maximized(_)),
    }
}

/// A portrait window that fits the primary display (the same size
/// `crate::startup` predicts).
fn default_window_size_on(cx: &App) -> gpui::Size<Pixels> {
    let Some(display) = cx.primary_display() else {
        return size(px(1024.0), px(768.0));
    };
    let screen = display.bounds().size;
    let (width, height) = default_window_size(f32::from(screen.width), f32::from(screen.height));
    size(px(width), px(height))
}

impl ReaderView {
    /// The window moved, was resized, maximized or restored: remember the
    /// placement once it has been still for a moment. A window that never
    /// left its initial (default) placement writes nothing.
    pub(crate) fn window_moved(&mut self, bounds: WindowBounds, cx: &mut Context<'_, Self>) {
        if bounds == self.initial_bounds && self.settings.window.is_none() {
            self.bounds_task = None;
            return;
        }
        // Replacing the task cancels the previous wait.
        self.bounds_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(PLACEMENT_SETTLE).await;
            let _ = this.update(cx, |this, cx| {
                this.settings.window = Some(placement_of(bounds));
                this.save_settings(cx);
            });
        }));
    }
}
