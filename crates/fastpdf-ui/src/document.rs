//! Opening documents off the UI thread (spec §10, §11).

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use fastpdf_core::DocumentSession;
use fastpdf_core::loader::{self, LoadError, LoadStrategy};
use fastpdf_engine_api::{
    DocumentSource, EngineDocument, EngineError, GuardedDocument, OpenOptions, PageIndex,
    PdfEngine, open_guarded,
};
use futures::channel::oneshot;

use crate::startup::{SessionParts, ViewGuess};
use crate::textures::TileImage;

/// A document that is ready for a session.
#[derive(Debug)]
pub struct OpenedDocument {
    pub path: PathBuf,
    pub doc: Arc<GuardedDocument>,
    pub strategy: LoadStrategy,
    pub file_bytes: usize,
    /// Time spent getting the bytes (read or map).
    pub load_ms: f64,
    /// Time spent in the engine's open plus the first page's geometry.
    pub open_ms: f64,
}

/// Why a document could not be opened. Displayed to the user as is.
#[derive(Debug)]
pub enum OpenFailure {
    Load(LoadError),
    Engine(EngineError),
    /// The opening thread went away without an answer.
    Abandoned,
}

impl fmt::Display for OpenFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Load(e) => e.fmt(f),
            Self::Engine(EngineError::PasswordRequired) => {
                f.write_str("this document is password protected (not supported yet)")
            }
            Self::Engine(e) => e.fmt(f),
            Self::Abandoned => f.write_str("opening was interrupted"),
        }
    }
}

impl std::error::Error for OpenFailure {}

/// Reads `path` and opens it with `engine`. Blocking; run it off the UI
/// thread. Only the minimum is done (spec §11): load or map the bytes, let
/// the engine answer `page_count`, and resolve page 1's geometry so the
/// session's first layout does not call the engine on the UI thread.
pub fn open_document_blocking(
    engine: &dyn PdfEngine,
    path: &Path,
) -> Result<OpenedDocument, OpenFailure> {
    let started = Instant::now();
    let loaded = loader::load(path).map_err(OpenFailure::Load)?;
    let loaded_at = Instant::now();
    let file_bytes = loaded.bytes.len();
    let source = DocumentSource::from_bytes(loaded.bytes).with_path(path);
    let doc = open_guarded(engine, source, &OpenOptions::default()).map_err(OpenFailure::Engine)?;
    // Cached by GuardedDocument; errors surface again (as page errors) later.
    let _ = doc.page_info(PageIndex::FIRST);
    let done = Instant::now();
    let opened = OpenedDocument {
        path: path.to_path_buf(),
        doc: Arc::new(doc),
        strategy: loaded.strategy,
        file_bytes,
        load_ms: ms(loaded_at - started),
        open_ms: ms(done - loaded_at),
    };
    log::info!(
        "opened {} ({} pages, {} bytes {}) load {:.1} ms, open {:.1} ms",
        path.display(),
        opened.doc.page_count(),
        file_bytes,
        opened.strategy,
        opened.load_ms,
        opened.open_ms
    );
    Ok(opened)
}

fn ms(d: std::time::Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

/// An opened document, with its session when one was started early
/// (`crate::startup`).
pub(crate) struct Opened {
    pub doc: OpenedDocument,
    pub session: Option<DocumentSession<TileImage>>,
}

impl fmt::Debug for Opened {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Opened")
            .field("doc", &self.doc)
            .field("early_session", &self.session.is_some())
            .finish()
    }
}

impl From<OpenedDocument> for Opened {
    fn from(doc: OpenedDocument) -> Self {
        Self { doc, session: None }
    }
}

/// An open that started before the window existed (the command-line file):
/// it runs on its own thread so engine work overlaps GPUI's start-up, and the
/// view picks the result up once it can show it.
#[derive(Debug)]
pub struct PendingOpen {
    pub(crate) path: PathBuf,
    pub(crate) result: oneshot::Receiver<Result<Opened, OpenFailure>>,
}

impl PendingOpen {
    /// Starts opening `path` on a new thread.
    pub fn spawn(engine: Arc<dyn PdfEngine>, path: PathBuf) -> Self {
        Self::spawn_with(engine, path, None)
    }

    /// Starts opening `path` on a new thread; with `early`, the thread also
    /// creates the document's session for the predicted view and asks it
    /// for a frame, so page 1 renders while GPUI starts.
    pub(crate) fn spawn_with(
        engine: Arc<dyn PdfEngine>,
        path: PathBuf,
        early: Option<(SessionParts, ViewGuess)>,
    ) -> Self {
        let (tx, rx) = oneshot::channel();
        let thread_path = path.clone();
        let spawned = std::thread::Builder::new()
            .name("fastpdf-open".into())
            .spawn(move || {
                let result = open_document_blocking(engine.as_ref(), &thread_path).map(|doc| {
                    let session = early.map(|(parts, view)| {
                        let mut session = parts.session(&doc, (view.width, view.height), view.scale);
                        // Plans and schedules the visible tiles (P0) and
                        // their neighbors; workers start right away.
                        let frame = session.frame();
                        log::info!(
                            "page 1 rendering before the window exists: {} tiles queued for a {:.0}x{:.0} view at scale {}",
                            frame.pending,
                            view.width,
                            view.height,
                            view.scale
                        );
                        session
                    });
                    Opened { doc, session }
                });
                let _ = tx.send(result);
            });
        if let Err(e) = spawned {
            // The receiver reports `Abandoned` because the sender was dropped.
            log::error!("cannot start the open thread: {e}");
        }
        Self { path, result: rx }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}
