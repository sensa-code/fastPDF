//! The opened document: shared state, reopen generations and memory trimming.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, RwLock};

use fastpdf_engine_api::{
    CancelToken, DocumentMetadata, EngineDocument, EngineError, Link, MemoryPressure, OutlineItem,
    PageIndex, PageInfo, PixmapMut, RenderOutcome, RenderRequest, ResourceLimits, SharedBytes,
    TextLayer,
};
use hayro::hayro_syntax::object::ObjectIdentifier;
use hayro::hayro_syntax::{DecryptionError, LoadPdfError, Pdf};

use crate::geometry::PageGeom;
use crate::nav;
use crate::pool::{Pool, TextJob};
use crate::preflight::Verdict;
use crate::render::{self, BlockCache};
use crate::scan::ScanMemo;

/// Striped locks that serialize the verdict computation of one page.
const VERDICT_LOCKS: usize = 16;
/// Decoded content streams hayro may keep (it caches them per page for the
/// lifetime of the `Pdf`) before the adapter reopens the document to drop
/// them. Reopening costs a few milliseconds even for 2000 pages.
const CONTENT_STREAM_BUDGET: u64 = 64 * 1024 * 1024;

/// Opens `bytes` with hayro, mapping its errors onto [`EngineError`].
pub(crate) fn open_pdf(bytes: &SharedBytes, password: &str) -> Result<Pdf, EngineError> {
    // `PdfData` needs a sized owner; wrapping the (already shared) bytes in
    // one more Arc keeps the open zero-copy, mmap included.
    match Pdf::new_with_password(Arc::new(bytes.clone()), password) {
        Ok(pdf) => Ok(pdf),
        Err(LoadPdfError::Decryption(DecryptionError::PasswordProtected)) => {
            if password.is_empty() {
                Err(EngineError::PasswordRequired)
            } else {
                Err(EngineError::InvalidPassword)
            }
        }
        Err(LoadPdfError::Decryption(DecryptionError::UnsupportedAlgorithm)) => Err(
            EngineError::Unsupported("encryption handler or algorithm".into()),
        ),
        Err(LoadPdfError::Decryption(DecryptionError::InvalidEncryption)) => Err(
            EngineError::Malformed("invalid encryption dictionary".into()),
        ),
        Err(LoadPdfError::Decryption(DecryptionError::MissingIDEntry)) => Err(
            EngineError::Malformed("encrypted document without /ID".into()),
        ),
        Err(LoadPdfError::Invalid) => Err(EngineError::Malformed(
            "no usable cross-reference table, catalog or page tree".into(),
        )),
    }
}

/// One opened `Pdf`. hayro caches decoded content and object streams inside
/// it forever, so the adapter replaces it (same bytes, new generation) when
/// those caches grow past their budget or under hard memory pressure.
pub(crate) struct Generation {
    pub(crate) id: u64,
    pub(crate) pdf: Pdf,
    content: Mutex<ContentAccount>,
}

#[derive(Default)]
struct ContentAccount {
    pages: HashSet<u32>,
    bytes: u64,
}

impl std::fmt::Debug for Generation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Generation").field("id", &self.id).finish()
    }
}

/// State shared by the document handle, the render pool and its threads.
pub(crate) struct DocInner {
    pub(crate) bytes: SharedBytes,
    pub(crate) password: String,
    pub(crate) limits: ResourceLimits,
    pub(crate) encrypted: bool,
    page_count: u32,
    current: RwLock<Arc<Generation>>,
    next_generation: AtomicU64,
    regenerate: AtomicBool,
    regenerate_lock: Mutex<()>,
    /// Bumped to make every pool thread drop its hayro caches.
    cache_epoch: AtomicU64,
    pub(crate) verdicts: Vec<OnceLock<Verdict>>,
    verdict_locks: Vec<Mutex<()>>,
    scan_memo: Mutex<ScanMemo>,
    pub(crate) blocks: BlockCache,
    page_ids: OnceLock<HashMap<ObjectIdentifier, u32>>,
}

impl std::fmt::Debug for DocInner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DocInner")
            .field("page_count", &self.page_count)
            .field("encrypted", &self.encrypted)
            .finish_non_exhaustive()
    }
}

impl DocInner {
    pub(crate) fn new(
        bytes: SharedBytes,
        password: String,
        limits: ResourceLimits,
        pdf: Pdf,
    ) -> Self {
        let page_count = u32::try_from(pdf.pages().len()).unwrap_or(u32::MAX);
        // A password supplied for an unencrypted file is ignored by hayro, so
        // it says nothing about encryption.
        let encrypted = mentions_encrypt(bytes.as_slice());
        Self {
            bytes,
            password,
            limits,
            encrypted,
            page_count,
            current: RwLock::new(Arc::new(Generation {
                id: 0,
                pdf,
                content: Mutex::new(ContentAccount::default()),
            })),
            next_generation: AtomicU64::new(1),
            regenerate: AtomicBool::new(false),
            regenerate_lock: Mutex::new(()),
            cache_epoch: AtomicU64::new(0),
            verdicts: (0..page_count).map(|_| OnceLock::new()).collect(),
            verdict_locks: (0..VERDICT_LOCKS).map(|_| Mutex::new(())).collect(),
            scan_memo: Mutex::new(ScanMemo::default()),
            blocks: BlockCache::new(render::BLOCK_CACHE_BYTES),
            page_ids: OnceLock::new(),
        }
    }

    pub(crate) fn page_count(&self) -> u32 {
        self.page_count
    }

    /// The `Pdf` new work should use; reopens it first if a reopen was
    /// requested.
    pub(crate) fn current(&self) -> Arc<Generation> {
        if self.regenerate.load(Ordering::Acquire) {
            self.reopen();
        }
        Arc::clone(&read(&self.current))
    }

    pub(crate) fn current_id(&self) -> u64 {
        read(&self.current).id
    }

    pub(crate) fn cache_epoch(&self) -> u64 {
        self.cache_epoch.load(Ordering::Acquire)
    }

    pub(crate) fn bump_cache_epoch(&self) {
        self.cache_epoch.fetch_add(1, Ordering::AcqRel);
    }

    /// Asks for a fresh `Pdf` (same bytes) to drop hayro's internal caches,
    /// or to get rid of locks a panic might have poisoned.
    pub(crate) fn request_reopen(&self) {
        self.regenerate.store(true, Ordering::Release);
    }

    pub(crate) fn reopen_pending(&self) -> bool {
        self.regenerate.load(Ordering::Acquire)
    }

    fn reopen(&self) {
        let _guard = lock(&self.regenerate_lock);
        if !self.regenerate.swap(false, Ordering::AcqRel) {
            return; // another thread already reopened
        }
        // The bytes opened before, so this only fails if hayro changed its
        // mind (it does not); keep the old generation in that case.
        if let Ok(pdf) = open_pdf(&self.bytes, &self.password) {
            let id = self.next_generation.fetch_add(1, Ordering::Relaxed);
            *write(&self.current) = Arc::new(Generation {
                id,
                pdf,
                content: Mutex::new(ContentAccount::default()),
            });
        }
    }

    /// Records that `page`'s content stream is now decoded inside
    /// `generation` and schedules a reopen once the budget is exceeded.
    pub(crate) fn account_content(&self, generation: &Generation, page: u32, bytes: usize) {
        let mut account = lock(&generation.content);
        if account.pages.insert(page) {
            account.bytes = account.bytes.saturating_add(bytes as u64);
            if account.bytes > CONTENT_STREAM_BUDGET {
                self.request_reopen();
            }
        }
    }

    pub(crate) fn verdict_lock(&self, page: u32) -> MutexGuard<'_, ()> {
        let i = page as usize % self.verdict_locks.len().max(1);
        match self.verdict_locks.get(i) {
            Some(m) => lock(m),
            // Unreachable: the vector is never empty.
            None => lock(&self.regenerate_lock),
        }
    }

    pub(crate) fn scan_memo(&self) -> MutexGuard<'_, ScanMemo> {
        lock(&self.scan_memo)
    }

    pub(crate) fn page_geom(&self, page: PageIndex) -> Result<PageGeom, EngineError> {
        let generation = self.current();
        let pages = generation.pdf.pages();
        let p = pages
            .get(page.as_usize())
            .ok_or(EngineError::PageOutOfRange {
                page,
                page_count: self.page_count,
            })?;
        Ok(PageGeom::from_page(p))
    }

    /// Page object id → page index, for destinations. Built on first use.
    pub(crate) fn page_ids(&self, generation: &Generation) -> &HashMap<ObjectIdentifier, u32> {
        self.page_ids.get_or_init(|| {
            generation
                .pdf
                .pages()
                .iter()
                .enumerate()
                .filter_map(|(i, p)| Some((p.raw().obj_id()?, u32::try_from(i).ok()?)))
                .collect()
        })
    }
}

/// True when an `/Encrypt` key appears near the start or end of the file,
/// where trailers and cross-reference stream dictionaries live. hayro does
/// not expose whether a document was encrypted with an empty user password.
fn mentions_encrypt(bytes: &[u8]) -> bool {
    const WINDOW: usize = 64 * 1024;
    let head = &bytes[..bytes.len().min(WINDOW)];
    let tail = &bytes[bytes.len().saturating_sub(WINDOW)..];
    let needle = b"/Encrypt";
    head.windows(needle.len()).any(|w| w == needle)
        || tail.windows(needle.len()).any(|w| w == needle)
}

/// The [`EngineDocument`] handed to FastPDF.
pub(crate) struct HayroDocument {
    inner: Arc<DocInner>,
    pool: Pool,
}

impl std::fmt::Debug for HayroDocument {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HayroDocument")
            .field("inner", &self.inner)
            .finish_non_exhaustive()
    }
}

impl HayroDocument {
    pub(crate) fn new(inner: DocInner) -> Self {
        let inner = Arc::new(inner);
        let pool = Pool::new(Arc::clone(&inner));
        Self { inner, pool }
    }
}

impl Drop for HayroDocument {
    fn drop(&mut self) {
        self.pool.shutdown();
    }
}

impl EngineDocument for HayroDocument {
    fn page_count(&self) -> u32 {
        self.inner.page_count()
    }

    fn page_info(&self, page: PageIndex) -> Result<PageInfo, EngineError> {
        let geom = self.inner.page_geom(page)?;
        Ok(PageInfo {
            size: geom.page_size(),
            rotation: geom.rotation,
        })
    }

    fn metadata(&self) -> Result<DocumentMetadata, EngineError> {
        let generation = self.inner.current();
        Ok(nav::metadata(&generation.pdf, self.inner.encrypted))
    }

    fn render(
        &self,
        request: &RenderRequest,
        target: &mut PixmapMut<'_>,
        cancel: &CancelToken,
    ) -> Result<RenderOutcome, EngineError> {
        render::render(&self.inner, &self.pool, request, target, cancel)
    }

    fn text_layer(&self, page: PageIndex, cancel: &CancelToken) -> Result<TextLayer, EngineError> {
        if page.get() >= self.inner.page_count() {
            return Err(EngineError::PageOutOfRange {
                page,
                page_count: self.inner.page_count(),
            });
        }
        cancel.check()?;
        self.pool.text(TextJob {
            page: page.get(),
            cancel: cancel.clone(),
        })
    }

    fn outline(&self) -> Result<Vec<OutlineItem>, EngineError> {
        let generation = self.inner.current();
        Ok(nav::outline(&self.inner, &generation))
    }

    fn links(&self, page: PageIndex) -> Result<Vec<Link>, EngineError> {
        let generation = self.inner.current();
        nav::links(&self.inner, &generation, page)
    }

    fn trim_memory(&self, pressure: MemoryPressure) {
        match pressure {
            MemoryPressure::Normal => {}
            MemoryPressure::Soft => {
                // Rendered blocks and every thread's hayro caches (fonts,
                // glyph outlines, color spaces).
                self.inner.blocks.clear();
                self.inner.bump_cache_epoch();
                self.pool.release_idle_threads();
            }
            MemoryPressure::Hard => {
                // Additionally hayro's decoded content/object streams (by
                // reopening the `Pdf`) and the scan memo.
                self.inner.blocks.clear();
                self.inner.scan_memo().clear();
                self.inner.request_reopen();
                self.inner.bump_cache_epoch();
                self.pool.release_idle_threads();
            }
        }
    }
}

// Lock poisoning only means another thread panicked while holding the lock;
// the protected data stays valid, so recover the guard instead of panicking.
pub(crate) fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn read<T>(l: &RwLock<T>) -> std::sync::RwLockReadGuard<'_, T> {
    l.read().unwrap_or_else(|e| e.into_inner())
}

fn write<T>(l: &RwLock<T>) -> std::sync::RwLockWriteGuard<'_, T> {
    l.write().unwrap_or_else(|e| e.into_inner())
}
