//! Benchmark hooks (benchmark plan B-8): start-up milestones and frame
//! events.
//!
//! The UI reports *what* happens and *when*; the application decides what to
//! do with it. With `FASTPDF_BENCH=1` it prints the milestones as JSON lines
//! on stdout and counts the frame events (a write per frame would disturb
//! the timing being measured) in memory that `tools/bench-app` reads from
//! outside (`-AppProbe`), so a benchmark can check that an idle reader draws
//! no frames. Without `FASTPDF_BENCH` there is no hook: every report is a
//! `None` check, and nothing is created, counted or printed.

use std::sync::Arc;

/// Something the benchmark wants to know about.
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
    /// Frame event: the root view rendered (once per frame).
    Render,
    /// Frame event: the document canvas prepainted.
    Prepaint,
    /// Frame event: the document canvas painted.
    Paint,
    /// Frame event: background work (tiles, evictions, search, print) woke
    /// the UI thread.
    Wake,
}

impl BenchEvent {
    pub fn name(self) -> &'static str {
        match self {
            Self::WindowVisible => "window_visible",
            Self::FirstPaint => "first_paint",
            Self::DocumentOpened => "document_opened",
            Self::FirstPageExact => "first_page_exact",
            Self::Render => "render",
            Self::Prepaint => "prepaint",
            Self::Paint => "paint",
            Self::Wake => "wake",
        }
    }
}

/// Receives milestones and frame events; called on the UI thread.
pub type BenchHook = Arc<dyn Fn(BenchEvent) + Send + Sync>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_names_are_unique() {
        let all = [
            BenchEvent::WindowVisible,
            BenchEvent::FirstPaint,
            BenchEvent::DocumentOpened,
            BenchEvent::FirstPageExact,
            BenchEvent::Render,
            BenchEvent::Prepaint,
            BenchEvent::Paint,
            BenchEvent::Wake,
        ];
        let names: std::collections::HashSet<_> = all.iter().map(|e| e.name()).collect();
        assert_eq!(names.len(), all.len());
    }
}
