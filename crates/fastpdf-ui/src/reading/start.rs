//! Where a document starts: on the open thread, the remembered position is
//! looked up, checked against the document as it is now and the restored
//! row's geometry is fetched; a session then applies it before its first
//! frame, so the first page planned and rendered is the restored one (page
//! 1 is never rendered first). Every open that may start somewhere else
//! than page 1 goes through here: the command-line document (also its early
//! session, `crate::startup`), later opens and a password unlock.

use std::path::Path;

use fastpdf_core::DocumentSession;
use fastpdf_core::positions::{FileIdentity, PositionStore, Restore, restore_for};
use fastpdf_engine_api::{EngineDocument, PageIndex, PdfEngine};

use crate::document::{OpenFailure, Opened, OpenedDocument, load_source, open_loaded};
use crate::navigation::apply_default_zoom;
use crate::settings::DefaultZoom;
use crate::startup::{SessionParts, ViewGuess};
use crate::textures::TileImage;

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
    let mut loaded = load_source(path)?;
    loaded.identity = identity;
    let doc = open_loaded(engine, loaded, None)?;
    let mut opened = with_start(doc, identity, positions);
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
    let Some(store) = positions else {
        return doc.into();
    };
    let page_count = doc.doc.page_count();
    let record = store.lookup(&doc.path);
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
