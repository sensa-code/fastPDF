//! Where a document starts: while the open thread loads and opens the
//! document, a helper thread looks its remembered position up; the record
//! is checked against the document as it is now and the restored row's
//! geometry is fetched; a session then applies it before its first frame,
//! so the first page planned and rendered is the restored one (page 1 is
//! never rendered first). Every open that may start somewhere else than
//! page 1 goes through here: the command-line document (also its early
//! session, `crate::startup`), later opens and a password unlock.

use std::path::Path;

use fastpdf_core::DocumentSession;
use fastpdf_core::positions::{FileIdentity, PositionRecord, PositionStore, Restore, restore_for};
use fastpdf_engine_api::{EngineDocument, PageIndex, PdfEngine};
use gpui::Context;

use crate::document::{OpenFailure, Opened, OpenedDocument, load_source, open_loaded};
use crate::navigation::apply_default_zoom;
use crate::reader::ReaderView;
use crate::settings::DefaultZoom;
use crate::startup::{SessionParts, ViewGuess};
use crate::textures::TileImage;

#[cfg(test)]
mod tests;

/// Pages around the restored one whose geometry the open thread fetches:
/// its row in every layout, and the rows next to it.
const PREFETCH_BEFORE: u32 = 2;
const PREFETCH_AFTER: u32 = 3;

/// What the reading positions said about a document being opened.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ReadingStart {
    /// Where it starts; `None`: at the top, in the default zoom.
    pub restore: Option<Restore>,
    /// The file's size and time before it was loaded.
    pub identity: Option<FileIdentity>,
    /// The store had a record of it (even one that no longer applies).
    pub remembered: bool,
}

/// Opens `path` (blocking; the open thread or a background task) to start
/// where `positions` says it was left. With `early`, the document's session
/// is created for the predicted view and asked for a frame, so the tiles of
/// the first page shown render while GPUI starts.
pub(crate) fn open_for_reading(
    engine: &dyn PdfEngine,
    path: &Path,
    positions: Option<&PositionStore>,
    early: Option<(SessionParts, ViewGuess)>,
) -> Result<Opened, OpenFailure> {
    // Before the load: a file replaced meanwhile counts as changed.
    let identity = positions.and_then(|_| FileIdentity::of(path));
    // The positions file is read and parsed alongside the load and the
    // engine's open instead of after them.
    let (doc, looked_up) = std::thread::scope(|scope| {
        let lookup = positions.map(|store| {
            std::thread::Builder::new()
                .name("fastpdf-positions".into())
                .spawn_scoped(scope, move || store.lookup(path))
        });
        let doc = load_source(path).and_then(|mut loaded| {
            loaded.identity = identity;
            open_loaded(engine, loaded, None)
        });
        let looked_up = match (positions, lookup) {
            // A lookup that panicked found nothing.
            (_, Some(Ok(thread))) => Some(thread.join().ok().flatten()),
            // No thread to spare: look it up here.
            (Some(store), Some(Err(_))) => Some(store.lookup(path)),
            _ => None,
        };
        (doc, looked_up)
    });
    let mut opened = start_from(doc?, identity, looked_up);
    if let Some((parts, view)) = early {
        let restore = opened.start.and_then(|s| s.restore);
        let mut session = parts.session(
            &opened.doc,
            (view.width, view.height),
            view.scale,
            restore.as_ref(),
        );
        // Plans and schedules the visible tiles (P0) and their neighbors;
        // workers start right away.
        let frame = session.frame();
        log::info!(
            "page {} rendering before the window exists: {} tiles queued for a {:.0}x{:.0} view at scale {}",
            session.current_page().display_number(),
            frame.pending,
            view.width,
            view.height,
            view.scale
        );
        opened.session = Some(session);
    }
    Ok(opened)
}

/// `doc` with its start from `positions` (blocking): the remembered record,
/// how it applies to the document now (`identity`: the file when it was
/// loaded) and the geometry of the restored row, so the session's first
/// layout does not ask the engine for it on the UI thread.
pub(crate) fn with_start(
    doc: OpenedDocument,
    identity: Option<FileIdentity>,
    positions: Option<&PositionStore>,
) -> Opened {
    let looked_up = positions.map(|store| store.lookup(&doc.path));
    start_from(doc, identity, looked_up)
}

/// `doc` with the start `looked_up` gives: `None` when the store was not
/// asked, else its answer.
fn start_from(
    doc: OpenedDocument,
    identity: Option<FileIdentity>,
    looked_up: Option<Option<PositionRecord>>,
) -> Opened {
    let Some(record) = looked_up else {
        return doc.into();
    };
    let page_count = doc.doc.page_count();
    let restore = record.and_then(|r| restore_for(&r, page_count, identity));
    match (&record, &restore) {
        (Some(record), None) => log::info!(
            "the remembered page {} of {} is past the end ({page_count} pages)",
            record.view.page.display_number(),
            doc.path.display()
        ),
        (_, Some(restore)) => {
            let page = restore.view().page.get();
            let last = page_count.saturating_sub(1);
            for p in
                page.saturating_sub(PREFETCH_BEFORE)..=page.saturating_add(PREFETCH_AFTER).min(last)
            {
                // Cached by GuardedDocument; errors surface again (as page
                // errors) when the session lays the page out.
                let _ = doc.doc.page_info(PageIndex::new(p));
            }
        }
        (None, None) => {}
    }
    Opened {
        start: Some(ReadingStart {
            restore,
            identity,
            remembered: record.is_some(),
        }),
        ..doc.into()
    }
}

/// Puts a new session where it starts: the restored view, else the top in
/// the default zoom. Before the first frame, so nothing renders at another
/// place or zoom. Applying it again to a session nothing was shown of
/// gives the same view.
pub(crate) fn apply_start(
    session: &mut DocumentSession<TileImage>,
    restore: Option<&Restore>,
    zoom: DefaultZoom,
) {
    // The default zoom first: a place restore keeps the session's zoom.
    if !matches!(restore, Some(Restore::Exact(_))) {
        apply_default_zoom(session, zoom);
    }
    if let Some(restore) = restore {
        session.restore_saved(restore.view(), restore.scope());
    }
}

impl ReaderView {
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
}
