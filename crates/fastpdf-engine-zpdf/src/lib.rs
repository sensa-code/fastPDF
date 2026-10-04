//! FastPDF engine adapter for zpdf (pinned at commit `fe0ed23`, v0.14.0).
//!
//! # Model
//!
//! * **Open** copies the document bytes once into the `Arc<[u8]>` zpdf
//!   requires (see [`ZpdfEngine::open`] for the memory cost), parses the xref
//!   and walks the page tree (zpdf does both eagerly).
//! * **Interpretation is serialized.** zpdf's `PdfDocument` keeps its object
//!   and font caches in `RefCell`s and is `!Sync`, so it lives behind one
//!   mutex. A page is interpreted once into a display list ([`PreparedPage`])
//!   and kept in a small byte-weighted LRU keyed by page, user rotation and
//!   the annotation flag.
//! * **Rasterization is parallel.** Prepared pages are immutable and
//!   `Send + Sync`; each render call rasterizes only its region with zpdf's
//!   CPU backend (tiny-skia), skipping paint commands whose bounds miss the
//!   region, and polls the cancel token between commands.
//! * **Rotation** (`/Rotate` plus the user's rotation) is baked into the
//!   display list by zpdf; the adapter adds the CropBox-origin translation
//!   that zpdf's own callers omit (audit: content shifted off rotated pages
//!   whose visible box does not start at the origin).
//! * **Seamless tiles.** Each region is rasterized with a small margin that
//!   is then discarded, and one-pixel hairline segments that cross the tile
//!   grid are drawn 1.01 px wide, because tiny-skia's hairline algorithm
//!   places a clipped segment differently in differently clipped rasters
//!   (see `hairline.rs`).
//! * **Font fixes.** Unused WinAnsiEncoding codes render as bullets (zpdf
//!   drew ReportLab's list bullets as "ù") and the standard Times styles get
//!   their bold / italic faces on Windows (see `fonts.rs`).
//!
//! [`PreparedPage`]: prepare::PreparedPage

mod cache;
mod convert;
mod fonts;
mod hairline;
mod password;
mod prepare;

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use fastpdf_engine_api::{
    CancelToken, ColorMode, DocumentMetadata, DocumentSource, EngineCapabilities, EngineDocument,
    EngineError, EngineInfo, Link, MemoryPressure, OpenOptions, OutlineItem, PageIndex, PageInfo,
    PdfEngine, PixmapMut, RenderOutcome, RenderRequest, TextLayer,
};
use zpdf_core::{ParseLimits, PdfObject};
use zpdf_document::PdfDocument;

use crate::cache::{PreparedCache, PreparedKey};
use crate::convert::{PageGeometry, map_error};
use crate::password::PasswordProbe;
use crate::prepare::{DocState, PreparedPage, RasterParams};

/// zpdf release and the exact commit this adapter is built and tested against.
const ZPDF_VERSION: &str = "0.14.0+fe0ed23";

/// Interpreted pages kept per document.
const PREPARED_PAGES: usize = 8;
/// Retained bytes of interpreted pages (display lists + decoded images) per
/// document; the page being rendered is always kept even if larger.
const PREPARED_BYTES: u64 = 256 * 1024 * 1024;

const CAPABILITIES: EngineCapabilities = EngineCapabilities {
    region_render: true,
    // Several threads can rasterize (different tiles of) the same document at
    // once; only interpretation of a not-yet-prepared page is serialized.
    parallel_render: true,
    cooperative_cancel: true,
    // Span-level only: no per-character boxes (`TextSpan::char_bounds` empty).
    text_extraction: true,
    outline: true,
    links: true,
    // Standard security handler: RC4 40/128, AES-128, AES-256 (R5/R6).
    encryption: true,
    gpu: false,
};

/// The zpdf engine.
#[derive(Debug, Default, Clone, Copy)]
pub struct ZpdfEngine;

impl ZpdfEngine {
    pub fn new() -> Self {
        Self
    }
}

impl PdfEngine for ZpdfEngine {
    fn info(&self) -> EngineInfo {
        EngineInfo {
            name: "zpdf",
            version: ZPDF_VERSION,
            capabilities: CAPABILITIES,
        }
    }

    /// Opens a document.
    ///
    /// **Memory:** zpdf only accepts an owned `Arc<[u8]>`, so the bytes of
    /// `source` (possibly a memory map) are copied once into a private
    /// allocation of the file's size, kept for the document's lifetime. The
    /// copy goes straight from the source slice into the `Arc` allocation, so
    /// the transient peak is the source plus one copy (passing a `Vec` to zpdf
    /// would add a second copy). If the caller keeps its own `SharedBytes`
    /// alive (e.g. for a fallback engine), the file is resident twice.
    fn open(
        &self,
        source: DocumentSource,
        options: &OpenOptions,
    ) -> Result<Box<dyn EngineDocument>, EngineError> {
        let bytes: Arc<[u8]> = Arc::from(source.data.as_slice());
        drop(source);
        let document = ZpdfDocument::open(bytes, options)?;
        Ok(Box::new(document))
    }
}

/// An open zpdf document.
pub struct ZpdfDocument {
    /// Shared with zpdf's `PdfFile` (no extra copy); kept to reopen the
    /// document when memory pressure asks for its caches to be dropped.
    bytes: Arc<[u8]>,
    password: Vec<u8>,
    limits: ParseLimits,
    render_budget: Option<Duration>,
    page_count: u32,
    /// Lock order: `state` before `prepared` / `geometry`. The fast paths take
    /// only `prepared` or `geometry`.
    state: Mutex<DocState>,
    prepared: Mutex<PreparedCache<PreparedPage>>,
    geometry: Mutex<HashMap<u32, PageGeometry>>,
}

impl std::fmt::Debug for ZpdfDocument {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ZpdfDocument")
            .field("bytes", &self.bytes.len())
            .field("page_count", &self.page_count)
            .finish_non_exhaustive()
    }
}

impl ZpdfDocument {
    fn open(bytes: Arc<[u8]>, options: &OpenOptions) -> Result<Self, EngineError> {
        let limits = convert::parse_limits(&options.limits);
        let password = options.password.as_deref();
        let doc = open_checked(&bytes, password, &limits)?;
        let page_count = u32::try_from(doc.page_count())
            .map_err(|_| EngineError::LimitExceeded(fastpdf_engine_api::LimitKind::PageCount))?;
        Ok(Self {
            bytes,
            password: password.unwrap_or_default().as_bytes().to_vec(),
            limits,
            render_budget: options.limits.max_render_time,
            page_count,
            state: Mutex::new(DocState::new(doc)),
            prepared: Mutex::new(PreparedCache::new(PREPARED_PAGES, PREPARED_BYTES)),
            geometry: Mutex::new(HashMap::new()),
        })
    }

    /// The interpretation state. A panic while interpreting poisons the lock;
    /// zpdf's caches may then be half-updated, so the document is reopened
    /// from its bytes instead of trusting them.
    fn state(&self) -> MutexGuard<'_, DocState> {
        match self.state.lock() {
            Ok(guard) => guard,
            Err(poisoned) => {
                let mut guard = poisoned.into_inner();
                if let Ok(doc) = PdfDocument::open_with_password_and_limits(
                    Arc::clone(&self.bytes),
                    &self.password,
                    self.limits.clone(),
                ) {
                    *guard = DocState::new(doc);
                }
                self.state.clear_poison();
                guard
            }
        }
    }

    fn page_geometry(&self, page: u32) -> Result<PageGeometry, EngineError> {
        if let Some(g) = lock(&self.geometry).get(&page) {
            return Ok(*g);
        }
        let g = self.state().geometry(page)?;
        lock(&self.geometry).insert(page, g);
        Ok(g)
    }

    /// The interpreted page for `key`, interpreting it at most once even when
    /// several render threads ask for it at the same time.
    fn prepared(
        &self,
        key: PreparedKey,
        user_rotation: fastpdf_engine_api::Rotation,
        cancel: &CancelToken,
    ) -> Result<Arc<PreparedPage>, EngineError> {
        if let Some(page) = lock(&self.prepared).get(key) {
            return Ok(page);
        }
        let mut state = self.state();
        // Another thread may have prepared it while this one waited.
        if let Some(page) = lock(&self.prepared).get(key) {
            return Ok(page);
        }
        // Interpretation cannot be interrupted inside zpdf; at least do not
        // start one for a request that is already stale.
        cancel.check()?;
        let page = Arc::new(state.prepare(key.page, user_rotation, key.annotations)?);
        lock(&self.geometry).insert(key.page, page.geometry);
        lock(&self.prepared).insert(key, Arc::clone(&page));
        Ok(page)
    }
}

/// Opens `bytes` with zpdf and turns its lenient password handling into the
/// errors FastPDF expects.
fn open_checked(
    bytes: &Arc<[u8]>,
    password: Option<&str>,
    limits: &ParseLimits,
) -> Result<PdfDocument, EngineError> {
    let probe = PasswordProbe::default();
    let opened = probe.observe(|| {
        PdfDocument::open_with_password_and_limits(
            Arc::clone(bytes),
            password.unwrap_or_default().as_bytes(),
            limits.clone(),
        )
    });
    let missing = |supplied: Option<&str>| {
        if supplied.is_some_and(|p| !p.is_empty()) {
            EngineError::InvalidPassword
        } else {
            EngineError::PasswordRequired
        }
    };
    let doc = match opened {
        Ok(doc) => doc,
        Err(zpdf_core::Error::WrongPassword) => return Err(missing(password)),
        Err(e) => return Err(map_error(e)),
    };
    if doc.is_encrypted() {
        match security_handler(&doc) {
            // Unreadable /Encrypt: zpdf ignores it and so do we.
            None => {}
            Some(handler) if handler != "Standard" => {
                return Err(EngineError::Unsupported(format!(
                    "security handler /{handler}"
                )));
            }
            // AES-256 (V5) whose password did not validate: zpdf opens it
            // without a decryptor, i.e. undecrypted.
            Some(_) if doc.file().decryptor().is_none() => return Err(missing(password)),
            // RC4 / AES-128 (V <= 4) opened with a key zpdf could not validate.
            Some(_) if probe.key_unverified() && password.is_none_or(str::is_empty) => {
                return Err(EngineError::PasswordRequired);
            }
            Some(_) => {}
        }
    }
    Ok(doc)
}

/// `/Filter` of the trailer's `/Encrypt` dictionary, when readable.
fn security_handler(doc: &PdfDocument) -> Option<String> {
    let file = doc.file();
    let encrypt = match file.trailer.get("Encrypt")? {
        PdfObject::Ref(r) => file.resolve(*r).ok()?,
        direct => direct.clone(),
    };
    let dict = encrypt.as_dict().ok()?;
    Some(dict.get_name("Filter").ok()?.to_owned())
}

impl EngineDocument for ZpdfDocument {
    fn page_count(&self) -> u32 {
        self.page_count
    }

    fn page_info(&self, page: PageIndex) -> Result<PageInfo, EngineError> {
        let g = self.page_geometry(page.get())?;
        Ok(PageInfo {
            size: g.size(),
            rotation: g.rotation,
        })
    }

    fn metadata(&self) -> Result<DocumentMetadata, EngineError> {
        let state = self.state();
        let doc = &state.doc;
        let info = doc.info().unwrap_or_default();
        let (major, minor) = doc.version();
        Ok(DocumentMetadata {
            title: info.title,
            author: info.author,
            subject: info.subject,
            keywords: info.keywords,
            creator: info.creator,
            producer: info.producer,
            creation_date: info.creation_date,
            modification_date: info.mod_date,
            pdf_version: Some(format!("{major}.{minor}")),
            encrypted: doc.is_encrypted(),
        })
    }

    fn render(
        &self,
        request: &RenderRequest,
        target: &mut PixmapMut<'_>,
        cancel: &CancelToken,
    ) -> Result<RenderOutcome, EngineError> {
        match request.color_mode {
            ColorMode::Normal => {}
            other => {
                return Err(EngineError::Unsupported(format!("color mode {other:?}")));
            }
        }
        cancel.check()?;
        let key = PreparedKey {
            page: request.page.get(),
            user_quarter_turns: request.rotation.quarter_turns(),
            annotations: request.annotations,
        };
        let page = self.prepared(key, request.rotation, cancel)?;
        cancel.check()?;
        let partial = page.rasterize(
            &RasterParams {
                region: request.region,
                scale: request.scale.get(),
                background: request.background,
                limits: &self.limits,
                render_budget: self.render_budget,
                page_pixels: request.scale.page_pixels(
                    page.geometry.size(),
                    page.geometry.rotation.then(request.rotation),
                ),
            },
            target,
            cancel,
        )?;
        Ok(RenderOutcome { partial })
    }

    fn text_layer(&self, page: PageIndex, cancel: &CancelToken) -> Result<TextLayer, EngineError> {
        let (geometry, spans) = {
            let mut state = self.state();
            cancel.check()?;
            state.text(page.get())?
        };
        Ok(TextLayer {
            page,
            spans: spans
                .iter()
                .filter(|s| !s.text.is_empty())
                .map(|s| convert::text_span(s, &geometry))
                .collect(),
        })
    }

    fn outline(&self) -> Result<Vec<OutlineItem>, EngineError> {
        let items = self.state().doc.outline();
        Ok(convert::outline(&items, &|p| self.page_geometry(p).ok()))
    }

    fn links(&self, page: PageIndex) -> Result<Vec<Link>, EngineError> {
        let (geometry, annotations) = {
            let state = self.state();
            let pdf_page = state.doc.page(page.as_usize()).map_err(map_error)?;
            let geometry = PageGeometry::new(pdf_page.effective_box(), pdf_page.rotate);
            (geometry, state.doc.page_annotations(&pdf_page))
        };
        Ok(convert::links(&annotations, &geometry, &|p| {
            self.page_geometry(p).ok()
        }))
    }

    /// The adapter's copy of the file, the interpreted pages in the LRU
    /// (display lists, decoded images, hairline replacements) and the font
    /// programs they use, each counted once. zpdf's own object, object-stream
    /// and shared-font caches cannot be observed from outside and are left
    /// out, so this is a lower bound. Never waits for an interpretation.
    fn memory_usage(&self) -> Option<u64> {
        let prepared = lock(&self.prepared);
        let mut fonts = HashSet::new();
        let font_bytes: u64 = prepared
            .values()
            .flat_map(|page| page.font_programs())
            .filter(|(identity, _)| fonts.insert(*identity))
            .map(|(_, bytes)| bytes)
            .sum();
        Some(
            (self.bytes.len() as u64)
                .saturating_add(prepared.bytes())
                .saturating_add(font_bytes),
        )
    }

    fn trim_memory(&self, pressure: MemoryPressure) {
        match pressure {
            MemoryPressure::Normal => {}
            MemoryPressure::Soft => lock(&self.prepared).clear(),
            MemoryPressure::Hard => {
                lock(&self.prepared).clear();
                // zpdf's object, object-stream and font caches can only be
                // released by dropping the document; reopening from the shared
                // bytes is cheap (xref + page tree) and keeps the file copy.
                let mut state = self.state();
                if let Ok(doc) = PdfDocument::open_with_password_and_limits(
                    Arc::clone(&self.bytes),
                    &self.password,
                    self.limits.clone(),
                ) {
                    *state = DocState::new(doc);
                }
            }
        }
    }
}

/// Locks a cache mutex. Poisoning only means a panic happened elsewhere while
/// it was held; the caches hold complete entries only, so keep using them.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn document_is_shareable_across_render_threads() {
        assert_send_sync::<ZpdfDocument>();
        assert_send_sync::<PreparedPage>();
    }

    #[test]
    fn info_is_honest() {
        let info = ZpdfEngine::new().info();
        assert_eq!(info.name, "zpdf");
        assert_eq!(info.version, "0.14.0+fe0ed23");
        assert!(info.capabilities.region_render);
        assert!(!info.capabilities.gpu);
    }
}
