//! A document opened in a view: layout, viewport, navigation, and the tile
//! pipeline, independent of the UI toolkit.
//!
//! The UI calls [`DocumentSession::frame`] when it paints and draws the
//! returned [`Frame`]: page backgrounds, exact tiles, and lower/higher
//! resolution stand-ins for tiles that are still rendering (spec §13: the
//! view never waits for a render). Everything expensive happens on the
//! render scheduler's worker threads; `frame` only does bookkeeping.

use std::collections::HashSet;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};

use fastpdf_engine_api::{
    DocumentId, EngineDocument, EngineError, GuardedDocument, PageIndex, PageInfo, PageSize,
    PixelFormat, Pixmap, Rgba8, Rotation,
};
use fastpdf_render::{
    DocumentLayout, LayoutRect, PlanConfig, Priority, RenderScheduler, ScaleBucket,
    SchedulerConfig, SchedulerStats, TileCache, TileGrid, TileKey, Viewport, ZoomLevel, plan_tiles,
};

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
}

/// An image to draw, already positioned.
#[derive(Debug, Clone, PartialEq)]
pub struct FrameTile<V> {
    pub image: V,
    pub rect: [f32; 4],
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
}

enum Incoming<V> {
    Tile {
        key: TileKey,
        image: V,
        bytes: usize,
    },
    Failed {
        key: TileKey,
        error: EngineError,
    },
}

/// Inputs that require a new render plan when they change.
#[derive(Debug, Clone, PartialEq)]
struct PlanInputs {
    viewport: Viewport,
    rotation: Rotation,
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
    zoom_mode: ZoomMode,
    scheduler: RenderScheduler,
    incoming: Receiver<Incoming<V>>,
    wake_pending: Arc<AtomicBool>,
    tiles: TileCache<V>,
    failed: HashSet<TileKey>,
    page_errors: Vec<(PageIndex, String)>,
    planned: Option<PlanInputs>,
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
        let pending = Arc::clone(&wake_pending);
        let sink = move |result: fastpdf_render::TileResult| {
            let msg = match result.result {
                Ok(pixmap) => {
                    let bytes = pixmap.byte_len();
                    Incoming::Tile {
                        key: result.key,
                        image: convert(pixmap),
                        bytes,
                    }
                }
                Err(error) => Incoming::Failed {
                    key: result.key,
                    error,
                },
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
            zoom_mode: ZoomMode::FitWidth,
            scheduler,
            incoming,
            wake_pending,
            tiles,
            failed: HashSet::new(),
            page_errors: Vec::new(),
            planned: None,
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
        self.wake_pending.store(false, Ordering::Release);
        self.drain_incoming();

        let visible = self.viewport.visible_rect();
        let near = visible.expand_y(visible.height * f64::from(self.config.near_margin));
        let range = self.layout.pages_intersecting(near.y, near.bottom());
        self.resolve_pages(range);

        let inputs = PlanInputs {
            viewport: self.viewport,
            rotation: self.rotation,
            layout_version: self.layout_version,
        };
        let plan = plan_tiles(
            self.id,
            &self.layout,
            &self.viewport,
            |p| self.known_page_info(p),
            &self.plan_config(),
        );

        let frame = self.build_frame(&plan);
        // Tiles evicted after planning would otherwise never come back.
        if frame.pending > 0 && self.scheduler.is_idle() {
            self.planned = None;
        }
        if self.planned.as_ref() != Some(&inputs) {
            let needed: Vec<_> = plan
                .into_iter()
                .filter(|t| !self.tiles.contains(&t.key) && !self.failed.contains(&t.key))
                .collect();
            self.scheduler.submit_plan(needed);
            self.planned = Some(inputs);
        }
        frame
    }

    pub fn stats(&self) -> SessionStats {
        let t = self.tiles.stats();
        SessionStats {
            scheduler: self.scheduler.stats(),
            tile_bytes: t.bytes,
            tile_entries: t.entries,
            tile_hits: t.hits,
            tile_misses: t.misses,
            tile_evictions: t.evictions,
        }
    }

    fn plan_config(&self) -> PlanConfig {
        PlanConfig {
            tile_size: self.config.tile_size,
            near_margin: self.config.near_margin,
            prefetch_next_page: true,
            rotation: self.rotation,
            background: self.config.paper,
            ..PlanConfig::default()
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
                    // Results for an old rotation are useless now.
                    if key.rotation == self.rotation {
                        self.tiles.insert(key, image, bytes);
                    }
                }
                Incoming::Failed { key, error } => {
                    self.failed.insert(key);
                    let page = key.page.page;
                    if !self.page_errors.iter().any(|(p, _)| *p == page) {
                        self.page_errors.push((page, error.to_string()));
                    }
                }
            }
        }
    }

    /// Resolves real sizes for `pages`, keeping the view anchored.
    fn resolve_pages(&mut self, pages: std::ops::Range<u32>) {
        let unknown: Vec<PageIndex> = pages
            .map(PageIndex::new)
            .filter(|p| !self.layout.is_known(*p))
            .collect();
        if unknown.is_empty() {
            return;
        }
        let anchor = self.layout.anchor_at(self.viewport.scroll_y);
        let mut updates = Vec::with_capacity(unknown.len());
        for page in unknown {
            match self.doc.page_info(page) {
                Ok(info) => updates.push((page, info.display_size(self.rotation))),
                Err(e) => {
                    // Keep the estimate but stop asking; show the error.
                    let estimate = self.layout.page_size(page).unwrap_or(PageSize::LETTER);
                    updates.push((page, estimate));
                    self.page_errors.push((page, e.to_string()));
                }
            }
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

    fn build_frame(&self, plan: &[fastpdf_render::PlannedTile]) -> Frame<V> {
        let visible = self.viewport.visible_rect();
        let pages: Vec<FramePage> = {
            let range = self.layout.pages_intersecting(visible.y, visible.bottom());
            range
                .map(PageIndex::new)
                .filter_map(|page| {
                    let rect = self.layout.page_rect(page)?;
                    Some(FramePage {
                        page,
                        rect: self.to_view(&rect),
                        error: self
                            .page_errors
                            .iter()
                            .find(|(p, _)| *p == page)
                            .map(|(_, e)| e.clone()),
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
        for tile in plan.iter().filter(|t| t.priority == Priority::Visible) {
            let Some((page_rect, grid)) = self.page_geometry(tile.key.page.page, tile.key.bucket)
            else {
                continue;
            };
            let region = tile.request.region;
            let bytes = region.width as usize * region.height as usize * 4;
            if let Some(image) = self.tiles.with(&tile.key, Clone::clone) {
                exact.push(FrameTile {
                    image,
                    rect: self.pixels_to_view(
                        &page_rect,
                        &grid,
                        [
                            region.x as f32,
                            region.y as f32,
                            region.width as f32,
                            region.height as f32,
                        ],
                    ),
                    exact: true,
                });
                visible_keys.push(tile.key);
                visible_bytes += bytes;
                continue;
            }
            if !self.failed.contains(&tile.key) {
                pending += 1;
            }
            let page = tile.key.page.page;
            let fallbacks = self.tiles.fallbacks(&tile.key, |bucket| {
                self.page_geometry(page, bucket).map(|(_, g)| g)
            });
            if let Some(best) = fallbacks.into_iter().next()
                && seen_standins.insert(best.key)
                && let Some(image) = self.tiles.with(&best.key, Clone::clone)
            {
                standins.push(FrameTile {
                    image,
                    rect: self.pixels_to_view(&page_rect, &grid, best.dest),
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
                renders: Arc::new(AtomicUsize::new(0)),
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
}
