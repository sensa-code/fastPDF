//! Per-page verdict computed before a page is rendered for the first time.
//!
//! 1. A static scan of the objects the page reaches (`scan.rs`): nesting
//!    depth, declared image sizes, Flate decompression bombs.
//! 2. A budgeted interpretation pass with a counting device that mirrors the
//!    work hayro's renderer will do (tiling patterns, soft masks and Type3
//!    glyphs are interpreted too). Form-XObject DAGs that fan out
//!    exponentially (a few KB can mean minutes of interpretation) exhaust the
//!    budget here, so the real render is never started.
//!
//! hayro has no cancellation hook, so the budget aborts the interpreter by
//! unwinding with a private payload (`resume_unwind`, which skips the panic
//! hook) that is caught right here. This requires `panic = "unwind"`, which
//! FastPDF mandates anyway (ADR 0002).

use std::collections::HashSet;
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::time::{Duration, Instant};

use fastpdf_engine_api::{CancelToken, EngineError, LimitKind, ResourceLimits};
use hayro::hayro_interpret::font::{Glyph, GlyphRun};
use hayro::hayro_interpret::pattern::Pattern;
use hayro::hayro_interpret::util::TransformExt;
use hayro::hayro_interpret::{
    BlendMode, ClipPath, Context, Device, DrawMode, DrawProps, Image, ImageDrawProps,
    InterpreterCache, InterpreterSettings, Paint, SoftMask, interpret_page,
};
use hayro::hayro_syntax::object::{Dict, ObjectIdentifier};
use hayro::hayro_syntax::page::Page;
use hayro::kurbo::{Affine, BezPath, Rect};

use crate::document::DocInner;
use crate::fonts;
use crate::scan::{self, ScanError};

/// Device calls allowed in one interpretation pass, independent of time, so
/// that runaway pages are stopped deterministically even on fast machines.
pub(crate) const MAX_DEVICE_OPS: u64 = 50_000_000;
/// The deadline is checked every this many device calls.
const CHECK_INTERVAL: u64 = 256;

/// Outcome of the checks for one page; cached for the document's lifetime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Verdict {
    Ok,
    Limit(LimitKind),
}

impl Verdict {
    pub(crate) fn into_result(self) -> Result<(), EngineError> {
        match self {
            Self::Ok => Ok(()),
            Self::Limit(kind) => Err(EngineError::LimitExceeded(kind)),
        }
    }
}

/// Why an interpretation pass was aborted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Abort {
    Budget,
    Cancelled,
}

/// Work budget for one interpretation pass.
#[derive(Debug)]
pub(crate) struct Budget {
    deadline: Option<Instant>,
    cancel: Option<CancelToken>,
    ops: u64,
    next_check: u64,
}

impl Budget {
    pub(crate) fn new(time: Option<Duration>, cancel: Option<CancelToken>) -> Self {
        Self {
            deadline: time.and_then(|t| Instant::now().checked_add(t)),
            cancel,
            ops: 0,
            next_check: CHECK_INTERVAL,
        }
    }

    /// Accounts `n` device operations; unwinds out of the interpreter once
    /// the budget is exhausted or the request was cancelled.
    pub(crate) fn tick(&mut self, n: u64) {
        self.ops = self.ops.saturating_add(n);
        if self.ops > MAX_DEVICE_OPS {
            resume_unwind(Box::new(Abort::Budget));
        }
        if self.ops >= self.next_check {
            self.next_check = self.ops.saturating_add(CHECK_INTERVAL);
            if self.cancel.as_ref().is_some_and(CancelToken::is_cancelled) {
                resume_unwind(Box::new(Abort::Cancelled));
            }
            if self.deadline.is_some_and(|d| Instant::now() >= d) {
                resume_unwind(Box::new(Abort::Budget));
            }
        }
    }
}

/// Runs `f`, converting a budget abort raised inside it into `Err`. Real
/// panics keep unwinding to the caller's panic boundary.
pub(crate) fn run_budgeted<T>(f: impl FnOnce() -> T) -> Result<T, Abort> {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(v) => Ok(v),
        Err(payload) => match payload.downcast::<Abort>() {
            Ok(abort) => Err(*abort),
            Err(other) => resume_unwind(other),
        },
    }
}

/// Interpretation budget for the pre-flight pass: half of the per-render
/// deadline, so a page that passes still has room for rasterization.
fn preflight_time(limits: &ResourceLimits) -> Option<Duration> {
    limits.max_render_time.map(|t| t / 2)
}

/// Returns the page's verdict, computing it on first use. Concurrent callers
/// for the same page wait for one computation; a cancelled computation is
/// not cached.
pub(crate) fn ensure_verdict<'a>(
    doc: &DocInner,
    index: u32,
    page: &'a Page<'a>,
    cache: &InterpreterCache<'a>,
    cancel: Option<&CancelToken>,
) -> Result<(), EngineError> {
    let Some(cell) = doc.verdicts.get(index as usize) else {
        return Err(EngineError::Internal("page index out of range".into()));
    };
    if let Some(v) = cell.get() {
        return v.into_result();
    }
    let _guard = doc.verdict_lock(index);
    if let Some(v) = cell.get() {
        return v.into_result();
    }
    let verdict = compute(doc, index, page, cache, cancel)?;
    let _ = cell.set(verdict);
    verdict.into_result()
}

fn compute<'a>(
    doc: &DocInner,
    index: u32,
    page: &'a Page<'a>,
    cache: &InterpreterCache<'a>,
    cancel: Option<&CancelToken>,
) -> Result<Verdict, EngineError> {
    let limits = &doc.limits;
    let (scan_on, interpret_on) = checks_enabled();
    let mut nested = true;
    if scan_on {
        let mut memo = doc.scan_memo();
        match scan::scan_page(page, limits, &mut memo, cancel) {
            Ok(summary) => {
                nested = summary.nested;
                doc.set_decode_estimate(index, summary.image_bytes);
            }
            Err(ScanError::Limit(kind)) => return Ok(Verdict::Limit(kind)),
            Err(ScanError::Cancelled) => return Err(EngineError::Cancelled),
        }
    }
    if !interpret_on {
        return Ok(Verdict::Ok);
    }
    // Without nested content the interpretation is one linear pass over the
    // (already bomb-checked) content stream: small streams cannot blow up,
    // so the second pass is skipped for them. Inline images are only
    // visible to the interpretation pass, so they keep it on.
    if !nested
        && let Some(content) = page.page_stream()
        && content.len() <= SIMPLE_CONTENT_BYTES
        && !has_inline_image(content)
    {
        return Ok(Verdict::Ok);
    }
    let settings = InterpreterSettings {
        font_resolver: fonts::resolver(),
        render_annotations: true,
        ..InterpreterSettings::default()
    };
    let (w, h) = page.render_dimensions();
    let bbox = Rect::new(0.0, 0.0, f64::from(w), f64::from(h));
    let mut device = PreflightDevice {
        budget: Budget::new(preflight_time(limits), cancel.cloned()),
        max_pixels: limits.max_decoded_image_pixels,
        violation: None,
        masks: HashSet::new(),
    };
    let result = run_budgeted(|| {
        let mut ctx = Context::new(
            page.initial_transform(true).to_kurbo(),
            bbox,
            cache,
            page.xref(),
            settings,
        );
        interpret_page(page, &mut ctx, &mut device);
    });
    match result {
        Ok(()) => Ok(device.violation.map_or(Verdict::Ok, Verdict::Limit)),
        Err(Abort::Budget) => Ok(Verdict::Limit(LimitKind::RenderTime)),
        Err(Abort::Cancelled) => Err(EngineError::Cancelled),
    }
}

/// Content streams up to this size skip the interpretation pass when the
/// page reaches no nested content.
const SIMPLE_CONTENT_BYTES: usize = 4 * 1024 * 1024;

/// True when the content stream may contain an inline image (`BI` token).
/// False positives (the bytes inside a string) only cost a pre-flight pass.
fn has_inline_image(content: &[u8]) -> bool {
    let delimiter = |b: u8| b.is_ascii_whitespace() || b == 0 || b"()<>[]{}/%".contains(&b);
    content.windows(2).enumerate().any(|(i, w)| {
        w == b"BI"
            && (i == 0 || delimiter(content[i - 1]))
            && content.get(i + 2).is_none_or(|b| delimiter(*b))
    })
}

/// Which guardrails run: (static scan, budgeted pre-interpretation).
///
/// Always both, except in builds with the `diagnostics` feature (enabled by
/// fastpdf-bench only), where `FASTPDF_HAYRO_PREFLIGHT=0` disables both and
/// `=scan` keeps only the static scan to measure the guardrails' cost.
/// Shipping builds cannot switch hostile-input protection off.
fn checks_enabled() -> (bool, bool) {
    #[cfg(feature = "diagnostics")]
    {
        static MODE: std::sync::OnceLock<(bool, bool)> = std::sync::OnceLock::new();
        *MODE.get_or_init(
            || match std::env::var("FASTPDF_HAYRO_PREFLIGHT").as_deref() {
                Ok("0") => (false, false),
                Ok("scan") => (true, false),
                _ => (true, true),
            },
        )
    }
    #[cfg(not(feature = "diagnostics"))]
    {
        (true, true)
    }
}

/// Counts the work of an interpretation and checks image sizes; draws
/// nothing.
struct PreflightDevice {
    budget: Budget,
    max_pixels: u64,
    violation: Option<LimitKind>,
    masks: HashSet<ObjectIdentifier>,
}

impl PreflightDevice {
    fn paint<'a>(&mut self, paint: &Paint<'a>, is_stroke: bool) {
        if let Paint::Pattern(pattern) = paint
            && let Pattern::Tiling(tiling) = pattern.as_ref()
        {
            // hayro's renderer interprets the pattern cell on every use.
            let _ = tiling.interpret(self, Affine::IDENTITY, is_stroke);
        }
    }

    fn mask<'a>(&mut self, mask: Option<&SoftMask<'a>>) {
        if let Some(mask) = mask
            && self.masks.insert(mask.id())
        {
            // hayro caches each soft mask once per render.
            mask.interpret(self);
        }
    }

    fn props<'a>(&mut self, props: &DrawProps<'a>, is_stroke: bool) {
        self.paint(&props.paint, is_stroke);
        self.mask(props.soft_mask.as_ref());
    }

    fn check_image(&mut self, image: &Image<'_, '_>) {
        let pixels = u64::from(image.width()).saturating_mul(u64::from(image.height()));
        if pixels > self.max_pixels {
            self.violation = Some(LimitKind::DecodedImage);
            return;
        }
        if let Image::Raster(raster) = image {
            let dict = raster.stream().dict();
            for key in [&b"SMask"[..], b"Mask"] {
                if let Some(mask) = dict.get::<Dict<'_>>(key)
                    && exceeds(&mask, self.max_pixels)
                {
                    self.violation = Some(LimitKind::DecodedImage);
                }
            }
        }
    }
}

fn exceeds(dict: &Dict<'_>, max_pixels: u64) -> bool {
    let w = u64::from(dict.get::<u32>(b"Width").unwrap_or(0));
    let h = u64::from(dict.get::<u32>(b"Height").unwrap_or(0));
    w.saturating_mul(h) > max_pixels
}

impl<'a> Device<'a> for PreflightDevice {
    fn draw_path(&mut self, _path: &BezPath, props: DrawProps<'a>, draw_mode: &DrawMode) {
        self.budget.tick(1);
        self.props(&props, matches!(draw_mode, DrawMode::Stroke(_)));
    }

    fn draw_rect(&mut self, _rect: &Rect, props: DrawProps<'a>, draw_mode: &DrawMode) {
        self.budget.tick(1);
        self.props(&props, matches!(draw_mode, DrawMode::Stroke(_)));
    }

    fn push_clip_path(&mut self, _clip_path: &ClipPath) {
        self.budget.tick(1);
    }

    fn push_clip_rect(&mut self, _rect: &Rect) {
        self.budget.tick(1);
    }

    fn push_transparency_group(
        &mut self,
        _opacity: f32,
        mask: Option<SoftMask<'a>>,
        _blend_mode: BlendMode,
    ) {
        self.budget.tick(1);
        self.mask(mask.as_ref());
    }

    fn draw_glyph_run(
        &mut self,
        glyph_run: &GlyphRun<'_, 'a>,
        props: DrawProps<'a>,
        draw_mode: &DrawMode,
    ) {
        let glyphs = glyph_run.glyphs();
        self.budget.tick(glyphs.len() as u64);
        for glyph in glyphs {
            if let Glyph::Type3(type3) = &**glyph {
                type3.interpret(self, props.transform, glyph.transform(), &props.paint);
            }
        }
        self.props(&props, matches!(draw_mode, DrawMode::Stroke(_)));
    }

    fn draw_image(&mut self, image: Image<'a, '_>, props: ImageDrawProps<'a>) {
        self.budget.tick(1);
        self.check_image(&image);
        self.mask(props.soft_mask.as_ref());
    }

    fn pop_clip(&mut self) {
        self.budget.tick(1);
    }

    fn pop_transparency_group(&mut self) {
        self.budget.tick(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_aborts_by_operation_count() {
        let mut budget = Budget::new(None, None);
        let r = run_budgeted(|| budget.tick(MAX_DEVICE_OPS + 1));
        assert_eq!(r, Err(Abort::Budget));
    }

    #[test]
    fn budget_aborts_on_cancel_and_deadline() {
        let token = CancelToken::new();
        token.cancel();
        let mut budget = Budget::new(None, Some(token));
        assert_eq!(
            run_budgeted(|| budget.tick(CHECK_INTERVAL)),
            Err(Abort::Cancelled)
        );
        let mut budget = Budget::new(Some(Duration::ZERO), None);
        assert_eq!(
            run_budgeted(|| budget.tick(CHECK_INTERVAL)),
            Err(Abort::Budget)
        );
    }

    #[test]
    fn inline_image_tokens_are_found() {
        assert!(has_inline_image(b"q BI /W 1 /H 1 ID x EI Q"));
        assert!(has_inline_image(b"BI"));
        assert!(!has_inline_image(b"BT /F1 12 Tf (OBIT) Tj ET /BIG Do"));
    }

    #[test]
    fn real_panics_are_not_swallowed() {
        let r = catch_unwind(|| run_budgeted(|| panic!("boom")));
        assert!(r.is_err());
    }
}
