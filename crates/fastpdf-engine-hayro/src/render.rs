//! Rendering: request validation, block merging and the hayro call.
//!
//! The scheduler asks for one tile per call. hayro re-interprets the whole
//! page on every `render_into` call (only rasterization is clipped to the
//! target), so rendering 512-px tiles one by one repeats the interpretation
//! for every tile (docs/audit/hayro.md, "Rendering Model"). The adapter
//! therefore renders aligned **blocks** of up to `BLOCK_SIDE` pixels once
//! and cuts tiles out of them: concurrent requests for tiles of the same
//! block wait for the first one instead of rendering it again, and finished
//! blocks stay in a small byte-budgeted LRU so neighbouring tiles (visible,
//! near-visible, prefetch) are memcpys. Requests that cover a whole block
//! (thumbnails, small full pages) are rendered directly.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::Duration;

use fastpdf_engine_api::{
    CancelToken, ColorMode, EngineError, LimitKind, PixelRect, PixelSize, PixmapMut, RenderOutcome,
    RenderRequest, RenderScale, ResourceLimits, Rgba8, Rotation,
};
use hayro::hayro_interpret::InterpreterSettings;
use hayro::vello_cpu::color::AlphaColor;
use hayro::vello_cpu::{RasterizerSettings, RenderContext, TargetInit};
use hayro::{RenderSettings, render_into};

use crate::document::{DocInner, Generation, lock};
use crate::fonts;
use crate::geometry::PageGeom;
use crate::pool::{Pool, ThreadCaches};
use crate::preflight;

/// Side of a merged render block in device pixels (16 MiB of RGBA).
pub(crate) const BLOCK_SIDE: u32 = 2048;
/// Finished blocks kept for neighbouring tiles.
pub(crate) const BLOCK_CACHE_BYTES: usize = 48 * 1024 * 1024;
/// vello_cpu panics for render targets wider or taller than 65 532 px
/// (`snap_to_tile_coordinates`); FastPDF never needs more than a D3D11
/// texture (16 384 px), so the adapter refuses anything larger regardless of
/// the limits it was given.
pub(crate) const MAX_TARGET_SIDE: u32 = 16_384;
/// How often a caller waiting for another thread's block re-checks its
/// cancel token.
const WAIT_POLL: Duration = Duration::from_millis(20);
/// Attempts to obtain a block whose owner got cancelled before giving up
/// and rendering directly.
const BLOCK_ATTEMPTS: usize = 3;

/// Work for a pool thread: render `rect` (pixel space of the page at
/// `scale` after rotation) into a new RGBA buffer.
#[derive(Debug, Clone)]
pub(crate) struct RenderJob {
    pub(crate) page: u32,
    pub(crate) scale: RenderScale,
    pub(crate) rotation: Rotation,
    pub(crate) rect: PixelRect,
    pub(crate) background: Rgba8,
    pub(crate) annotations: bool,
    /// `None` for shared blocks: other tiles wait for them, so the first
    /// requester's cancellation must not abort the work.
    pub(crate) cancel: Option<CancelToken>,
}

/// Premultiplied RGBA8, rows tightly packed.
#[derive(Debug)]
pub(crate) struct Rendered {
    pub(crate) rect: PixelRect,
    pub(crate) data: Vec<u8>,
    pub(crate) partial: bool,
}

/// Validates the region against the adapter's own hard limits. The guard
/// layer checks the configurable ones before calling the engine; this keeps
/// hayro/vello from ever seeing a size they panic on.
pub(crate) fn check_target(size: PixelSize, limits: &ResourceLimits) -> Result<(), EngineError> {
    if size.width > MAX_TARGET_SIDE || size.height > MAX_TARGET_SIDE {
        return Err(EngineError::LimitExceeded(LimitKind::BitmapDimension));
    }
    limits.check_bitmap(size)
}

pub(crate) fn render(
    doc: &Arc<DocInner>,
    pool: &Pool,
    request: &RenderRequest,
    target: &mut PixmapMut<'_>,
    cancel: &CancelToken,
) -> Result<RenderOutcome, EngineError> {
    cancel.check()?;
    let geom: PageGeom = doc.page_geom(request.page)?;
    let size = geom.page_size();
    doc.limits.check_page_size(size)?;
    let page_px = request
        .scale
        .page_pixels(size, geom.rotation.then(request.rotation));
    let region = request.region;
    if region.is_empty() || !region.is_within(page_px.bounds()) {
        return Err(EngineError::InvalidRequest(format!(
            "region {region:?} outside page bounds {:?}",
            page_px.bounds()
        )));
    }
    if target.size() != region.size() {
        return Err(EngineError::InvalidRequest(
            "target size does not match region size".into(),
        ));
    }
    check_target(region.size(), &doc.limits)?;

    let job = RenderJob {
        page: request.page.get(),
        scale: request.scale,
        rotation: request.rotation,
        rect: region,
        background: request.background,
        annotations: request.annotations,
        cancel: Some(cancel.clone()),
    };
    if let Some(block) = block_for(region, page_px, &doc.limits).filter(|_| blocks_enabled()) {
        let key = BlockKey::new(request, block);
        for _ in 0..BLOCK_ATTEMPTS {
            match render_block(doc, pool, key, &job, block, cancel)? {
                Some(rendered) => return copy_out(&rendered, region, target),
                // The block's owner was cancelled; try again.
                None => cancel.check()?,
            }
        }
    }
    let rendered = pool.render(job)?;
    copy_out(&rendered, region, target)
}

/// Block merging can be switched off with `FASTPDF_HAYRO_BLOCKS=0` to
/// measure its effect (benchmark diagnostics only).
fn blocks_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("FASTPDF_HAYRO_BLOCKS").is_none_or(|v| v != "0"))
}

/// Block side, overridable with `FASTPDF_HAYRO_BLOCK_SIDE` (benchmark
/// diagnostics only; clamped to 256..=4096).
fn block_side() -> u32 {
    static SIDE: OnceLock<u32> = OnceLock::new();
    *SIDE.get_or_init(|| {
        std::env::var("FASTPDF_HAYRO_BLOCK_SIDE")
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
            .map_or(BLOCK_SIDE, |v| v.clamp(256, 4096))
    })
}

/// The aligned block containing `region`, when merging helps: the region is
/// strictly smaller than its block and lies inside it.
fn block_for(region: PixelRect, page_px: PixelSize, limits: &ResourceLimits) -> Option<PixelRect> {
    let side = block_side();
    let bx = region.x / side * side;
    let by = region.y / side * side;
    let block = PixelRect::new(
        bx,
        by,
        side.min(page_px.width.saturating_sub(bx)),
        side.min(page_px.height.saturating_sub(by)),
    );
    (!block.is_empty()
        && region.is_within(block)
        && block != region
        && check_target(block.size(), limits).is_ok())
    .then_some(block)
}

/// Returns the block, rendering it if this caller is the first to ask.
/// `Ok(None)` when the thread that owned the block was cancelled first.
fn render_block(
    doc: &DocInner,
    pool: &Pool,
    key: BlockKey,
    job: &RenderJob,
    block: PixelRect,
    cancel: &CancelToken,
) -> Result<Option<Arc<Rendered>>, EngineError> {
    let (slot, owner) = doc.blocks.acquire(key);
    if !owner {
        return match slot.wait(cancel) {
            Ok(rendered) => Ok(Some(rendered)),
            Err(EngineError::Cancelled) if !cancel.is_cancelled() => Ok(None),
            Err(e) => Err(e),
        };
    }
    // Re-check before committing a thread to a whole block.
    if cancel.is_cancelled() {
        doc.blocks.complete(key, &slot, Err(EngineError::Cancelled));
        return Err(EngineError::Cancelled);
    }
    let block_job = RenderJob {
        rect: block,
        cancel: None,
        ..job.clone()
    };
    match pool.render(block_job) {
        Ok(rendered) => {
            let rendered = Arc::new(rendered);
            doc.blocks.complete(key, &slot, Ok(Arc::clone(&rendered)));
            Ok(Some(rendered))
        }
        Err(e) => {
            doc.blocks.complete(key, &slot, Err(e.clone()));
            Err(e)
        }
    }
}

/// Copies `region` out of a rendered rectangle into the caller's target,
/// converting to the target's channel order.
fn copy_out(
    rendered: &Rendered,
    region: PixelRect,
    target: &mut PixmapMut<'_>,
) -> Result<RenderOutcome, EngineError> {
    let r = rendered.rect;
    if !region.is_within(r) {
        return Err(EngineError::Internal(
            "rendered rect does not cover region".into(),
        ));
    }
    let stride = r.width as usize * 4;
    let offset = (region.y - r.y) as usize * stride + (region.x - r.x) as usize * 4;
    let src = rendered
        .data
        .get(offset..)
        .ok_or_else(|| EngineError::Internal("rendered buffer too small".into()))?;
    target.copy_from_rgba(src, stride)?;
    Ok(RenderOutcome {
        partial: rendered.partial,
    })
}

/// Runs on a pool thread.
pub(crate) fn execute_render<'p>(
    doc: &DocInner,
    generation: &'p Generation,
    caches: &mut ThreadCaches<'p>,
    job: &RenderJob,
) -> Result<Rendered, EngineError> {
    let cancelled = || job.cancel.as_ref().is_some_and(CancelToken::is_cancelled);
    if cancelled() {
        return Err(EngineError::Cancelled);
    }
    let pages = generation.pdf.pages();
    let page = pages
        .get(job.page as usize)
        .ok_or_else(|| EngineError::Internal("page index out of range".into()))?;
    preflight::ensure_verdict(doc, job.page, page, &caches.interp, job.cancel.as_ref())?;
    if cancelled() {
        return Err(EngineError::Cancelled);
    }

    let (w, h) = (job.rect.width, job.rect.height);
    check_target(PixelSize::new(w, h), &doc.limits)?;
    let (w16, h16) = (
        u16::try_from(w).map_err(|_| EngineError::LimitExceeded(LimitKind::BitmapDimension))?,
        u16::try_from(h).map_err(|_| EngineError::LimitExceeded(LimitKind::BitmapDimension))?,
    );
    let geom = PageGeom::from_page(page);
    let transform = geom.device_transform(page, job.scale, job.rotation, job.rect);

    let warnings = Arc::new(AtomicU32::new(0));
    let counter = Arc::clone(&warnings);
    let settings = InterpreterSettings {
        font_resolver: fonts::resolver(),
        render_annotations: job.annotations,
        warning_sink: Arc::new(move |_| {
            counter.fetch_add(1, Ordering::Relaxed);
        }),
        ..InterpreterSettings::default()
    };

    let ctx = match caches.context.as_mut() {
        Some(ctx) => {
            ctx.reset_and_resize(w16, h16);
            ctx
        }
        None => caches.context.insert(RenderContext::new(w16, h16)),
    };
    render_into(
        page,
        &caches.render,
        &settings,
        &RenderSettings::default(),
        ctx,
        transform,
    );
    ctx.flush();
    if cancelled() {
        return Err(EngineError::Cancelled);
    }

    // check_target bounded w * h * 4 by max_bitmap_bytes.
    let mut data = vec![0u8; w as usize * h as usize * 4];
    let pixmap = hayro::vello_cpu::PixmapMut::new(w16, h16, &mut data)
        .ok_or_else(|| EngineError::Internal("render buffer size mismatch".into()))?;
    let bg = job.background;
    ctx.render_with(
        pixmap,
        &mut caches.resources,
        RasterizerSettings {
            target_init: TargetInit::Clear(AlphaColor::from_rgba8(bg.r, bg.g, bg.b, bg.a)),
            ..RasterizerSettings::default()
        },
    );

    if let Some(stream) = page.page_stream() {
        doc.account_content(generation, job.page, stream.len());
    }
    caches.note_page(job.page);
    Ok(Rendered {
        rect: job.rect,
        data,
        partial: warnings.load(Ordering::Relaxed) > 0,
    })
}

/// Identity of a block: everything that changes its pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct BlockKey {
    page: u32,
    scale_bits: u32,
    rotation: Rotation,
    background: Rgba8,
    color: ColorMode,
    annotations: bool,
    x: u32,
    y: u32,
}

impl BlockKey {
    fn new(request: &RenderRequest, block: PixelRect) -> Self {
        Self {
            page: request.page.get(),
            scale_bits: request.scale.get().to_bits(),
            rotation: request.rotation,
            background: request.background,
            color: request.color_mode,
            annotations: request.annotations,
            x: block.x,
            y: block.y,
        }
    }
}

enum SlotState {
    Pending,
    Ready(Arc<Rendered>),
    Failed(EngineError),
}

/// One block being rendered or ready.
pub(crate) struct Slot {
    state: Mutex<SlotState>,
    ready: Condvar,
    last_use: AtomicU64,
}

impl Slot {
    fn wait(&self, cancel: &CancelToken) -> Result<Arc<Rendered>, EngineError> {
        let mut state = lock(&self.state);
        loop {
            match &*state {
                SlotState::Ready(r) => return Ok(Arc::clone(r)),
                SlotState::Failed(e) => return Err(e.clone()),
                SlotState::Pending => {}
            }
            if cancel.is_cancelled() {
                return Err(EngineError::Cancelled);
            }
            state = self
                .ready
                .wait_timeout(state, WAIT_POLL)
                .map(|(g, _)| g)
                .unwrap_or_else(|e| e.into_inner().0);
        }
    }
}

struct BlockMap {
    slots: HashMap<BlockKey, Arc<Slot>>,
    bytes: usize,
    tick: u64,
}

/// Byte-budgeted LRU of rendered blocks plus in-flight deduplication.
pub(crate) struct BlockCache {
    map: Mutex<BlockMap>,
    budget: usize,
}

impl BlockCache {
    pub(crate) fn new(budget: usize) -> Self {
        Self {
            map: Mutex::new(BlockMap {
                slots: HashMap::new(),
                bytes: 0,
                tick: 0,
            }),
            budget,
        }
    }

    /// The slot for `key` and whether the caller must render it.
    fn acquire(&self, key: BlockKey) -> (Arc<Slot>, bool) {
        let mut map = lock(&self.map);
        map.tick += 1;
        let tick = map.tick;
        if let Some(slot) = map.slots.get(&key) {
            slot.last_use.store(tick, Ordering::Relaxed);
            return (Arc::clone(slot), false);
        }
        let slot = Arc::new(Slot {
            state: Mutex::new(SlotState::Pending),
            ready: Condvar::new(),
            last_use: AtomicU64::new(tick),
        });
        map.slots.insert(key, Arc::clone(&slot));
        (slot, true)
    }

    fn complete(
        &self,
        key: BlockKey,
        slot: &Arc<Slot>,
        result: Result<Arc<Rendered>, EngineError>,
    ) {
        let bytes = result.as_ref().map_or(0, |r| r.data.len());
        let failed = result.is_err();
        *lock(&slot.state) = match result {
            Ok(r) => SlotState::Ready(r),
            Err(e) => SlotState::Failed(e),
        };
        slot.ready.notify_all();

        let mut map = lock(&self.map);
        let is_current = map.slots.get(&key).is_some_and(|s| Arc::ptr_eq(s, slot));
        if !is_current {
            return; // cleared while rendering
        }
        if failed {
            // Failures are not cached; per-page verdicts already make
            // deterministic failures fast.
            map.slots.remove(&key);
            return;
        }
        map.bytes += bytes;
        while map.bytes > self.budget {
            let victim = map
                .slots
                .iter()
                .filter(|(_, s)| matches!(*lock(&s.state), SlotState::Ready(_)))
                .min_by_key(|(_, s)| s.last_use.load(Ordering::Relaxed))
                .map(|(k, _)| *k);
            let Some(victim) = victim else { break };
            if let Some(s) = map.slots.remove(&victim)
                && let SlotState::Ready(r) = &*lock(&s.state)
            {
                map.bytes = map.bytes.saturating_sub(r.data.len());
            }
        }
    }

    /// Drops every finished block (in-flight ones complete normally).
    pub(crate) fn clear(&self) {
        let mut map = lock(&self.map);
        map.slots
            .retain(|_, s| matches!(*lock(&s.state), SlotState::Pending));
        map.bytes = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits() -> ResourceLimits {
        ResourceLimits::default()
    }

    #[test]
    fn blocks_are_aligned_and_clipped_to_the_page() {
        let page = PixelSize::new(2448, 3168);
        let tile = PixelRect::new(512, 2560, 512, 512);
        assert_eq!(
            block_for(tile, page, &limits()),
            Some(PixelRect::new(0, 2048, 2048, 1120))
        );
        let edge = PixelRect::new(2048, 0, 400, 512);
        assert_eq!(
            block_for(edge, page, &limits()),
            Some(PixelRect::new(2048, 0, 400, 2048))
        );
    }

    #[test]
    fn whole_blocks_and_straddling_regions_render_directly() {
        let page = PixelSize::new(1224, 1584);
        assert_eq!(block_for(page.bounds(), page, &limits()), None);
        let big = PixelSize::new(5000, 5000);
        let straddling = PixelRect::new(1900, 0, 300, 300);
        assert_eq!(block_for(straddling, big, &limits()), None);
    }

    #[test]
    fn oversized_targets_are_refused() {
        let mut l = limits();
        l.max_bitmap_dimension = 100_000;
        l.max_bitmap_bytes = u64::MAX;
        assert_eq!(
            check_target(PixelSize::new(65_535, 8), &l),
            Err(EngineError::LimitExceeded(LimitKind::BitmapDimension))
        );
    }

    #[test]
    fn block_cache_evicts_by_bytes() {
        let cache = BlockCache::new(1000);
        let key = |x| BlockKey {
            page: 0,
            scale_bits: 0,
            rotation: Rotation::R0,
            background: Rgba8::WHITE,
            color: ColorMode::Normal,
            annotations: true,
            x,
            y: 0,
        };
        for x in 0..3 {
            let (slot, owner) = cache.acquire(key(x));
            assert!(owner);
            let r = Arc::new(Rendered {
                rect: PixelRect::new(0, 0, 1, 1),
                data: vec![0; 400],
                partial: false,
            });
            cache.complete(key(x), &slot, Ok(r));
        }
        let map = lock(&cache.map);
        assert!(map.bytes <= 1000);
        assert_eq!(map.slots.len(), 2);
        assert!(!map.slots.contains_key(&key(0)));
    }
}
