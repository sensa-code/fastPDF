//! Reopening documents where they were left (reading positions).
//!
//! The positions live in a [`PositionStore`] next to the recent-files list
//! (`fastpdf_core::positions`), so an empty `FASTPDF_RECENT_FILE`, which
//! turns the list off, turns them off too, and tests and benchmarks never
//! touch the user's profile. The setting `remember_position` (settings
//! panel, on by default) turns reading and writing them off; the file
//! stays.
//!
//! * Opening (`start.rs`): the open thread looks the document up and
//!   fetches the restored row's geometry; the session applies the restored
//!   view before its first frame, so the restored page is the first page
//!   rendered. The same file and page count restore page, offset, zoom and
//!   rotation; a changed file restores page and offset in the default zoom;
//!   a page past the end restores nothing. The page layout stays global.
//! * Reading (`tracker.rs`): the canvas notes the view every frame (no IO,
//!   no allocation); once it has been still for two seconds after moving,
//!   one timer queues the record and a background task writes the file. No
//!   polling: an idle reader has no timer.
//! * Closing, switching documents and quitting write the view right away:
//!   closing in the background, quitting (Ctrl+Q, and the window's close
//!   button, where only the view's release still runs) on the UI thread.
//!   A crash loses at most the last two seconds of movement.

mod gate;
mod start;
mod tracker;

use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use fastpdf_core::positions::{PositionRecord, PositionStore, quantized};
use fastpdf_core::{DocumentSession, SavedView};
use gpui::{Context, Task};

use gate::{Follow, Gate};
pub(crate) use start::{ReadingStart, apply_start, open_for_reading, with_start};
use tracker::{SETTLE, Settle, Tracked};

use crate::reader::ReaderView;
use crate::textures::TileImage;

/// The store and the open document's position as last written and seen.
#[derive(Default)]
pub(crate) struct ReadingMemory {
    /// `None`: there is no recent-files list, so no positions either.
    store: Option<Arc<PositionStore>>,
    doc: Option<Tracked>,
    /// Fires [`SETTLE`] after the view started moving.
    settle: Option<Task<()>>,
}

impl ReadingMemory {
    pub(crate) fn new(store: Option<Arc<PositionStore>>) -> Self {
        Self {
            store,
            ..Self::default()
        }
    }

    /// Positions can be kept (the settings panel says so when not).
    pub(crate) fn has_store(&self) -> bool {
        self.store.is_some()
    }
}

impl ReaderView {
    /// What reading positions may do now (`gate.rs`).
    fn reading_gate(&self) -> Gate {
        Gate {
            store: self.reading.has_store(),
            remember: self.settings.remember_position,
        }
    }

    /// The store an open looks the document up in; `None` when the setting
    /// is off or there is no store.
    pub(crate) fn positions_for_open(&self) -> Option<Arc<PositionStore>> {
        let lookup = self.reading_gate().lookup();
        self.reading.store.clone().filter(|_| lookup)
    }

    /// A document finished opening with `start`, maybe with an early
    /// session already at the restored view. When positions were turned
    /// off meanwhile the start is dropped, and so is such a session (off
    /// the UI thread: its render workers join): the document starts at the
    /// top, like any document opened with positions off.
    pub(crate) fn screen_start(
        &self,
        start: Option<ReadingStart>,
        session: Option<DocumentSession<TileImage>>,
        cx: &mut Context<'_, Self>,
    ) -> (Option<ReadingStart>, Option<DocumentSession<TileImage>>) {
        if start.is_none() || self.reading_gate().keep(start).is_some() {
            return (start, session);
        }
        let restored = start.is_some_and(|s| s.restore.is_some());
        let session = match session {
            Some(early) if restored => {
                cx.background_executor()
                    .spawn(async move { drop(early) })
                    .detach();
                None
            }
            other => other,
        };
        (None, session)
    }

    /// The document at `path` is open (its start is applied): follow its
    /// view. Without a start (positions were off when it started opening)
    /// only when they are on now: its view is then written like a moved one.
    pub(crate) fn track_reading(&mut self, path: &Path, start: Option<ReadingStart>) {
        self.reading.settle = None;
        self.reading.doc = None;
        let follow = self.reading_gate().follow(start);
        let Some(session) = self.session() else {
            return;
        };
        let view = quantized(session.saved_view());
        let page_count = session.page_count();
        let path = path.to_path_buf();
        let now = Instant::now();
        self.reading.doc = match follow {
            Follow::Start(start) => {
                if let Some(restore) = &start.restore {
                    log::info!(
                        "restored the reading position of {}: page {} ({})",
                        path.display(),
                        restore.view().page.display_number(),
                        restore.kind()
                    );
                }
                Some(Tracked::new(path, page_count, &start, view, now))
            }
            Follow::Adopt => Some(Tracked::adopted(path, page_count, view, now)),
            Follow::Nothing => None,
        };
    }

    /// Canvas prepaint: notes where the view is and starts the settle timer
    /// when it moved. Cheap (no IO, no allocation); nothing when off.
    pub(crate) fn note_reading_position(&mut self, cx: &mut Context<'_, Self>) {
        if !self.reading_gate().write() || self.reading.doc.is_none() {
            return;
        }
        let Some(view) = self.reading_view() else {
            return;
        };
        let moved = self
            .reading
            .doc
            .as_mut()
            .is_some_and(|doc| doc.observe(view, Instant::now()));
        if moved && self.reading.settle.is_none() {
            self.arm_reading_timer(SETTLE, cx);
        }
    }

    fn arm_reading_timer(&mut self, after: std::time::Duration, cx: &mut Context<'_, Self>) {
        self.reading.settle = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(after).await;
            let _ = this.update(cx, |this, cx| this.reading_settled(cx));
        }));
    }

    /// The settle timer fired: write the view if it stayed still, else
    /// wait for it to.
    fn reading_settled(&mut self, cx: &mut Context<'_, Self>) {
        self.reading.settle = None;
        if !self.reading_gate().write() {
            return;
        }
        let Some(view) = self.reading_view() else {
            return;
        };
        let Some(doc) = self.reading.doc.as_mut() else {
            return;
        };
        match doc.settled(view, Instant::now()) {
            Settle::Write(record) => {
                let path = doc.path().to_path_buf();
                self.queue_position(&path, record, "settled");
                self.flush_positions_later(cx);
            }
            Settle::Wait(after) => self.arm_reading_timer(after, cx),
            Settle::Nothing => {}
        }
    }

    /// The document closes (or another one replaces it): its position is
    /// written in the background.
    pub(crate) fn save_reading_position(&mut self, cx: &mut Context<'_, Self>) {
        if self.close_reading("closed") {
            self.flush_positions_later(cx);
        }
        self.reading.doc = None;
    }

    /// FastPDF quits or the window closes: the open document's position and
    /// everything not written yet goes to the file now, on this thread (a
    /// background write might not finish before the process ends). Running
    /// it twice writes nothing new.
    pub(crate) fn save_reading_position_now(&mut self) {
        self.close_reading("exit");
        if let Some(store) = &self.reading.store {
            flush_positions(store);
        }
    }

    /// Turns remembering positions on or off. Off stops reading and
    /// writing them (the file stays); on follows the open document from
    /// where it is (`track_reading` without a start).
    pub(crate) fn set_remember_position(&mut self, on: bool, cx: &mut Context<'_, Self>) {
        self.settings.remember_position = on;
        if !on {
            self.reading.settle = None;
        } else if self.reading.doc.is_none()
            && let crate::reader::DocState::Open(open) = &self.doc
        {
            let path = open.path.clone();
            self.track_reading(&path, None);
        }
        self.save_settings(cx);
        cx.notify();
    }

    /// The open document's view at the file's precision.
    fn reading_view(&self) -> Option<SavedView> {
        self.session().map(|s| quantized(s.saved_view()))
    }

    /// Queues the open document's record when closing it calls for one;
    /// true when it did.
    fn close_reading(&mut self, why: &str) -> bool {
        self.reading.settle = None;
        if !self.reading_gate().write() {
            return false;
        }
        let Some(view) = self.reading_view() else {
            return false;
        };
        let Some(doc) = self.reading.doc.as_mut() else {
            return false;
        };
        let Some(record) = doc.closing(view) else {
            return false;
        };
        let path = doc.path().to_path_buf();
        self.queue_position(&path, record, why);
        true
    }

    /// Hands `record` to the store; `why` (settled, closed, exit) is
    /// only logged.
    fn queue_position(&self, path: &Path, record: PositionRecord, why: &str) {
        let Some(store) = &self.reading.store else {
            return;
        };
        if store.remember(path, record) {
            log::info!(
                "reading position of {} saved ({why}): page {}",
                path.display(),
                record.view.page.display_number()
            );
        } else {
            log::info!(
                "reading position of {} not kept: the positions file cannot hold its path",
                path.display()
            );
        }
    }

    fn flush_positions_later(&self, cx: &mut Context<'_, Self>) {
        let Some(store) = self.reading.store.clone() else {
            return;
        };
        cx.background_executor()
            .spawn(async move { flush_positions(&store) })
            .detach();
    }
}

/// Writes the records `store` holds, logging the outcome.
fn flush_positions(store: &PositionStore) {
    match store.flush() {
        Ok(0) => {}
        Ok(documents) => log::info!(
            "reading positions written to {} ({documents} documents)",
            store.path().display()
        ),
        Err(e) => log::warn!(
            "cannot save the reading positions to {}: {e}",
            store.path().display()
        ),
    }
}
