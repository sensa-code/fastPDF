//! When the open document's position is written: once the view has been
//! still for [`SETTLE`] after moving, and when the document closes (or
//! FastPDF quits) — never on every scroll. Pure decisions, so they can be
//! tested without a window; `crate::reading` runs the timer and the store.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use fastpdf_core::SavedView;
use fastpdf_core::positions::{FileIdentity, PositionRecord};

use super::ReadingStart;

/// How long the view must stay still before its position is written.
pub(crate) const SETTLE: Duration = Duration::from_secs(2);

/// The decision when the settle timer fires.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Settle {
    /// Still for long enough and not written yet: write this record.
    Write(PositionRecord),
    /// Still moving (or not still for long enough): ask again after this.
    Wait(Duration),
    /// Nothing new to write.
    Nothing,
}

/// The open document whose position is followed.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Tracked {
    path: PathBuf,
    identity: Option<FileIdentity>,
    page_count: u32,
    /// The store has an entry that this session has not refreshed yet:
    /// closing writes even an unchanged view, so the document moves to the
    /// front of the store (the least recently left are dropped first).
    refresh: bool,
    /// The view as restored or opened, or as last written.
    saved: SavedView,
    /// The view the last observation saw, and since when.
    seen: SavedView,
    changed_at: Instant,
}

impl Tracked {
    /// `path` opened with `page_count` pages, showing `view` (after its
    /// start was applied).
    pub(crate) fn new(
        path: PathBuf,
        page_count: u32,
        start: &ReadingStart,
        view: SavedView,
        now: Instant,
    ) -> Self {
        Self {
            path,
            identity: start.identity,
            page_count,
            refresh: start.remembered,
            saved: view,
            seen: view,
            changed_at: now,
        }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// Notes the current view; true when it differs from the one written,
    /// so a settle timer should run (if none does).
    pub(crate) fn observe(&mut self, view: SavedView, now: Instant) -> bool {
        if view != self.seen {
            self.seen = view;
            self.changed_at = now;
        }
        self.seen != self.saved
    }

    /// The settle timer fired and the view is `view`.
    pub(crate) fn settled(&mut self, view: SavedView, now: Instant) -> Settle {
        self.observe(view, now);
        let still = now.saturating_duration_since(self.changed_at);
        if still < SETTLE {
            return Settle::Wait(SETTLE - still);
        }
        if view == self.saved {
            return Settle::Nothing;
        }
        Settle::Write(self.written(view))
    }

    /// The document closes (or FastPDF quits) showing `view`: the record to
    /// write, when the view moved since the last write or the store's entry
    /// is to be refreshed. Calling it again writes nothing new.
    pub(crate) fn closing(&mut self, view: SavedView) -> Option<PositionRecord> {
        (view != self.saved || self.refresh).then(|| self.written(view))
    }

    /// The record for `view`, which counts as written from now on.
    fn written(&mut self, view: SavedView) -> PositionRecord {
        self.saved = view;
        self.seen = view;
        self.refresh = false;
        PositionRecord {
            view,
            page_count: self.page_count,
            identity: self.identity,
        }
    }
}

#[cfg(test)]
mod tests {
    use fastpdf_core::SavedZoom;
    use fastpdf_engine_api::{PageIndex, Rotation};

    use super::*;

    fn at(page: u32, offset: f32) -> SavedView {
        SavedView {
            page: PageIndex::new(page),
            offset,
            x: 0.0,
            zoom: SavedZoom::FitWidth,
            rotation: Rotation::R0,
        }
    }

    fn start(remembered: bool) -> ReadingStart {
        ReadingStart {
            restore: None,
            identity: Some(FileIdentity {
                len: 10,
                modified: 20,
            }),
            remembered,
        }
    }

    fn tracked(remembered: bool, t0: Instant) -> Tracked {
        Tracked::new(
            PathBuf::from("a.pdf"),
            50,
            &start(remembered),
            at(0, 0.0),
            t0,
        )
    }

    #[test]
    fn an_unmoved_new_document_writes_nothing() {
        let t0 = Instant::now();
        let mut doc = tracked(false, t0);
        assert!(!doc.observe(at(0, 0.0), t0 + SETTLE));
        assert_eq!(doc.settled(at(0, 0.0), t0 + SETTLE * 2), Settle::Nothing);
        assert_eq!(doc.closing(at(0, 0.0)), None);
    }

    #[test]
    fn the_timer_waits_until_the_view_is_still() {
        let t0 = Instant::now();
        let mut doc = tracked(false, t0);
        let second = Duration::from_secs(1);
        // Moving: a timer should run; moving on keeps it wanted.
        assert!(doc.observe(at(3, 0.1), t0));
        assert!(doc.observe(at(4, 0.2), t0 + second));
        // It fires 2 s after the first move: the view moved 1 s ago.
        assert_eq!(doc.settled(at(4, 0.2), t0 + SETTLE), Settle::Wait(second));
        // Still moving when it fires again: a full settle time from now.
        assert_eq!(
            doc.settled(at(5, 0.0), t0 + SETTLE + second),
            Settle::Wait(SETTLE)
        );
        // Still for 2 s: written once.
        let written = doc.settled(at(5, 0.0), t0 + SETTLE * 2 + second);
        assert_eq!(
            written,
            Settle::Write(PositionRecord {
                view: at(5, 0.0),
                page_count: 50,
                identity: start(false).identity,
            })
        );
        assert!(!doc.observe(at(5, 0.0), t0 + SETTLE * 3));
        assert_eq!(doc.settled(at(5, 0.0), t0 + SETTLE * 4), Settle::Nothing);
        // Closing where it was written: nothing new.
        assert_eq!(doc.closing(at(5, 0.0)), None);
    }

    #[test]
    fn going_back_where_it_was_written_needs_no_write() {
        let t0 = Instant::now();
        let mut doc = tracked(false, t0);
        assert!(doc.observe(at(9, 0.5), t0));
        assert!(!doc.observe(at(0, 0.0), t0 + Duration::from_millis(500)));
        assert_eq!(doc.settled(at(0, 0.0), t0 + SETTLE * 2), Settle::Nothing);
    }

    #[test]
    fn closing_writes_a_moved_view_or_refreshes_an_entry_once() {
        let t0 = Instant::now();
        let mut moved = tracked(false, t0);
        moved.observe(at(7, 0.25), t0);
        let record = moved.closing(at(7, 0.25)).expect("moved");
        assert_eq!(record.view, at(7, 0.25));
        assert_eq!(moved.closing(at(7, 0.25)), None, "quit after close");

        // Opened where the store said and closed unmoved: the entry moves
        // to the front of the store, once.
        let mut remembered = tracked(true, t0);
        assert!(remembered.closing(at(0, 0.0)).is_some());
        assert_eq!(remembered.closing(at(0, 0.0)), None);
        // A settled write refreshes it too.
        let mut settled = tracked(true, t0);
        settled.observe(at(1, 0.0), t0);
        assert!(matches!(
            settled.settled(at(1, 0.0), t0 + SETTLE),
            Settle::Write(_)
        ));
        assert_eq!(settled.closing(at(1, 0.0)), None);
        assert_eq!(settled.path(), Path::new("a.pdf"));
    }
}
