//! Opening documents off the UI thread (spec §10, §11).

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use fastpdf_core::DocumentSession;
use fastpdf_core::loader::{self, LoadError, LoadStrategy};
use fastpdf_core::positions::{FileIdentity, PositionStore};
use fastpdf_engine_api::{
    DocumentSource, EngineDocument, EngineError, GuardedDocument, OpenOptions, PageIndex, Password,
    PdfEngine, open_guarded,
};
use futures::channel::oneshot;

use crate::reading::ReadingStart;
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
    /// Opened with a password: nothing writes this document (ADR 0014).
    pub password_protected: bool,
}

/// Why a document could not be opened. Displayed to the user as is.
#[derive(Debug)]
pub enum OpenFailure {
    Load(LoadError),
    Engine(EngineError),
    /// The document is encrypted and needs a password, or the one given did
    /// not open it. The loaded file is kept for the next attempt.
    Locked(Box<LockedSource>),
    /// The opening thread went away without an answer.
    Abandoned,
}

impl fmt::Display for OpenFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Load(e) => e.fmt(f),
            Self::Engine(e) => e.fmt(f),
            Self::Locked(locked) if locked.wrong_password => {
                f.write_str("the password did not open the document")
            }
            Self::Locked(_) => f.write_str("a password is needed"),
            Self::Abandoned => f.write_str("opening was interrupted"),
        }
    }
}

impl std::error::Error for OpenFailure {}

/// A loaded (read or mapped) file that is not open yet: the input of
/// [`open_loaded`], and what an encrypted document keeps while it asks for
/// its password, so each attempt reuses the bytes or the file handle.
pub struct LockedSource {
    pub(crate) path: PathBuf,
    /// Shared bytes; cloning it does not copy the file.
    pub(crate) source: DocumentSource,
    pub(crate) strategy: LoadStrategy,
    pub(crate) file_bytes: usize,
    /// Time spent getting the bytes (read or map).
    pub(crate) load_ms: f64,
    /// A password was given and did not open the document.
    pub(crate) wrong_password: bool,
    /// The file's size and time before it was loaded, when reading
    /// positions are kept (`crate::reading`).
    pub(crate) identity: Option<FileIdentity>,
}

impl fmt::Debug for LockedSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LockedSource")
            .field("path", &self.path)
            .field("file_bytes", &self.file_bytes)
            .field("wrong_password", &self.wrong_password)
            .finish_non_exhaustive()
    }
}

/// Reads `path` and opens it with `engine`. Blocking; run it off the UI
/// thread. Only the minimum is done (spec §11): load or map the bytes, let
/// the engine answer `page_count`, and resolve page 1's geometry so the
/// session's first layout does not call the engine on the UI thread.
pub fn open_document_blocking(
    engine: &dyn PdfEngine,
    path: &Path,
) -> Result<OpenedDocument, OpenFailure> {
    open_loaded(engine, load_source(path)?, None)
}

/// Loads (reads or maps) `path` for [`open_loaded`].
pub(crate) fn load_source(path: &Path) -> Result<LockedSource, OpenFailure> {
    let started = Instant::now();
    let loaded = loader::load(path).map_err(OpenFailure::Load)?;
    let file_bytes = loaded.bytes.len();
    Ok(LockedSource {
        path: path.to_path_buf(),
        source: DocumentSource::from_bytes(loaded.bytes).with_path(path),
        strategy: loaded.strategy,
        file_bytes,
        load_ms: ms(started.elapsed()),
        wrong_password: false,
        identity: None,
    })
}

/// Opens a loaded file, with `password` for an encrypted document. Blocking.
/// When the document needs a password, or a different one, it comes back as
/// [`OpenFailure::Locked`] with the same bytes.
pub(crate) fn open_loaded(
    engine: &dyn PdfEngine,
    loaded: LockedSource,
    password: Option<Password>,
) -> Result<OpenedDocument, OpenFailure> {
    let started = Instant::now();
    let password_protected = password.is_some();
    let options = OpenOptions {
        password,
        ..OpenOptions::default()
    };
    let opened = open_guarded(engine, loaded.source.clone(), &options);
    // The engine keeps its own copy; this one is wiped now.
    drop(options);
    let doc = match opened {
        Ok(doc) => doc,
        Err(EngineError::PasswordRequired | EngineError::InvalidPassword) => {
            return Err(OpenFailure::Locked(Box::new(LockedSource {
                wrong_password: password_protected,
                ..loaded
            })));
        }
        Err(e) => return Err(OpenFailure::Engine(e)),
    };
    // Cached by GuardedDocument; errors surface again (as page errors) later.
    let _ = doc.page_info(PageIndex::FIRST);
    let opened = OpenedDocument {
        path: loaded.path,
        doc: Arc::new(doc),
        strategy: loaded.strategy,
        file_bytes: loaded.file_bytes,
        load_ms: loaded.load_ms,
        open_ms: ms(started.elapsed()),
        password_protected,
    };
    log::info!(
        "opened {} ({} pages, {} bytes {}) load {:.1} ms, open {:.1} ms",
        opened.path.display(),
        opened.doc.page_count(),
        opened.file_bytes,
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
/// (`crate::startup`) and where it starts (`crate::reading`).
pub(crate) struct Opened {
    pub doc: OpenedDocument,
    pub session: Option<DocumentSession<TileImage>>,
    /// What the reading positions said; `None` when they were not asked.
    pub start: Option<ReadingStart>,
}

impl fmt::Debug for Opened {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Opened")
            .field("doc", &self.doc)
            .field("early_session", &self.session.is_some())
            .field("start", &self.start)
            .finish()
    }
}

impl From<OpenedDocument> for Opened {
    fn from(doc: OpenedDocument) -> Self {
        Self {
            doc,
            session: None,
            start: None,
        }
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
        Self::spawn_with(engine, path, None, None)
    }

    /// Starts opening `path` on a new thread; with `positions`, it starts
    /// where it was left. With `early`, the thread also creates the
    /// document's session for the predicted view and asks it for a frame,
    /// so the first page shown renders while GPUI starts.
    pub(crate) fn spawn_with(
        engine: Arc<dyn PdfEngine>,
        path: PathBuf,
        early: Option<(SessionParts, ViewGuess)>,
        positions: Option<Arc<PositionStore>>,
    ) -> Self {
        let (tx, rx) = oneshot::channel();
        let thread_path = path.clone();
        let spawned = std::thread::Builder::new()
            .name("fastpdf-open".into())
            .spawn(move || {
                let result = crate::reading::open_for_reading(
                    engine.as_ref(),
                    &thread_path,
                    positions.as_deref(),
                    early,
                );
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

#[cfg(test)]
mod tests {
    use fastpdf_engine_api::{
        CancelToken, EngineCapabilities, EngineInfo, PageInfo, PageSize, PixmapMut, RenderOutcome,
        RenderRequest, Rotation, SharedBytes,
    };

    use super::*;

    /// One letter page; bytes that start with "locked" need the password "pw".
    struct Engine;
    struct Doc;

    impl PdfEngine for Engine {
        fn info(&self) -> EngineInfo {
            EngineInfo {
                name: "test",
                version: "0",
                capabilities: EngineCapabilities::default(),
            }
        }

        fn open(
            &self,
            source: DocumentSource,
            options: &OpenOptions,
        ) -> Result<Box<dyn EngineDocument>, EngineError> {
            if source.data.as_slice().starts_with(b"locked") {
                match options.password.as_ref().map(Password::as_str) {
                    None | Some("") => return Err(EngineError::PasswordRequired),
                    Some("pw") => {}
                    Some(_) => return Err(EngineError::InvalidPassword),
                }
            }
            Ok(Box::new(Doc))
        }
    }

    impl EngineDocument for Doc {
        fn page_count(&self) -> u32 {
            1
        }

        fn page_info(&self, _: PageIndex) -> Result<PageInfo, EngineError> {
            Ok(PageInfo {
                size: PageSize::LETTER,
                rotation: Rotation::R0,
            })
        }

        fn render(
            &self,
            _: &RenderRequest,
            _: &mut PixmapMut<'_>,
            _: &CancelToken,
        ) -> Result<RenderOutcome, EngineError> {
            Ok(RenderOutcome::default())
        }
    }

    fn loaded(bytes: &[u8]) -> LockedSource {
        LockedSource {
            path: PathBuf::from("test.pdf"),
            source: DocumentSource::from_bytes(SharedBytes::from_vec(bytes.to_vec()))
                .with_path("test.pdf"),
            strategy: LoadStrategy::Read,
            file_bytes: bytes.len(),
            load_ms: 0.0,
            wrong_password: false,
            identity: None,
        }
    }

    fn locked(result: Result<OpenedDocument, OpenFailure>) -> Box<LockedSource> {
        match result {
            Err(OpenFailure::Locked(locked)) => locked,
            other => panic!("not locked: {other:?}"),
        }
    }

    #[test]
    fn encrypted_documents_stay_loaded_until_a_password_opens_them() {
        let first = locked(open_loaded(&Engine, loaded(b"locked"), None));
        assert!(!first.wrong_password);
        let bytes = first.source.data.as_slice().as_ptr();
        let second = locked(open_loaded(&Engine, *first, Some(Password::from("bad"))));
        assert!(second.wrong_password);
        // The same bytes: the file is not read again for the next attempt.
        assert_eq!(second.source.data.as_slice().as_ptr(), bytes);
        let opened = open_loaded(&Engine, *second, Some(Password::from("pw"))).unwrap();
        assert!(opened.password_protected);
        assert_eq!(opened.doc.page_count(), 1);
        assert_eq!(opened.path, PathBuf::from("test.pdf"));
    }

    #[test]
    fn plain_documents_open_without_a_password() {
        let opened = open_loaded(&Engine, loaded(b"plain"), None).unwrap();
        assert!(!opened.password_protected);
    }

    #[test]
    fn locked_failures_never_show_the_password() {
        let wrong = open_loaded(
            &Engine,
            loaded(b"locked"),
            Some(Password::from("zq9-secret")),
        )
        .unwrap_err();
        let text = format!("{wrong} {wrong:?}");
        assert!(!text.contains("zq9"), "{text}");
        assert_eq!(wrong.to_string(), "the password did not open the document");
        let needed = open_loaded(&Engine, loaded(b"locked"), None).unwrap_err();
        assert_eq!(needed.to_string(), "a password is needed");
    }
}
