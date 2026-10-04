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

mod document;
mod fonts;
mod geometry;
mod nav;
mod pool;
mod preflight;
mod render;
mod scan;
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
