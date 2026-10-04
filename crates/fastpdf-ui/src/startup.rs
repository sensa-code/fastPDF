//! Work done while GPUI starts (spec §10; §29 "small PDF: first page
//! < 200 ms").
//!
//! GPUI's platform start-up (DirectWrite, the D3D11 device, the window)
//! takes most of the time before the first frame, and none of it needs the
//! document. So everything the first frame needs that does not need GPUI
//! runs meanwhile on the open thread: the settings are read, the
//! command-line document is opened and — when the window's size can be
//! predicted — its [`DocumentSession`] is created at that size and asked
//! for a frame. Page 1's tiles then render, and become GPU-ready images on
//! the render workers, while GPUI is still starting; the view adopts the
//! session and its first frame shows exact tiles.
//!
//! A wrong guess costs nothing but the early work: the view resizes the
//! session to the real document area, tiles of the same scale bucket stay
//! valid and the rest render as usual.

use std::path::PathBuf;
use std::sync::Arc;

use fastpdf_core::{DocumentSession, SessionConfig};
use fastpdf_engine_api::ColorMode;
use futures::channel::mpsc;

use crate::document::{OpenedDocument, PendingOpen};
use crate::reader::{ReaderOptions, Waker, apply_default_zoom};
use crate::settings::{DefaultZoom, Settings, WindowPlacement};
use crate::textures::{RetireQueue, TileImage, to_render_image};

/// The primary display, as the app sees it before GPUI starts.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScreenGuess {
    /// Size of the primary monitor in physical pixels.
    pub width_px: u32,
    pub height_px: u32,
    /// Its scale factor (DPI / 96).
    pub scale: f32,
}

/// The document area of the first frame, in logical pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ViewGuess {
    pub width: f32,
    pub height: f32,
    pub scale: f32,
}

/// The default window: portrait, fitting a display of `screen` logical
/// pixels. The single definition for both the guess and the real window.
pub(crate) fn default_window_size(screen_width: f32, screen_height: f32) -> (f32, f32) {
    let height = (screen_height * 0.85).clamp(480.0, 1400.0);
    let width = (height * 0.9).clamp(640.0, (screen_width * 0.9).max(640.0));
    (width, height)
}

/// Predicts the document area of the first frame from the saved window
/// placement (or the default window on `screen`), minus the toolbar and,
/// when it starts open, the sidebar. `None` when it cannot be predicted
/// (a maximized window fills a work area this code does not know).
pub(crate) fn guess_view(settings: &Settings, screen: ScreenGuess) -> Option<ViewGuess> {
    let scale = screen.scale;
    if !(scale.is_finite() && scale > 0.0) || screen.width_px == 0 || screen.height_px == 0 {
        return None;
    }
    let (width, height) = match settings.window.filter(WindowPlacement::is_plausible) {
        Some(p) if p.maximized => return None,
        Some(p) => (p.width, p.height),
        None => default_window_size(
            screen.width_px as f32 / scale,
            screen.height_px as f32 / scale,
        ),
    };
    // The platform window gets whole device pixels.
    let (width, height) = (
        (width * scale).round() / scale,
        (height * scale).round() / scale,
    );
    let sidebar = if settings.sidebar_open {
        crate::sidebar::WIDTH
    } else {
        0.0
    };
    let view = ViewGuess {
        width: width - sidebar,
        height: height - crate::toolbar::HEIGHT,
        scale,
    };
    (view.width >= 1.0 && view.height >= 1.0).then_some(view)
}

/// What a session needs besides the document; shared by the early start
/// and by documents opened later.
#[derive(Clone)]
pub(crate) struct SessionParts {
    pub config: SessionConfig,
    pub color: ColorMode,
    pub zoom: DefaultZoom,
    pub wake: Waker,
    pub retire: RetireQueue,
}

impl SessionParts {
    /// A session for `doc` showing a `size` view at `scale`, in the
    /// configured colors and zoom. Evicted images go to the retire queue
    /// (only the UI thread may release GPU textures).
    pub(crate) fn session(
        self,
        doc: &OpenedDocument,
        size: (f32, f32),
        scale: f32,
    ) -> DocumentSession<TileImage> {
        let Self {
            config,
            color,
            zoom,
            wake,
            retire,
        } = self;
        let evict_wake = wake.clone();
        let mut session = DocumentSession::new(
            Arc::clone(&doc.doc),
            config,
            size,
            scale,
            to_render_image,
            move || wake.wake(),
            move |evicted| {
                if retire.retire(evicted.into_iter().map(|(_, image)| image)) {
                    evict_wake.wake();
                }
            },
        );
        // Before the first frame, so no tile renders in the wrong colors or
        // at the wrong zoom; fit width is the session's own start.
        session.set_color_mode(color);
        if zoom != DefaultZoom::FitWidth {
            apply_default_zoom(&mut session, zoom);
        }
        session
    }
}

/// Everything prepared before the window exists, handed to
/// [`crate::open_reader_window`].
pub struct Startup {
    pub(crate) settings: Settings,
    pub(crate) pending: Option<PendingOpen>,
    pub(crate) wake: Waker,
    pub(crate) wake_rx: mpsc::UnboundedReceiver<()>,
    pub(crate) retire: RetireQueue,
}

impl std::fmt::Debug for Startup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Startup")
            .field("settings", &self.settings)
            .field("pending", &self.pending)
            .finish_non_exhaustive()
    }
}

impl Startup {
    /// Reads the settings and starts opening `file` (if any) on its own
    /// thread; with `screen`, page 1 starts rendering there too. Call it as
    /// early as possible, before GPUI starts.
    pub fn begin(
        options: &ReaderOptions,
        file: Option<PathBuf>,
        screen: Option<ScreenGuess>,
    ) -> Self {
        let settings = options
            .settings_file
            .as_deref()
            .map(Settings::load)
            .unwrap_or_default();
        let (tx, wake_rx) = mpsc::unbounded();
        let wake = Waker(tx);
        let retire = RetireQueue::default();
        let pending = file.map(|path| {
            let early = screen.and_then(|s| guess_view(&settings, s)).map(|view| {
                let parts = SessionParts {
                    config: options.session.clone(),
                    color: if settings.night_mode {
                        ColorMode::Inverted
                    } else {
                        ColorMode::Normal
                    },
                    zoom: settings.default_zoom,
                    wake: wake.clone(),
                    retire: retire.clone(),
                };
                (parts, view)
            });
            PendingOpen::spawn_with(Arc::clone(&options.engine), path, early)
        });
        Self {
            settings,
            pending,
            wake,
            wake_rx,
            retire,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sidebar::SidebarTab;

    const FULL_HD: ScreenGuess = ScreenGuess {
        width_px: 1920,
        height_px: 1080,
        scale: 1.0,
    };

    #[test]
    fn the_default_window_is_predicted_like_the_real_one() {
        // 1080 * 0.85 = 918 high, 826.2 wide (826 device pixels); minus
        // the 40 px toolbar.
        let view = guess_view(&Settings::default(), FULL_HD).expect("predictable");
        assert_eq!((view.width, view.height, view.scale), (826.0, 878.0, 1.0));
        // 4K at 150%: the same logical screen as 2560 x 1440.
        let hidpi = ScreenGuess {
            width_px: 3840,
            height_px: 2160,
            scale: 1.5,
        };
        let view = guess_view(&Settings::default(), hidpi).expect("predictable");
        assert_eq!(view.scale, 1.5);
        // 1440 * 0.85 = 1224 logical = 1836 device pixels; minus the toolbar.
        assert_eq!(view.height, 1184.0);
        let (width, _) = default_window_size(2560.0, 1440.0);
        assert_eq!(view.width, (width * 1.5).round() / 1.5);
    }

    #[test]
    fn saved_placements_and_the_sidebar_shape_the_guess() {
        let settings = Settings {
            sidebar_open: true,
            sidebar_tab: SidebarTab::Pages,
            window: Some(WindowPlacement {
                x: 10.0,
                y: 10.0,
                width: 1184.0,
                height: 781.0,
                maximized: false,
            }),
            ..Settings::default()
        };
        let view = guess_view(&settings, FULL_HD).expect("predictable");
        assert_eq!((view.width, view.height), (1184.0 - 220.0, 781.0 - 40.0));
        let maximized = Settings {
            window: settings.window.map(|w| WindowPlacement {
                maximized: true,
                ..w
            }),
            ..settings
        };
        assert_eq!(guess_view(&maximized, FULL_HD), None);
        let broken = ScreenGuess {
            scale: f32::NAN,
            ..FULL_HD
        };
        assert_eq!(guess_view(&Settings::default(), broken), None);
    }

    #[test]
    fn sessions_can_be_prepared_off_the_ui_thread() {
        fn assert_send<T: Send>() {}
        assert_send::<DocumentSession<TileImage>>();
        assert_send::<SessionParts>();
    }
}
