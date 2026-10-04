//! A document opened in a view: layout, viewport, navigation, and the tile
//! pipeline, independent of the UI toolkit.
//!
//! The UI calls [`DocumentSession::frame`] when it paints and draws the
//! returned [`Frame`]: page backgrounds, exact tiles, and lower/higher
//! resolution stand-ins for tiles that are still rendering (spec §13: the
//! view never waits for a render). Everything expensive happens on the
//! render scheduler's worker threads; `frame` only does bookkeeping.
//!
//! Transient engine failures (`EngineError::is_transient`: a render host
//! restarting or gone while it handled a request) are not shown as errors:
//! the page keeps its estimated size or its stand-ins, and the request is
//! made again after a growing delay (`crate::retry`); a retry clock wakes
//! the UI through the session's wake hook when one is due.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::hash::Hash;
use std::ops::Range;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use fastpdf_cache::{BudgetedCache, SharedCache, retention};
use fastpdf_engine_api::{
    ColorMode, Destination, DestinationView, DocumentId, EngineDocument, EngineError,
    GuardedDocument, PageId, PageIndex, PageInfo, PageRect, PageSize, PixelFormat, Pixmap,
    RenderRequest, RenderScale, Rgba8, Rotation,
};
use fastpdf_render::{
    DocumentLayout, Lane, LayoutRect, PlanConfig, Priority, RenderJob, RenderScheduler,
    ScaleBucket, SchedulerConfig, SchedulerStats, TileCache, TileGrid, TileKey, Viewport,
    ZoomLevel, plan_tiles,
};

use crate::retry::{Backoff, RetryClock};

/// Session settings.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionConfig {
    pub tile_size: u32,
    pub workers: usize,
    /// Tile cache budget in bytes (spec §15: 128 MB to start).
    pub tile_budget: usize,
    /// Gap between pages, in points.
    pub page_gap: f32,
    /// Margin used by fit-width / fit-page, in logical pixels.
    pub fit_margin: f32,
    pub pixel_format: PixelFormat,
    pub paper: Rgba8,
    /// Extra area rendered ahead of scrolling, in viewport heights.
    pub near_margin: f32,
    /// Pixels rendered around each tile so scaled tiles meet without seams.
    pub tile_gutter: u32,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            tile_size: fastpdf_render::DEFAULT_TILE_SIZE,
            workers: SchedulerConfig::default_workers(),
            tile_budget: 128 * 1024 * 1024,
            page_gap: 8.0,
            fit_margin: 8.0,
            pixel_format: PixelFormat::Bgra8Premultiplied,
            paper: Rgba8::WHITE,
            near_margin: 0.5,
            tile_gutter: 2,
        }
    }
}

/// How zoom follows window resizes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZoomMode {
    FitWidth,
    FitPage,
    Custom,
}

/// A page visible in the frame. Rectangles are `[x, y, width, height]` in
/// logical pixels relative to the viewport's top-left corner.
#[derive(Debug, Clone, PartialEq)]
pub struct FramePage {
    pub page: PageIndex,
    pub rect: [f32; 4],
    /// Set when the page cannot be rendered; the UI shows it instead of content.
    pub error: Option<String>,
    /// The engine error behind `error`, for localized messages.
    pub cause: Option<EngineError>,
}

/// An image to draw, already positioned.
#[derive(Debug, Clone, PartialEq)]
pub struct FrameTile<V> {
    pub key: TileKey,
    pub image: V,
    /// Destination `[x, y, width, height]` in logical pixels.
    pub rect: [f32; 4],
    /// Part of the image to draw, `[x, y, width, height]` in image pixels.
    /// Tiles are rendered with a gutter; only this inner part is shown.
    pub src: [f32; 4],
    /// False for stand-ins from another scale bucket.
    pub exact: bool,
}

/// Everything the UI needs to paint the document area.
#[derive(Debug, Clone, PartialEq)]
pub struct Frame<V> {
    pub pages: Vec<FramePage>,
    /// Draw in order: stand-ins first, exact tiles on top.
    pub tiles: Vec<FrameTile<V>>,
    /// Visible tiles that are not available at the exact resolution yet.
    pub pending: usize,
    /// Visible tiles that are rendered but were not ready to draw (e.g. the
    /// UI's per-frame upload budget was spent); stand-ins were used instead.
    /// The UI should request another frame while this is non-zero.
    pub deferred: usize,
}

/// Counters for the development overlay (spec §46).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SessionStats {
    pub scheduler: SchedulerStats,
    pub tile_bytes: usize,
    pub tile_entries: usize,
    pub tile_hits: u64,
    pub tile_misses: u64,
    pub tile_evictions: u64,
    pub thumbnail_bytes: usize,
    pub thumbnail_entries: usize,
    /// Pages, tiles and thumbnails waiting to retry a transient failure.
    pub pending_retries: usize,
}

/// Identity of a sidebar thumbnail.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ThumbnailKey {
    pub page: PageId,
    pub width_px: u32,
    pub rotation: Rotation,
    pub color: ColorMode,
}

/// One sidebar row returned by [`DocumentSession::thumbnails`].
#[derive(Debug, Clone, PartialEq)]
pub struct ThumbnailItem<V> {
    pub page: PageIndex,
    /// Thumbnail size in device pixels: the configured width and the
    /// page's aspect ratio.
    pub size: [u32; 2],
    /// `None` while rendering, or when the page cannot be rendered.
    pub image: Option<V>,
    pub failed: bool,
}

/// What the shared scheduler renders for a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum RenderKey {
    Tile(TileKey),
    Thumbnail(ThumbnailKey),
}

enum Incoming<V> {
    Tile {
        key: TileKey,
        image: V,
        bytes: usize,
    },
    Thumbnail {
        key: ThumbnailKey,
        image: V,
        bytes: usize,
    },
    Failed {
        key: RenderKey,
        error: EngineError,
    },
}

type ThumbnailEvict<V> = Arc<dyn Fn(Vec<(ThumbnailKey, V)>) + Send + Sync>;

/// Sidebar thumbnails; exists only while the sidebar is open (spec §20,
/// §23: thumbnail generation is a cost, so it starts on demand).
struct Thumbnails<V> {
    width_px: u32,
    cache: Arc<SharedCache<ThumbnailKey, V>>,
    failed: HashSet<ThumbnailKey>,
    requested: Option<(Range<u32>, Rotation)>,
}

/// Inputs that require a new render plan when they change.
#[derive(Debug, Clone, PartialEq)]
struct PlanInputs {
    viewport: Viewport,
    rotation: Rotation,
    color: ColorMode,
    layout_version: u64,
}

/// One open document in one view.
pub struct DocumentSession<V: Clone + Send + 'static> {
    id: DocumentId,
    doc: Arc<GuardedDocument>,
    config: SessionConfig,
    layout: DocumentLayout,
    layout_version: u64,
    viewport: Viewport,
    rotation: Rotation,
    color: ColorMode,
    zoom_mode: ZoomMode,
    scheduler: RenderScheduler<RenderKey>,
    incoming: Receiver<Incoming<V>>,
    wake_pending: Arc<AtomicBool>,
    tiles: TileCache<V>,
    failed: HashSet<TileKey>,
    page_errors: Vec<(PageIndex, EngineError)>,
    /// Transient failures waiting for their next attempt.
    geometry_retry: HashMap<PageIndex, Backoff>,
    /// Pages whose geometry failed for good (a permanent error, or the last
    /// retry of a transient one), shared by the page view and thumbnails so
    /// neither asks the engine again.
    geometry_failed: HashMap<PageIndex, EngineError>,
    tile_retry: HashMap<TileKey, Backoff>,
    thumbnail_retry: HashMap<ThumbnailKey, Backoff>,
    retry_clock: RetryClock,
    planned: Option<PlanInputs>,
    thumbnails: Option<Thumbnails<V>>,
    thumbnail_evict: Option<ThumbnailEvict<V>>,
}

impl<V: Clone + Send + 'static> fmt::Debug for DocumentSession<V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DocumentSession")
            .field("id", &self.id)
            .field("pages", &self.page_count())
            .field("zoom", &self.viewport.zoom)
            .finish_non_exhaustive()
    }
}

impl<V: Clone + Send + 'static> DocumentSession<V> {
    /// Creates a session for an opened document.
    ///
    /// * `convert` turns rendered pixmaps into the UI's image type; it runs
    ///   on render worker threads, so GPU-side preparation stays off the UI
    ///   thread.
    /// * `wake` asks the UI for a repaint; calls are coalesced until the
    ///   next [`DocumentSession::frame`].
    /// * `on_evict` receives tiles dropped from the cache (free GPU memory).
    pub fn new(
        doc: Arc<GuardedDocument>,
        config: SessionConfig,
        view_size: (f32, f32),
        device_scale: f32,
        convert: impl Fn(Pixmap) -> V + Send + Sync + 'static,
        wake: impl Fn() + Send + Sync + 'static,
        on_evict: impl Fn(Vec<(TileKey, V)>) + Send + Sync + 'static,
    ) -> Self {
        let id = DocumentId::next();
        let first = doc.page_info(PageIndex::FIRST).ok();
        let estimate = first.map_or(PageSize::LETTER, |i| i.display_size(Rotation::R0));
        let mut layout = DocumentLayout::new(doc.page_count(), estimate, config.page_gap);
        if first.is_some() {
            layout.set_page_sizes([(PageIndex::FIRST, estimate)]);
        }

        let (tx, incoming) = mpsc::channel();
        let tx = Mutex::new(tx);
        let wake_pending = Arc::new(AtomicBool::new(false));
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(wake);
        let retry_wake = {
            let pending = Arc::clone(&wake_pending);
            let wake = Arc::clone(&wake);
            move || {
                if !pending.swap(true, Ordering::AcqRel) {
                    wake();
                }
            }
        };
        let pending = Arc::clone(&wake_pending);
        let sink = move |result: fastpdf_render::TileResult<RenderKey>| {
            let msg = match (result.key, result.result) {
                (RenderKey::Tile(key), Ok(pixmap)) => {
                    let bytes = pixmap.byte_len();
                    Incoming::Tile {
                        key,
                        image: convert(pixmap),
                        bytes,
                    }
                }
                (RenderKey::Thumbnail(key), Ok(pixmap)) => {
                    let bytes = pixmap.byte_len();
                    Incoming::Thumbnail {
                        key,
                        image: convert(pixmap),
                        bytes,
                    }
                }
                (key, Err(error)) => Incoming::Failed { key, error },
            };
            if let Ok(tx) = tx.lock() {
                let _ = tx.send(msg);
            }
            if !pending.swap(true, Ordering::AcqRel) {
                wake();
            }
        };
        let workers = if doc.engine().capabilities.parallel_render {
            config.workers
        } else {
            1
        };
        let scheduler = RenderScheduler::new(
            doc.clone() as Arc<dyn EngineDocument>,
            SchedulerConfig {
                workers,
                pixel_format: config.pixel_format,
                limits: doc.limits().clone(),
            },
            sink,
        );
        let tiles = TileCache::new(config.tile_budget, on_evict);
        let mut session = Self {
            id,
            doc,
            viewport: Viewport::new(
                view_size.0,
                view_size.1,
                ZoomLevel::ACTUAL_SIZE,
                device_scale,
            ),
            config,
            layout,
            layout_version: 0,
            rotation: Rotation::R0,
            color: ColorMode::Normal,
            zoom_mode: ZoomMode::FitWidth,
            scheduler,
            incoming,
            wake_pending,
            tiles,
            failed: HashSet::new(),
            page_errors: Vec::new(),
            geometry_retry: HashMap::new(),
            geometry_failed: HashMap::new(),
            tile_retry: HashMap::new(),
            thumbnail_retry: HashMap::new(),
            retry_clock: RetryClock::new(Arc::new(retry_wake)),
            planned: None,
            thumbnails: None,
            thumbnail_evict: None,
        };
        session.apply_zoom_mode();
        session.viewport.clamp_scroll(&session.layout);
        session
    }

    pub fn id(&self) -> DocumentId {
        self.id
    }

    pub fn document(&self) -> &Arc<GuardedDocument> {
        &self.doc
    }

    pub fn page_count(&self) -> u32 {
        self.doc.page_count()
    }

    pub fn viewport(&self) -> &Viewport {
        &self.viewport
    }

    pub fn layout(&self) -> &DocumentLayout {
        &self.layout
    }

    pub fn zoom(&self) -> ZoomLevel {
        self.viewport.zoom
    }

    pub fn zoom_mode(&self) -> ZoomMode {
        self.zoom_mode
    }

    pub fn rotation(&self) -> Rotation {
        self.rotation
    }

    pub fn color_mode(&self) -> ColorMode {
        self.color
    }

    /// Switches page colors (e.g. night mode). Tiles of the other mode stay
    /// cached until evicted, so switching back is instant while they last;
    /// the UI should paint unrendered paper in the matching color.
    pub fn set_color_mode(&mut self, color: ColorMode) {
        self.color = color;
    }

    /// Tile cache handle, for registration with the memory budget manager.
    pub fn tile_cache(&self) -> &TileCache<V> {
        &self.tiles
    }

    /// The page at the top third of the view — what users consider "current".
    pub fn current_page(&self) -> PageIndex {
        let v = self.viewport.visible_rect();
        self.layout.page_at(v.y + v.height / 3.0)
    }

    // ---- view changes -------------------------------------------------

    pub fn resize(&mut self, width: f32, height: f32, device_scale: f32) {
        let anchor = self.layout.anchor_at(self.viewport.scroll_y);
        let zoom = self.viewport.zoom;
        let (sx, sy) = (self.viewport.scroll_x, self.viewport.scroll_y);
        self.viewport = Viewport::new(width, height, zoom, device_scale);
        self.viewport.scroll_x = sx;
        self.viewport.scroll_y = sy;
        self.apply_zoom_mode();
        self.viewport.scroll_y = self.layout.y_for_anchor(anchor);
        self.viewport.clamp_scroll(&self.layout);
    }

    /// Scrolls by logical pixels.
    pub fn scroll_by(&mut self, dx: f32, dy: f32) {
        let ppp = self.viewport.px_per_point();
        self.viewport.scroll_x += f64::from(dx) / ppp;
        self.viewport.scroll_y += f64::from(dy) / ppp;
        self.viewport.clamp_scroll(&self.layout);
    }

    /// Maps a rectangle in page space (points, unrotated, CropBox top-left
    /// origin — what text layers, links and search hits use) to view
    /// coordinates `[x, y, width, height]` in logical pixels, applying the
    /// page's and the user's rotation. `None` for unknown pages.
    pub fn page_to_view(&self, page: PageIndex, rect: PageRect) -> Option<[f32; 4]> {
        let (page_rect, info, rotation) = self.page_frame(page)?;
        let (ax, ay) = rotate_point(rect.x0, rect.y0, info.size, rotation);
        let (bx, by) = rotate_point(rect.x1, rect.y1, info.size, rotation);
        let x0 = page_rect.x + f64::from(ax.min(bx));
        let y0 = page_rect.y + f64::from(ay.min(by));
        let x1 = page_rect.x + f64::from(ax.max(bx));
        let y1 = page_rect.y + f64::from(ay.max(by));
        let [vx0, vy0] = self.layout_to_view(x0, y0);
        let [vx1, vy1] = self.layout_to_view(x1, y1);
        Some([vx0, vy0, vx1 - vx0, vy1 - vy0])
    }

    /// The page under a view point (logical pixels) and the point in that
    /// page's space, for hit testing (selection, links). `None` between
    /// pages or outside the document.
    pub fn view_to_page(&self, x: f32, y: f32) -> Option<(PageIndex, f32, f32)> {
        let ppp = self.viewport.px_per_point();
        let lx = self.viewport.scroll_x + f64::from(x) / ppp;
        let ly = self.viewport.scroll_y + f64::from(y) / ppp;
        let page = self.layout.page_at(ly);
        let (page_rect, info, rotation) = self.page_frame(page)?;
        if lx < page_rect.x || lx > page_rect.right() || ly < page_rect.y || ly > page_rect.bottom()
        {
            return None;
        }
        let dx = (lx - page_rect.x) as f32;
        let dy = (ly - page_rect.y) as f32;
        let (px, py) = unrotate_point(dx, dy, info.size, rotation);
        Some((page, px, py))
    }

    /// Navigates to a link or bookmark target, scrolled so the destination's
    /// top (when given) is at the top of the view.
    pub fn go_to_destination(&mut self, dest: &Destination) {
        self.go_to_page(dest.page);
        let top = match dest.view {
            DestinationView::Xyz { top, .. } | DestinationView::FitWidth { top } => top,
            DestinationView::FitRect(r) => Some(r.y0),
            DestinationView::Fit | DestinationView::FitHeight { .. } => None,
        };
        if let (Some(top), Some((page_rect, info, rotation))) = (top, self.page_frame(dest.page)) {
            let (_, dy) = rotate_point(0.0, top.max(0.0), info.size, rotation);
            self.viewport.scroll_y = page_rect.y + f64::from(dy);
            self.viewport.clamp_scroll(&self.layout);
        }
    }

    pub fn go_to_page(&mut self, page: PageIndex) {
        let page = PageIndex::new(page.get().min(self.page_count().saturating_sub(1)));
        self.resolve_pages(page.get()..page.get() + 1);
        if let Some(rect) = self.layout.page_rect(page) {
            self.viewport.scroll_y = rect.y - f64::from(self.config.page_gap);
            self.viewport.clamp_scroll(&self.layout);
        }
    }

    pub fn next_page(&mut self) {
        if let Some(next) = self.current_page().next(self.page_count()) {
            self.go_to_page(next);
        }
    }

    pub fn prev_page(&mut self) {
        if let Some(prev) = self.current_page().prev() {
            self.go_to_page(prev);
        }
    }

    pub fn first_page(&mut self) {
        self.go_to_page(PageIndex::FIRST);
    }

    pub fn last_page(&mut self) {
        self.go_to_page(PageIndex::new(self.page_count().saturating_sub(1)));
    }

    /// Scrolls one view height (minus a little overlap for context).
    pub fn page_down(&mut self) {
        self.scroll_by(0.0, self.viewport.height * 0.9);
    }

    pub fn page_up(&mut self) {
        self.scroll_by(0.0, -self.viewport.height * 0.9);
    }

    /// Sets a custom zoom, keeping the content under (`ax`, `ay`) — logical
    /// pixels in the view; `None` = view center — in place.
    pub fn set_zoom(&mut self, zoom: ZoomLevel, anchor: Option<(f32, f32)>) {
        self.zoom_mode = ZoomMode::Custom;
        let (ax, ay) = anchor.unwrap_or((self.viewport.width / 2.0, self.viewport.height / 2.0));
        self.viewport.zoom_around(zoom, ax, ay);
        self.viewport.clamp_scroll(&self.layout);
    }

    pub fn zoom_in(&mut self, anchor: Option<(f32, f32)>) {
        self.set_zoom(self.viewport.zoom.zoom_in(), anchor);
    }

    pub fn zoom_out(&mut self, anchor: Option<(f32, f32)>) {
        self.set_zoom(self.viewport.zoom.zoom_out(), anchor);
    }

    pub fn actual_size(&mut self) {
        self.set_zoom(ZoomLevel::ACTUAL_SIZE, None);
    }

    pub fn fit_width(&mut self) {
        self.zoom_mode = ZoomMode::FitWidth;
        self.apply_zoom_mode();
        self.viewport.clamp_scroll(&self.layout);
    }

    pub fn fit_page(&mut self) {
        let current = self.current_page();
        self.zoom_mode = ZoomMode::FitPage;
        self.apply_zoom_mode();
        self.go_to_page(current);
    }

    pub fn rotate_clockwise(&mut self) {
        self.set_rotation(self.rotation.clockwise());
    }

    pub fn rotate_counter_clockwise(&mut self) {
        self.set_rotation(self.rotation.counter_clockwise());
    }

    fn set_rotation(&mut self, rotation: Rotation) {
        let current = self.current_page();
        self.rotation = rotation;
        // Rebuild with rotated estimates; known pages are re-resolved lazily.
        let estimate = self
            .doc
            .page_info(PageIndex::FIRST)
            .map_or(PageSize::LETTER, |i| i.display_size(rotation));
        self.layout = DocumentLayout::new(self.page_count(), estimate, self.config.page_gap);
        self.layout_version += 1;
        self.apply_zoom_mode();
        self.go_to_page(current);
    }

    fn apply_zoom_mode(&mut self) {
        let zoom = match self.zoom_mode {
            ZoomMode::Custom => return,
            ZoomMode::FitWidth => self
                .viewport
                .fit_width_zoom(&self.layout, self.config.fit_margin),
            ZoomMode::FitPage => {
                let page = self
                    .layout
                    .page_size(self.current_page())
                    .unwrap_or(PageSize::LETTER);
                self.viewport.fit_page_zoom(page, self.config.fit_margin)
            }
        };
        self.viewport.zoom = zoom;
    }

    // ---- painting -----------------------------------------------------

    /// Collects finished tiles, schedules what the view needs, and returns
    /// what to draw now. Cheap enough to call on every paint.
    pub fn frame(&mut self) -> Frame<V> {
        self.frame_with(|_| true)
    }

    /// Like [`DocumentSession::frame`], but asks `ready` whether an image
    /// may be drawn this frame (e.g. whether it fits the UI's upload budget).
    /// Images that are not ready are replaced by stand-ins and counted in
    /// [`Frame::deferred`].
    pub fn frame_with(&mut self, mut ready: impl FnMut(&V) -> bool) -> Frame<V> {
        self.wake_pending.store(false, Ordering::Release);
        self.drain_incoming();

        let visible = self.viewport.visible_rect();
        let near = visible.expand_y(visible.height * f64::from(self.config.near_margin));
        let range = self.layout.pages_intersecting(near.y, near.bottom());
        self.resolve_pages(range);

        let inputs = PlanInputs {
            viewport: self.viewport,
            rotation: self.rotation,
            color: self.color,
            layout_version: self.layout_version,
        };
        let plan = plan_tiles(
            self.id,
            &self.layout,
            &self.viewport,
            |p| self.known_page_info(p),
            &self.plan_config(),
        );

        let frame = self.build_frame(&plan, &mut ready);
        // Tiles evicted after planning would otherwise never come back.
        if frame.pending > 0 && self.scheduler.is_idle() {
            self.planned = None;
        }
        if self.planned.as_ref() != Some(&inputs) {
            let now = Instant::now();
            let needed: Vec<_> = plan
                .into_iter()
                .filter(|t| {
                    !self.tiles.contains(&t.key)
                        && !self.failed.contains(&t.key)
                        && !self.tile_retry.get(&t.key).is_some_and(|b| b.waiting(now))
                })
                .map(|t| RenderJob {
                    key: RenderKey::Tile(t.key),
                    priority: t.priority,
                    distance: t.distance,
                    request: t.request,
                })
                .collect();
            self.scheduler.submit_plan(needed);
            self.planned = Some(inputs);
        }
        self.schedule_retries();
        frame
    }

    /// Asks the retry clock to wake the UI when the earliest pending retry
    /// is due (nothing at all when none is pending).
    ///
    /// Only retries still in the future count. One that is already due was
    /// either attempted by this frame (and then succeeded, failed for good
    /// or moved its time forward) or is not needed right now (a page scrolled
    /// away, a tile no longer planned); waking the UI for it would only
    /// repeat this frame, forever. It is attempted again when it is needed.
    fn schedule_retries(&self) {
        let now = Instant::now();
        let next = self
            .geometry_retry
            .values()
            .chain(self.tile_retry.values())
            .chain(self.thumbnail_retry.values())
            .map(|b| b.next)
            .filter(|&next| next > now)
            .min();
        if let Some(next) = next {
            self.retry_clock.schedule(next);
        }
    }

    /// Releases everything this session shows: cancels all rendering and
    /// passes every cached tile and thumbnail to the eviction hooks, so the
    /// UI can free their GPU resources before dropping the session.
    pub fn close(&mut self) {
        self.scheduler.cancel_all();
        self.tiles.remove_document(self.id);
        self.disable_thumbnails();
        self.planned = None;
    }

    pub fn stats(&self) -> SessionStats {
        let t = self.tiles.stats();
        let th = self
            .thumbnails
            .as_ref()
            .map(|th| th.cache.stats())
            .unwrap_or_default();
        SessionStats {
            scheduler: self.scheduler.stats(),
            tile_bytes: t.bytes,
            tile_entries: t.entries,
            tile_hits: t.hits,
            tile_misses: t.misses,
            tile_evictions: t.evictions,
            thumbnail_bytes: th.bytes,
            thumbnail_entries: th.entries,
            pending_retries: self.geometry_retry.len()
                + self.tile_retry.len()
                + self.thumbnail_retry.len(),
        }
    }

    // ---- thumbnails (spec §23) -----------------------------------------

    /// Starts the thumbnail sidebar. Nothing is rendered for thumbnails
    /// before this is called, and afterwards only the rows the sidebar asks
    /// for (plus a small margin) — never every page.
    ///
    /// `on_evict` receives thumbnails dropped from their cache (free GPU
    /// memory), including late arrivals after the sidebar closed.
    /// Returns the cache handle for the memory budget manager.
    pub fn enable_thumbnails(
        &mut self,
        width_px: u32,
        budget: usize,
        on_evict: impl Fn(Vec<(ThumbnailKey, V)>) + Send + Sync + 'static,
    ) -> Arc<dyn BudgetedCache> {
        self.disable_thumbnails();
        let hook: ThumbnailEvict<V> = Arc::new(on_evict);
        let cache_hook = Arc::clone(&hook);
        let cache = Arc::new(
            SharedCache::new("thumbnails", budget, retention::THUMBNAILS)
                .with_eviction_hook(move |evicted| cache_hook(evicted)),
        );
        self.thumbnails = Some(Thumbnails {
            width_px: width_px.clamp(16, 1024),
            cache: Arc::clone(&cache),
            failed: HashSet::new(),
            requested: None,
        });
        self.thumbnail_evict = Some(hook);
        cache
    }

    /// Closes the thumbnail sidebar: cancels pending thumbnail renders and
    /// releases every cached thumbnail.
    pub fn disable_thumbnails(&mut self) {
        if let Some(th) = self.thumbnails.take() {
            self.scheduler.submit(Lane::Thumbnails, Vec::new());
            th.cache.retain(|_, _| false);
        }
    }

    pub fn thumbnails_enabled(&self) -> bool {
        self.thumbnails.is_some()
    }

    /// Thumbnails for the sidebar rows `visible` (page indices). Missing
    /// ones for `visible` and `margin` rows around it are scheduled at P4,
    /// behind everything the main view needs. Empty while disabled.
    pub fn thumbnails(&mut self, visible: Range<u32>, margin: u32) -> Vec<ThumbnailItem<V>> {
        self.drain_incoming();
        let rotation = self.rotation;
        let id = self.id;
        let count = self.page_count();
        let Some(th) = self.thumbnails.as_mut() else {
            return Vec::new();
        };
        let visible = visible.start.min(count)..visible.end.min(count);
        let wanted =
            visible.start.saturating_sub(margin)..visible.end.saturating_add(margin).min(count);
        let color = self.color;
        let key = |page: u32| ThumbnailKey {
            page: PageId::new(id, PageIndex::new(page)),
            width_px: th.width_px,
            rotation,
            color,
        };

        let mut items = Vec::with_capacity(visible.len());
        let mut jobs = Vec::new();
        let now = Instant::now();
        for page in wanted.clone() {
            let k = key(page);
            let in_view = visible.contains(&page);
            let index = PageIndex::new(page);
            // A page whose geometry is waiting for a retry is still loading:
            // neither asked again yet nor shown as failed.
            let geometry_waiting = self
                .geometry_retry
                .get(&index)
                .is_some_and(|b| b.waiting(now));
            let (info, info_failed) = if self.geometry_failed.contains_key(&index) {
                (None, true)
            } else if geometry_waiting {
                (None, false)
            } else {
                match self.doc.page_info(index) {
                    Ok(info) => {
                        self.geometry_retry.remove(&index);
                        (Some(info), false)
                    }
                    Err(e)
                        if e.is_transient()
                            && note_failure(&mut self.geometry_retry, index, now) =>
                    {
                        (None, false)
                    }
                    Err(e) => {
                        // Final: remembered, so neither this list nor the
                        // page view asks the engine again.
                        self.geometry_retry.remove(&index);
                        self.geometry_failed.insert(index, e);
                        (None, true)
                    }
                }
            };
            let thumbnail_waiting = self.thumbnail_retry.get(&k).is_some_and(|b| b.waiting(now));
            let size = info.map_or([th.width_px, th.width_px], |i| {
                let d = i.display_size(rotation);
                let h = (th.width_px as f32 * d.height / d.width.max(1.0))
                    .round()
                    .max(1.0);
                [th.width_px, h as u32]
            });
            let image = if in_view {
                th.cache.with(&k, Clone::clone)
            } else {
                None
            };
            let cached = image.is_some() || th.cache.contains(&k);
            let failed = th.failed.contains(&k) || info_failed;
            if in_view {
                items.push(ThumbnailItem {
                    page: PageIndex::new(page),
                    size,
                    image,
                    failed,
                });
            }
            if cached || failed || thumbnail_waiting {
                continue;
            }
            let Some(info) = info else { continue };
            let scale =
                RenderScale::new(th.width_px as f32 / info.display_size(rotation).width.max(1.0));
            let Some(scale) = scale else {
                th.failed.insert(k);
                continue;
            };
            let mut request = RenderRequest::full_page(
                PageIndex::new(page),
                info.size,
                info.rotation,
                rotation,
                scale,
            );
            request.background = self.config.paper;
            request.color_mode = color;
            // Visible rows first, in order; margin rows after them.
            let distance = if in_view {
                (page - visible.start) as f32
            } else {
                10_000.0 + page.abs_diff(visible.start) as f32
            };
            jobs.push(RenderJob {
                key: RenderKey::Thumbnail(k),
                priority: Priority::Thumbnail,
                distance,
                request,
            });
        }

        let request = (wanted, rotation);
        let stale = th.requested.as_ref() != Some(&request);
        let stalled = !jobs.is_empty() && self.scheduler.is_lane_idle(Lane::Thumbnails);
        if stale || stalled {
            self.scheduler.submit(Lane::Thumbnails, jobs);
            th.requested = Some(request);
        }
        self.schedule_retries();
        items
    }

    fn plan_config(&self) -> PlanConfig {
        PlanConfig {
            tile_size: self.config.tile_size,
            near_margin: self.config.near_margin,
            prefetch_next_page: true,
            rotation: self.rotation,
            color: self.color,
            background: self.config.paper,
            gutter: self.config.tile_gutter,
        }
    }

    fn known_page_info(&self, page: PageIndex) -> Option<PageInfo> {
        if !self.layout.is_known(page) {
            return None;
        }
        self.doc.page_info(page).ok()
    }

    fn drain_incoming(&mut self) {
        while let Ok(msg) = self.incoming.try_recv() {
            match msg {
                Incoming::Tile { key, image, bytes } => {
                    self.tile_retry.remove(&key);
                    // Results for an old rotation are useless now; tiles of
                    // the other color mode are kept for a quick switch back.
                    if key.rotation == self.rotation {
                        self.tiles.insert(key, image, bytes);
                    }
                }
                Incoming::Thumbnail { key, image, bytes } => {
                    self.thumbnail_retry.remove(&key);
                    match &self.thumbnails {
                        Some(th)
                            if th.width_px == key.width_px
                                && key.rotation == self.rotation
                                && key.color == self.color =>
                        {
                            th.cache.insert(key, image, bytes);
                        }
                        // Sidebar closed or resized meanwhile: release right away.
                        _ => {
                            if let Some(evict) = &self.thumbnail_evict {
                                evict(vec![(key, image)]);
                            }
                        }
                    }
                }
                Incoming::Failed {
                    key: RenderKey::Tile(key),
                    error,
                } => {
                    if error.is_transient()
                        && note_failure(&mut self.tile_retry, key, Instant::now())
                    {
                        continue;
                    }
                    self.tile_retry.remove(&key);
                    self.failed.insert(key);
                    let page = key.page.page;
                    if !self.page_errors.iter().any(|(p, _)| *p == page) {
                        self.page_errors.push((page, error));
                    }
                }
                Incoming::Failed {
                    key: RenderKey::Thumbnail(key),
                    error,
                } => {
                    if error.is_transient()
                        && note_failure(&mut self.thumbnail_retry, key, Instant::now())
                    {
                        continue;
                    }
                    self.thumbnail_retry.remove(&key);
                    if let Some(th) = self.thumbnails.as_mut() {
                        th.failed.insert(key);
                    }
                }
            }
        }
    }

    /// Resolves real sizes for `pages`, keeping the view anchored.
    fn resolve_pages(&mut self, pages: std::ops::Range<u32>) {
        let now = Instant::now();
        let unknown: Vec<PageIndex> = pages
            .map(PageIndex::new)
            .filter(|p| !self.layout.is_known(*p))
            .filter(|p| !self.geometry_retry.get(p).is_some_and(|b| b.waiting(now)))
            .collect();
        if unknown.is_empty() {
            return;
        }
        let anchor = self.layout.anchor_at(self.viewport.scroll_y);
        let mut updates = Vec::with_capacity(unknown.len());
        for page in unknown {
            // A page that failed for good already (e.g. while listing
            // thumbnails) is not asked again, and its error stays final
            // even when it was a transient one that ran out of retries.
            let (answer, already_final) = match self.geometry_failed.get(&page) {
                Some(e) => (Err(e.clone()), true),
                None => (self.doc.page_info(page), false),
            };
            match answer {
                Ok(info) => {
                    self.geometry_retry.remove(&page);
                    updates.push((page, info.display_size(self.rotation)));
                }
                // Not known yet (render host restarting, ...): keep the
                // estimate, show no error, ask again later.
                Err(e)
                    if !already_final
                        && e.is_transient()
                        && note_failure(&mut self.geometry_retry, page, now) => {}
                Err(e) => {
                    // Keep the estimate but stop asking; show the error.
                    self.geometry_retry.remove(&page);
                    self.geometry_failed.insert(page, e.clone());
                    let estimate = self.layout.page_size(page).unwrap_or(PageSize::LETTER);
                    updates.push((page, estimate));
                    self.page_errors.push((page, e));
                }
            }
        }
        if updates.is_empty() {
            return;
        }
        // Newly known pages need planning even when their size matched the
        // estimate, so the version always moves.
        self.layout_version += 1;
        if self.layout.set_page_sizes(updates) {
            // Zoom deliberately stays put: a fit-width zoom that jumped while
            // scrolling past a wider page would be disorienting. Fit modes
            // are re-applied on resize and on explicit commands only.
            self.viewport.scroll_y = self.layout.y_for_anchor(anchor);
            self.viewport.clamp_scroll(&self.layout);
        }
    }

    fn build_frame(
        &self,
        plan: &[fastpdf_render::PlannedTile],
        ready: &mut dyn FnMut(&V) -> bool,
    ) -> Frame<V> {
        let visible = self.viewport.visible_rect();
        let pages: Vec<FramePage> = {
            let range = self.layout.pages_intersecting(visible.y, visible.bottom());
            range
                .map(PageIndex::new)
                .filter_map(|page| {
                    let rect = self.layout.page_rect(page)?;
                    let cause = self
                        .page_errors
                        .iter()
                        .find(|(p, _)| *p == page)
                        .map(|(_, e)| e.clone());
                    Some(FramePage {
                        page,
                        rect: self.to_view(&rect),
                        error: cause.as_ref().map(ToString::to_string),
                        cause,
                    })
                })
                .collect()
        };

        let mut exact = Vec::new();
        let mut standins = Vec::new();
        let mut seen_standins = HashSet::new();
        let mut visible_keys = Vec::new();
        let mut visible_bytes = 0;
        let mut pending = 0;
        let mut deferred = 0;
        let gutter = self.config.tile_gutter;
        for tile in plan.iter().filter(|t| t.priority == Priority::Visible) {
            let Some((page_rect, grid)) = self.page_geometry(tile.key.page.page, tile.key.bucket)
            else {
                continue;
            };
            let Some((rendered, inner)) = grid.rendered_region(tile.key.coord, gutter) else {
                continue;
            };
            if let Some(image) = self.tiles.with(&tile.key, Clone::clone) {
                visible_keys.push(tile.key);
                visible_bytes += rendered.width as usize * rendered.height as usize * 4;
                if ready(&image) {
                    exact.push(FrameTile {
                        key: tile.key,
                        image,
                        rect: self.pixels_to_view(&page_rect, &grid, rect_f32(inner)),
                        src: src_rect(rendered, inner),
                        exact: true,
                    });
                    continue;
                }
                deferred += 1;
            } else if !self.failed.contains(&tile.key) {
                pending += 1;
            }
            let page = tile.key.page.page;
            let fallbacks = self.tiles.fallbacks(&tile.key, |bucket| {
                self.page_geometry(page, bucket).map(|(_, g)| g)
            });
            let best = fallbacks
                .into_iter()
                .find(|f| !seen_standins.contains(&f.key));
            if let Some(best) = best
                && let Some(image) = self.tiles.with(&best.key, Clone::clone)
                && let Some((_, fb_grid)) = self.page_geometry(page, best.key.bucket)
                && let Some((fb_rendered, fb_inner)) =
                    fb_grid.rendered_region(best.key.coord, gutter)
                && ready(&image)
            {
                seen_standins.insert(best.key);
                standins.push(FrameTile {
                    key: best.key,
                    image,
                    rect: self.pixels_to_view(&page_rect, &grid, best.dest),
                    src: src_rect(fb_rendered, fb_inner),
                    exact: false,
                });
                visible_keys.push(best.key);
            }
        }
        self.tiles.mark_visible(visible_keys.iter(), visible_bytes);
        standins.extend(exact);
        Frame {
            pages,
            tiles: standins,
            pending,
            deferred,
        }
    }

    /// Layout rectangle and tile grid of a page at a bucket.
    fn page_geometry(
        &self,
        page: PageIndex,
        bucket: ScaleBucket,
    ) -> Option<(LayoutRect, TileGrid)> {
        let info = self.known_page_info(page)?;
        let rect = self.layout.page_rect(page)?;
        let px = bucket
            .render_scale()
            .page_pixels(info.size, info.rotation.then(self.rotation));
        Some((rect, TileGrid::new(px, self.config.tile_size)))
    }

    /// Maps `[x, y, w, h]` in a page's pixel space (of `grid`) to view
    /// pixels, snapped to the device pixel grid so adjacent tiles meet
    /// without seams.
    fn pixels_to_view(&self, page: &LayoutRect, grid: &TileGrid, r: [f32; 4]) -> [f32; 4] {
        let px = grid.page_pixels();
        let sx = page.width / f64::from(px.width);
        let sy = page.height / f64::from(px.height);
        let x0 = page.x + f64::from(r[0]) * sx;
        let y0 = page.y + f64::from(r[1]) * sy;
        let x1 = page.x + f64::from(r[0] + r[2]) * sx;
        let y1 = page.y + f64::from(r[1] + r[3]) * sy;
        let [vx0, vy0] = self.layout_to_view(x0, y0);
        let [vx1, vy1] = self.layout_to_view(x1, y1);
        [vx0, vy0, vx1 - vx0, vy1 - vy0]
    }

    /// Layout rectangle, geometry and total rotation of a known page.
    fn page_frame(&self, page: PageIndex) -> Option<(LayoutRect, PageInfo, Rotation)> {
        let info = self.known_page_info(page)?;
        let rect = self.layout.page_rect(page)?;
        Some((rect, info, info.rotation.then(self.rotation)))
    }

    fn to_view(&self, rect: &LayoutRect) -> [f32; 4] {
        let [x0, y0] = self.layout_to_view(rect.x, rect.y);
        let [x1, y1] = self.layout_to_view(rect.right(), rect.bottom());
        [x0, y0, x1 - x0, y1 - y0]
    }

    fn layout_to_view(&self, x: f64, y: f64) -> [f32; 2] {
        let ppp = self.viewport.px_per_point();
        let ds = f64::from(self.viewport.device_scale);
        let snap = |v: f64| ((v * ds).round() / ds) as f32;
        [
            snap((x - self.viewport.scroll_x) * ppp),
            snap((y - self.viewport.scroll_y) * ppp),
        ]
    }
}

/// Page space (unrotated, `size`) to displayed page space after a
/// clockwise `rotation`.
fn rotate_point(x: f32, y: f32, size: PageSize, rotation: Rotation) -> (f32, f32) {
    let (w, h) = (size.width, size.height);
    match rotation {
        Rotation::R0 => (x, y),
        Rotation::R90 => (h - y, x),
        Rotation::R180 => (w - x, h - y),
        Rotation::R270 => (y, w - x),
    }
}

/// Inverse of [`rotate_point`].
fn unrotate_point(dx: f32, dy: f32, size: PageSize, rotation: Rotation) -> (f32, f32) {
    let (w, h) = (size.width, size.height);
    match rotation {
        Rotation::R0 => (dx, dy),
        Rotation::R90 => (dy, h - dx),
        Rotation::R180 => (w - dx, h - dy),
        Rotation::R270 => (w - dy, dx),
    }
}

fn rect_f32(r: fastpdf_engine_api::PixelRect) -> [f32; 4] {
    [r.x as f32, r.y as f32, r.width as f32, r.height as f32]
}

/// The inner tile rectangle relative to the rendered (guttered) bitmap.
fn src_rect(
    rendered: fastpdf_engine_api::PixelRect,
    inner: fastpdf_engine_api::PixelRect,
) -> [f32; 4] {
    [
        (inner.x - rendered.x) as f32,
        (inner.y - rendered.y) as f32,
        inner.width as f32,
        inner.height as f32,
    ]
}

/// Records a transient failure of `key`; `true` while it will be retried,
/// `false` once its attempts are used up (the error is final then).
fn note_failure<K: Hash + Eq + Copy>(
    retries: &mut HashMap<K, Backoff>,
    key: K,
    now: Instant,
) -> bool {
    match Backoff::after_failure(retries.get(&key).copied(), now) {
        Some(backoff) => {
            retries.insert(key, backoff);
            true
        }
        None => {
            retries.remove(&key);
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fastpdf_engine_api::{
        CancelToken, DocumentSource, EngineCapabilities, EngineInfo, OpenOptions, PdfEngine,
        PixmapMut, RenderOutcome, RenderRequest, SharedBytes, open_guarded,
    };
    use std::sync::atomic::AtomicUsize;
    use std::time::{Duration, Instant};

    /// Letter pages; page 2 is landscape; page 4 fails to render.
    struct Engine;
    struct Doc {
        renders: Arc<AtomicUsize>,
    }

    impl PdfEngine for Engine {
        fn info(&self) -> EngineInfo {
            EngineInfo {
                name: "test",
                version: "0",
                capabilities: EngineCapabilities {
                    parallel_render: true,
                    region_render: true,
                    ..EngineCapabilities::default()
                },
            }
        }
        fn open(
            &self,
            _: DocumentSource,
            _: &OpenOptions,
        ) -> Result<Box<dyn EngineDocument>, EngineError> {
            Ok(Box::new(Doc {
                renders: RENDERS.with(Arc::clone),
            }))
        }
    }

    impl EngineDocument for Doc {
        fn page_count(&self) -> u32 {
            50
        }
        fn page_info(&self, page: PageIndex) -> Result<PageInfo, EngineError> {
            let size = if page.get() == 2 {
                PageSize::new(792.0, 612.0)
            } else {
                PageSize::LETTER
            };
            Ok(PageInfo {
                size,
                rotation: Rotation::R0,
            })
        }
        fn render(
            &self,
            request: &RenderRequest,
            target: &mut PixmapMut<'_>,
            _: &CancelToken,
        ) -> Result<RenderOutcome, EngineError> {
            self.renders.fetch_add(1, Ordering::Relaxed);
            if request.page.get() == 4 {
                return Err(EngineError::Malformed("bad page".into()));
            }
            target.fill(request.background);
            Ok(RenderOutcome::default())
        }
    }

    impl<V: Clone + Send + 'static> DocumentSession<V> {
        fn doc_renders(&self) -> Arc<AtomicUsize> {
            RENDERS.with(Arc::clone)
        }
    }

    thread_local! {
        static RENDERS: Arc<AtomicUsize> = Arc::new(AtomicUsize::new(0));
    }

    fn session() -> DocumentSession<Arc<Pixmap>> {
        let doc = open_guarded(
            &Engine,
            DocumentSource::from_bytes(SharedBytes::from_vec(vec![0])),
            &OpenOptions::default(),
        )
        .unwrap();
        DocumentSession::new(
            Arc::new(doc),
            SessionConfig {
                workers: 2,
                ..SessionConfig::default()
            },
            (1280.0, 720.0),
            1.0,
            Arc::new,
            || {},
            |_| {},
        )
    }

    /// Calls `frame` until no visible tile is pending.
    fn settle(s: &mut DocumentSession<Arc<Pixmap>>) -> Frame<Arc<Pixmap>> {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let frame = s.frame();
            if frame.pending == 0 || Instant::now() > deadline {
                return frame;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn opens_fit_width_and_fills_the_view() {
        let mut s = session();
        assert_eq!(s.zoom_mode(), ZoomMode::FitWidth);
        let first = s.frame();
        assert!(
            first.pending > 0,
            "nothing is rendered before the first frame"
        );
        let frame = settle(&mut s);
        assert_eq!(frame.pending, 0);
        assert!(frame.tiles.iter().all(|t| t.exact));
        assert_eq!(frame.pages[0].page, PageIndex::FIRST);
        // Tiles cover the page width exactly (fit width, no seams).
        let page = frame.pages[0].rect;
        let right = frame
            .tiles
            .iter()
            .map(|t| t.rect[0] + t.rect[2])
            .fold(0.0, f32::max);
        assert!(
            (right - (page[0] + page[2])).abs() <= 1.0,
            "{right} vs {page:?}"
        );
    }

    #[test]
    fn tiles_carry_keys_and_inner_source_rects() {
        let mut s = session();
        let frame = settle(&mut s);
        let t = &frame.tiles[0];
        // The first tile touches the page's top-left corner: no gutter there,
        // 2 px on the right and bottom.
        assert_eq!(t.src, [0.0, 0.0, 512.0, 512.0]);
        assert_eq!(t.image.size().width, 514);
        assert!(frame.tiles.iter().all(|t| t.key.page.document == s.id()));
    }

    #[test]
    fn images_that_are_not_ready_are_deferred_to_stand_ins() {
        let mut s = session();
        settle(&mut s);
        // Nothing may be drawn this frame (upload budget spent).
        let frame = s.frame_with(|_| false);
        assert!(frame.deferred > 0);
        assert!(frame.tiles.is_empty());
        assert_eq!(frame.pending, 0);
        // Next frame everything is ready again.
        let frame = s.frame();
        assert_eq!(frame.deferred, 0);
        assert!(!frame.tiles.is_empty());
    }

    #[test]
    fn close_releases_every_cached_tile() {
        let doc = open_guarded(
            &Engine,
            DocumentSource::from_bytes(SharedBytes::from_vec(vec![0])),
            &OpenOptions::default(),
        )
        .unwrap();
        let evicted = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&evicted);
        let mut s = DocumentSession::new(
            Arc::new(doc),
            SessionConfig::default(),
            (1280.0, 720.0),
            1.0,
            Arc::new,
            || {},
            move |e: Vec<(TileKey, Arc<Pixmap>)>| {
                counter.fetch_add(e.len(), Ordering::Relaxed);
            },
        );
        let frame = settle(&mut s);
        let shown = frame.tiles.len();
        assert!(shown > 0);
        s.close();
        assert!(evicted.load(Ordering::Relaxed) >= shown);
        assert_eq!(s.stats().tile_entries, 0);
    }

    #[test]
    fn rotation_mapping_round_trips() {
        let size = PageSize::new(600.0, 800.0);
        for r in [Rotation::R0, Rotation::R90, Rotation::R180, Rotation::R270] {
            let (dx, dy) = rotate_point(100.0, 50.0, size, r);
            let (x, y) = unrotate_point(dx, dy, size, r);
            assert!((x - 100.0).abs() < 1e-4 && (y - 50.0).abs() < 1e-4, "{r:?}");
        }
        // Clockwise: the page's top-left corner ends up top-right.
        assert_eq!(rotate_point(0.0, 0.0, size, Rotation::R90), (800.0, 0.0));
        assert_eq!(rotate_point(0.0, 0.0, size, Rotation::R270), (0.0, 600.0));
    }

    #[test]
    fn page_and_view_coordinates_agree() {
        let mut s = session();
        s.frame();
        // The page's top-left text box shows at the page's top-left on screen.
        let page = s.frame().pages[0].rect;
        let r = s
            .page_to_view(PageIndex::FIRST, PageRect::new(0.0, 0.0, 61.2, 79.2))
            .unwrap();
        assert!((r[0] - page[0]).abs() < 1.0 && (r[1] - page[1]).abs() < 1.0);
        assert!((r[2] - page[2] / 10.0).abs() < 1.0);
        // And clicking there maps back into page space.
        let (p, x, y) = s.view_to_page(r[0] + 1.0, r[1] + 1.0).unwrap();
        assert_eq!(p, PageIndex::FIRST);
        assert!(x < 5.0 && y < 5.0);

        // After a clockwise rotation the page's top-left is the view's top-right.
        s.rotate_clockwise();
        s.go_to_page(PageIndex::FIRST);
        s.frame();
        let page = s.frame().pages[0].rect;
        let r = s
            .page_to_view(PageIndex::FIRST, PageRect::new(0.0, 0.0, 10.0, 10.0))
            .unwrap();
        assert!(
            (r[0] + r[2] - (page[0] + page[2])).abs() < 1.0,
            "{r:?} {page:?}"
        );
        assert!((r[1] - page[1]).abs() < 1.0);
        let (_, x, y) = s.view_to_page(r[0] + r[2] - 1.0, r[1] + 1.0).unwrap();
        assert!(x < 10.0 && y < 10.0, "{x} {y}");
        // Points in the gap between pages hit nothing.
        assert!(s.view_to_page(page[0] - 3.0, page[1] + 5.0).is_none());
    }

    #[test]
    fn destinations_scroll_to_their_top() {
        let mut s = session();
        s.frame();
        s.go_to_destination(&Destination {
            page: PageIndex::new(5),
            view: DestinationView::Xyz {
                left: None,
                top: Some(400.0),
                zoom: None,
            },
        });
        let top = s.layout().page_rect(PageIndex::new(5)).unwrap().y;
        assert!((s.viewport().scroll_y - (top + 400.0)).abs() < 1e-6);
        assert_eq!(s.current_page(), PageIndex::new(5));
    }

    #[test]
    fn color_mode_switch_renders_inverted_tiles() {
        let mut s = session();
        let normal = settle(&mut s);
        assert_eq!(&normal.tiles[0].image.data()[..4], &[255, 255, 255, 255]);
        s.set_color_mode(ColorMode::Inverted);
        let inverted = settle(&mut s);
        assert!(
            inverted
                .tiles
                .iter()
                .all(|t| t.key.color == ColorMode::Inverted)
        );
        assert_eq!(&inverted.tiles[0].image.data()[..4], &[0, 0, 0, 255]);
        // Switching back reuses the cached normal tiles: nothing pending.
        s.set_color_mode(ColorMode::Normal);
        assert_eq!(s.frame().pending, 0);
    }

    #[test]
    fn navigation_moves_between_pages() {
        let mut s = session();
        s.frame();
        s.next_page();
        assert_eq!(s.current_page(), PageIndex::new(1));
        s.last_page();
        s.frame();
        assert_eq!(s.current_page(), PageIndex::new(49));
        s.prev_page();
        assert_eq!(s.current_page(), PageIndex::new(48));
        s.first_page();
        assert_eq!(s.current_page(), PageIndex::FIRST);
    }

    #[test]
    fn zoom_shows_stand_ins_until_exact_tiles_arrive() {
        let mut s = session();
        settle(&mut s);
        s.set_zoom(s.zoom().zoom_in().zoom_in(), None);
        let frame = s.frame();
        assert!(frame.pending > 0);
        assert!(
            frame.tiles.iter().any(|t| !t.exact),
            "old tiles stand in during zoom"
        );
        let frame = settle(&mut s);
        assert!(frame.tiles.iter().all(|t| t.exact));
    }

    #[test]
    fn broken_pages_report_errors_instead_of_crashing() {
        let mut s = session();
        s.go_to_page(PageIndex::new(4));
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut frame = s.frame();
        while Instant::now() < deadline
            && !frame
                .pages
                .iter()
                .any(|p| p.page.get() == 4 && p.error.is_some())
        {
            std::thread::sleep(Duration::from_millis(2));
            frame = s.frame();
        }
        assert!(
            frame
                .pages
                .iter()
                .any(|p| p.page.get() == 4 && p.error.is_some())
        );
    }

    #[test]
    fn page_sizes_resolve_lazily_with_stable_scroll() {
        let mut s = session();
        s.frame();
        assert!(!s.layout().is_known(PageIndex::new(40)));
        s.go_to_page(PageIndex::new(3));
        s.frame();
        // The landscape page 2 is now known; page 3 is still at the top.
        assert!(s.layout().is_known(PageIndex::new(2)));
        assert_eq!(s.current_page(), PageIndex::new(3));
    }

    #[test]
    fn thumbnails_are_lazy_and_limited_to_the_visible_rows() {
        let mut s = session();
        // Disabled: nothing is returned and nothing is rendered.
        assert!(s.thumbnails(0..4, 2).is_empty());
        let renders = s.doc_renders();
        assert_eq!(renders.load(Ordering::Relaxed), 0);

        let evicted = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&evicted);
        s.enable_thumbnails(160, 32 << 20, move |e| {
            counter.fetch_add(e.len(), Ordering::Relaxed);
        });
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut items = s.thumbnails(0..4, 2);
        while items.iter().any(|t| t.image.is_none()) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(2));
            items = s.thumbnails(0..4, 2);
        }
        assert_eq!(items.len(), 4);
        assert!(items.iter().all(|t| t.image.is_some()));
        // Page 2 is landscape: 160 x round(160 * 612 / 792).
        assert_eq!(items[2].size, [160, 124]);
        assert_eq!(items[0].image.as_ref().unwrap().size().width, 160);
        // Only the visible rows plus the margin (pages 0..6) were rendered,
        // never all 50 pages.
        let deadline = Instant::now() + Duration::from_secs(10);
        while !s.scheduler.is_lane_idle(Lane::Thumbnails) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(renders.load(Ordering::Relaxed) <= 6, "{renders:?}");

        // Page 4 cannot be rendered by the test engine: reported, not retried.
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut broken = s.thumbnails(4..5, 0);
        while !broken[0].failed && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(2));
            broken = s.thumbnails(4..5, 0);
        }
        assert!(broken[0].failed && broken[0].image.is_none());

        s.disable_thumbnails();
        assert!(!s.thumbnails_enabled());
        assert!(evicted.load(Ordering::Relaxed) >= 4);
    }

    #[test]
    fn rotation_rebuilds_layout_and_keeps_the_page() {
        let mut s = session();
        s.go_to_page(PageIndex::new(10));
        s.rotate_clockwise();
        assert_eq!(s.rotation(), Rotation::R90);
        assert_eq!(s.current_page(), PageIndex::new(10));
        let size = s.layout().page_size(PageIndex::new(10)).unwrap();
        assert!(size.width > size.height);
        let frame = settle(&mut s);
        assert_eq!(frame.pending, 0);
    }

    /// The UI builds the command-line document's first session on its open
    /// thread while the toolkit starts (`fastpdf_ui` start-up) and moves it
    /// to the UI thread afterwards, so a session must be `Send` for every
    /// view type the sessions accept, not just the one the UI uses.
    #[test]
    fn sessions_can_move_between_threads() {
        fn assert_send<T: Send>() {}
        fn for_any_view<V: Clone + Send + 'static>() {
            assert_send::<DocumentSession<V>>();
        }
        for_any_view::<Arc<Vec<u8>>>();
    }

    /// Page 1's geometry is unavailable twice and the first three renders
    /// lose their host before the engine answers normally.
    struct FlakyEngine;
    struct FlakyDoc {
        geometry_failures: AtomicUsize,
        render_failures: AtomicUsize,
    }

    impl PdfEngine for FlakyEngine {
        fn info(&self) -> EngineInfo {
            Engine.info()
        }
        fn open(
            &self,
            _: DocumentSource,
            _: &OpenOptions,
        ) -> Result<Box<dyn EngineDocument>, EngineError> {
            Ok(Box::new(FlakyDoc {
                geometry_failures: AtomicUsize::new(2),
                render_failures: AtomicUsize::new(3),
            }))
        }
    }

    fn take_one(counter: &AtomicUsize) -> bool {
        let mut n = counter.load(Ordering::Acquire);
        while n > 0 {
            match counter.compare_exchange(n, n - 1, Ordering::AcqRel, Ordering::Acquire) {
                Ok(_) => return true,
                Err(actual) => n = actual,
            }
        }
        false
    }

    impl EngineDocument for FlakyDoc {
        fn page_count(&self) -> u32 {
            4
        }
        fn page_info(&self, page: PageIndex) -> Result<PageInfo, EngineError> {
            if page.get() == 1 && take_one(&self.geometry_failures) {
                return Err(EngineError::Unavailable("host restarting".into()));
            }
            let size = if page.get() == 1 {
                PageSize::new(792.0, 612.0)
            } else {
                PageSize::LETTER
            };
            Ok(PageInfo {
                size,
                rotation: Rotation::R0,
            })
        }
        fn render(
            &self,
            request: &RenderRequest,
            target: &mut PixmapMut<'_>,
            _: &CancelToken,
        ) -> Result<RenderOutcome, EngineError> {
            if take_one(&self.render_failures) {
                return Err(EngineError::HostExited(fastpdf_engine_api::HostExit {
                    reason: fastpdf_engine_api::HostExitReason::Crashed { exit_code: None },
                    permanent: false,
                }));
            }
            target.fill(request.background);
            Ok(RenderOutcome::default())
        }
    }

    #[test]
    fn transient_failures_recover_by_themselves_without_errors() {
        let doc = open_guarded(
            &FlakyEngine,
            DocumentSource::from_bytes(SharedBytes::from_vec(vec![0])),
            &OpenOptions::default(),
        )
        .unwrap();
        let wakes = Arc::new(AtomicUsize::new(0));
        let w = Arc::clone(&wakes);
        let mut s: DocumentSession<Arc<Pixmap>> = DocumentSession::new(
            Arc::new(doc),
            SessionConfig {
                workers: 1,
                ..SessionConfig::default()
            },
            (900.0, 2400.0),
            1.0,
            Arc::new,
            move || {
                w.fetch_add(1, Ordering::SeqCst);
            },
            |_| {},
        );
        let first = s.frame();
        assert!(first.pages.iter().all(|p| p.error.is_none()));
        assert!(!s.layout().is_known(PageIndex::new(1)), "estimate kept");
        assert!(s.stats().pending_retries > 0);
        // No input from here on: only the session's wake hook drives frames.
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut seen = 0;
        let frame = loop {
            assert!(
                Instant::now() < deadline,
                "did not recover: {:?}",
                s.stats()
            );
            let now = wakes.load(Ordering::SeqCst);
            if now == seen {
                std::thread::sleep(Duration::from_millis(5));
                continue;
            }
            seen = now;
            let frame = s.frame();
            assert!(
                frame.pages.iter().all(|p| p.error.is_none()),
                "{:?}",
                frame.pages
            );
            if frame.pending == 0 && s.stats().pending_retries == 0 {
                break frame;
            }
        };
        assert!(frame.tiles.iter().all(|t| t.exact));
        assert_eq!(
            s.layout().page_size(PageIndex::new(1)),
            Some(PageSize::new(792.0, 612.0))
        );
    }

    fn flaky_session() -> DocumentSession<Arc<Pixmap>> {
        let doc = open_guarded(
            &FlakyEngine,
            DocumentSource::from_bytes(SharedBytes::from_vec(vec![0])),
            &OpenOptions::default(),
        )
        .unwrap();
        DocumentSession::new(
            Arc::new(doc),
            SessionConfig {
                workers: 1,
                ..SessionConfig::default()
            },
            (900.0, 2400.0),
            1.0,
            Arc::new,
            || {},
            |_| {},
        )
    }

    /// A retry that is already due but not needed (its page scrolled away)
    /// must not keep the retry clock firing: every wake would only repeat
    /// the same frame, and idle CPU would never return to zero.
    #[test]
    fn retries_already_due_do_not_wake_the_ui_again() {
        let mut s = flaky_session();
        s.geometry_retry.insert(
            PageIndex::new(3),
            Backoff {
                attempts: 1,
                next: Instant::now() - Duration::from_secs(1),
            },
        );
        s.schedule_retries();
        assert!(
            !s.retry_clock.is_running(),
            "nothing in the future to wait for"
        );
        s.geometry_retry.insert(
            PageIndex::new(2),
            Backoff {
                attempts: 1,
                next: Instant::now() + Duration::from_secs(5),
            },
        );
        s.schedule_retries();
        assert!(s.retry_clock.is_running(), "a future retry is scheduled");
    }

    /// The last failed retry of a page's geometry while listing thumbnails
    /// is final: later lists neither ask the engine again nor start a new
    /// round of retries, and the page view shows the error without asking.
    #[test]
    fn thumbnail_geometry_failures_stay_final() {
        let mut s = flaky_session();
        s.enable_thumbnails(120, 1 << 20, |_| {});
        // Page 1's geometry fails twice (FlakyDoc); pretend this is the
        // last allowed attempt.
        s.geometry_retry.insert(
            PageIndex::new(1),
            Backoff {
                attempts: crate::retry::MAX_ATTEMPTS - 1,
                next: Instant::now() - Duration::from_millis(1),
            },
        );
        let items = s.thumbnails(0..4, 0);
        assert!(items[1].failed, "the last attempt failed: final");
        assert!(s.geometry_failed.contains_key(&PageIndex::new(1)));
        // FlakyDoc would answer the next request: it must not be asked.
        let items = s.thumbnails(0..4, 0);
        assert!(items[1].failed, "still failed, not retried");
        assert!(!s.geometry_retry.contains_key(&PageIndex::new(1)));
        let frame = s.frame();
        let page = frame
            .pages
            .iter()
            .find(|p| p.page == PageIndex::new(1))
            .expect("page 1 is visible");
        assert!(page.error.is_some(), "the page view shows the final error");
        assert!(
            !s.geometry_retry.contains_key(&PageIndex::new(1)),
            "no new round"
        );
    }

    #[test]
    fn retries_give_up_after_the_last_attempt() {
        let mut retries = HashMap::new();
        let t = Instant::now();
        let mut kept = 0;
        while note_failure(&mut retries, 7u32, t) {
            kept += 1;
            assert!(kept < 100);
        }
        assert_eq!(kept, crate::retry::MAX_ATTEMPTS - 1);
        assert!(retries.is_empty());
    }
}
