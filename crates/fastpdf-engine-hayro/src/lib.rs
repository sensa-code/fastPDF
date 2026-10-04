//! FastPDF engine adapter for [hayro](https://github.com/LaurenzV/hayro)
//! (pure-Rust PDF interpreter + vello_cpu rasterizer). See
//! `docs/audit/hayro.md` for the audit this adapter is based on.
//!
//! # Threading and memory model
//!
//! * `Pdf` is `Send + Sync`; one instance per open document, shared by all
//!   threads. hayro caches decoded content/object streams inside it, so the
//!   adapter reopens it (same bytes, a few ms) when those caches pass a
//!   budget or under hard memory pressure.
//! * hayro's `RenderCache`/`InterpreterCache` are `!Send` and borrow the
//!   `Pdf`; they live in a per-document pool of render threads (`pool.rs`)
//!   with large stacks. Callers block on a job hand-off.
//! * Tiles are cut from merged blocks (`render.rs`) so a page is interpreted
//!   once per 2048-px block instead of once per tile.
//!
//! # Guardrails (spec §25)
//!
//! Before a page is interpreted for the first time it gets a verdict
//! (`preflight.rs`): a static scan of the objects it reaches (nesting depth,
//! declared image sizes, Flate bombs) and a budgeted interpretation pass
//! (exponential Form-XObject fan-out). Region sizes are capped below what
//! vello_cpu can allocate without panicking. What cannot be stopped at this
//! layer is listed in `docs/audit/hayro.md`; process isolation is the
//! remaining defense.

mod decode;
mod document;
mod fonts;
mod geometry;
mod nav;
mod pool;
mod preflight;
mod render;
mod scan;
mod stats;
mod text;

use fastpdf_engine_api::{
    DocumentSource, EngineCapabilities, EngineDocument, EngineError, EngineInfo, OpenOptions,
    PdfEngine,
};

use crate::document::{DocInner, HayroDocument, open_pdf};

/// The pinned hayro commit (see this crate's `Cargo.toml`).
pub const HAYRO_REVISION: &str = "ced00dd082e6a7eda8561d4ac0f7fc3828af2ac7";

/// Engine version reported to FastPDF: hayro's version line plus the commit.
const VERSION: &str = "0.7.x+ced00dd0";

/// The hayro engine.
#[derive(Debug, Default, Clone, Copy)]
pub struct HayroEngine;

impl HayroEngine {
    pub fn new() -> Self {
        Self
    }
}

impl PdfEngine for HayroEngine {
    fn info(&self) -> EngineInfo {
        EngineInfo {
            name: "hayro",
            version: VERSION,
            capabilities: EngineCapabilities {
                region_render: true,
                parallel_render: true,
                // The pre-flight pass and the waits between pipeline stages
                // poll the token, but hayro's `render_into` itself cannot be
                // interrupted.
                cooperative_cancel: false,
                text_extraction: true,
                outline: true,
                links: true,
                encryption: true,
                gpu: false,
            },
        }
    }

    fn open(
        &self,
        source: DocumentSource,
        options: &OpenOptions,
    ) -> Result<Box<dyn EngineDocument>, EngineError> {
        let password = options.password.clone().unwrap_or_default();
        let pdf = open_pdf(&source.data, &password)?;
        let inner = DocInner::new(source.data, password, options.limits.clone(), pdf);
        Ok(Box::new(HayroDocument::new(inner)))
    }
}

/// Diagnostics for tests and benchmarks; not part of the engine contract.
#[doc(hidden)]
pub mod diagnostics {
    use std::sync::atomic::Ordering;

    /// Block-cache budget of one document, in bytes.
    pub const BLOCK_CACHE_BUDGET: u64 = crate::render::BLOCK_CACHE_BYTES as u64;
    /// Image-decode working memory all renders of the process may use at once.
    pub const DECODE_BUDGET: u64 = crate::decode::BUDGET;
    /// Decoded content-stream bytes after which a document reopens its `Pdf`.
    pub const CONTENT_STREAM_BUDGET: u64 = crate::document::CONTENT_STREAM_BUDGET;

    /// Memory the adapter holds outside FastPDF's budgeted caches, summed
    /// over all open documents of this process.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct MemoryCounters {
        /// Finished blocks kept for neighbouring tiles.
        pub block_cache_bytes: u64,
        /// Content streams hayro keeps decoded inside live `Pdf` generations.
        pub decoded_content_bytes: u64,
        /// Documents whose memory is not completely freed yet.
        pub live_documents: u64,
        pub live_generations: u64,
        /// Generations replaced since process start (content budget, hard
        /// memory pressure, panics).
        pub reopens: u64,
        pub render_threads: u64,
        /// Render threads executing a job.
        pub busy_threads: u64,
        /// `trim_memory` calls since process start.
        pub soft_trims: u64,
        pub hard_trims: u64,
        /// Estimated buffers of the pool threads' render contexts.
        pub context_bytes: u64,
        /// System font files mapped by the font resolver (file-backed and
        /// shared, so not part of `EngineDocument::memory_usage`).
        pub mapped_font_bytes: u64,
        /// Decode budget held by running renders (estimated bytes).
        pub decode_bytes: u64,
        /// Renders admitted by the decode budget, and those that waited.
        pub decode_admissions: u64,
        pub decode_waits: u64,
    }

    pub fn memory() -> MemoryCounters {
        let c = crate::stats::counters();
        MemoryCounters {
            block_cache_bytes: c.block_bytes.load(Ordering::Relaxed),
            decoded_content_bytes: c.content_bytes.load(Ordering::Relaxed),
            live_documents: c.documents.load(Ordering::Relaxed),
            live_generations: c.generations.load(Ordering::Relaxed),
            reopens: c.reopens.load(Ordering::Relaxed),
            render_threads: c.threads.load(Ordering::Relaxed),
            busy_threads: c.busy_threads.load(Ordering::Relaxed),
            soft_trims: c.soft_trims.load(Ordering::Relaxed),
            hard_trims: c.hard_trims.load(Ordering::Relaxed),
            context_bytes: c.context_bytes.load(Ordering::Relaxed),
            mapped_font_bytes: crate::fonts::mapped_bytes(),
            decode_bytes: c.decode_bytes.load(Ordering::Relaxed),
            decode_admissions: c.decode_admissions.load(Ordering::Relaxed),
            decode_waits: c.decode_waits.load(Ordering::Relaxed),
        }
    }

    /// `(system font hits, CJK font hits, embedded fallbacks, misses)` since
    /// process start.
    pub fn font_counters() -> (u64, u64, u64, u64) {
        let s = crate::fonts::stats();
        (
            s.system_hits.load(Ordering::Relaxed),
            s.cjk_hits.load(Ordering::Relaxed),
            s.embedded_fallbacks.load(Ordering::Relaxed),
            s.misses.load(Ordering::Relaxed),
        )
    }
}
