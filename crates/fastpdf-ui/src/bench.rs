//! Startup milestones for external benchmarks (benchmark plan B-8).
//!
//! The UI reports *when* things happen; the application decides how to print
//! them (one JSON line per event on stdout when `FASTPDF_BENCH=1`).

use std::sync::Arc;

/// A milestone on the way to the first visible page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BenchEvent {
    /// The main window exists and has been shown.
    WindowVisible,
    /// The first frame of the window has been presented.
    FirstPaint,
    /// A document session was created (engine open finished).
    DocumentOpened,
    /// Every visible tile of the first displayed page is at the exact
    /// resolution and has been presented.
    FirstPageExact,
}

impl BenchEvent {
    pub fn name(self) -> &'static str {
        match self {
            Self::WindowVisible => "window_visible",
            Self::FirstPaint => "first_paint",
            Self::DocumentOpened => "document_opened",
            Self::FirstPageExact => "first_page_exact",
        }
    }
}

/// Receives milestones; called on the UI thread.
pub type BenchHook = Arc<dyn Fn(BenchEvent) + Send + Sync>;
