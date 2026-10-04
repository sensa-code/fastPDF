use fastpdf_engine_api::{
    ColorMode, DocumentId, PageId, PageIndex, PageInfo, PixelRect, RenderRequest, Rgba8, Rotation,
};

use crate::{DEFAULT_TILE_SIZE, DocumentLayout, LayoutRect, TileGrid, TileKey, Viewport};

/// Render priority classes (spec §14). Lower values run first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Priority {
    /// P0: tiles currently on screen.
    Visible = 0,
    /// P1: off-screen tiles of pages that are partly visible.
    VisiblePage = 1,
    /// P2: tiles of other pages within the near margin.
    Near = 2,
    /// P3: the beginning of the next page.
    Prefetch = 3,
    /// P4: sidebar thumbnails.
    Thumbnail = 4,
    /// P5: search and other background work.
    Background = 5,
}

/// Knobs for [`plan_tiles`].
#[derive(Debug, Clone, PartialEq)]
pub struct PlanConfig {
    pub tile_size: u32,
    /// Extra area above and below the view, in view heights, rendered ahead
    /// of scrolling (P1/P2).
    pub near_margin: f32,
    /// Render the first screen of the page after the near area (P3).
    pub prefetch_next_page: bool,
    pub rotation: Rotation,
    pub color: ColorMode,
    pub background: Rgba8,
    /// Extra pixels rendered around each tile (see
    /// [`TileGrid::rendered_region`]); the request region includes them.
    pub gutter: u32,
}

impl Default for PlanConfig {
    fn default() -> Self {
        Self {
            tile_size: DEFAULT_TILE_SIZE,
            near_margin: 0.5,
            prefetch_next_page: true,
            rotation: Rotation::R0,
            color: ColorMode::Normal,
            background: Rgba8::WHITE,
            gutter: 0,
        }
    }
}

/// One unit of work for the render scheduler: what to render, how urgent
/// it is, and the key its result is stored under.
#[derive(Debug, Clone, PartialEq)]
pub struct RenderJob<K> {
    pub key: K,
    pub priority: Priority,
    /// Distance from the view center in layout points; orders jobs within
    /// a priority class.
    pub distance: f32,
    pub request: RenderRequest,
}

/// A tile the document view wants.
pub type PlannedTile = RenderJob<TileKey>;

/// Lists the tiles needed for `viewport`, highest priority first.
///
/// Only pages whose [`PageInfo`] is known are planned; `page_info` should
/// return `None` for pages that have not been resolved yet. Nothing outside
/// the near margin is ever planned ("Never render what the user cannot
/// see", spec §1).
pub fn plan_tiles(
    document: DocumentId,
    layout: &DocumentLayout,
    viewport: &Viewport,
    page_info: impl Fn(PageIndex) -> Option<PageInfo>,
    config: &PlanConfig,
) -> Vec<PlannedTile> {
    let visible = viewport.visible_rect();
    let near = visible.expand_y(visible.height * f64::from(config.near_margin.max(0.0)));
    let center_y = visible.center_y();
    let bucket = viewport.bucket();
    let scale = bucket.render_scale();
    let mut out = Vec::new();

    let near_pages = layout.pages_intersecting(near.y, near.bottom());
    let visible_pages = layout.pages_intersecting(visible.y, visible.bottom());

    let mut plan_area =
        |page: PageIndex, area: &LayoutRect, priority_for: &dyn Fn(bool) -> Priority| {
            let (Some(info), Some(page_rect)) = (page_info(page), layout.page_rect(page)) else {
                return;
            };
            let Some(area) = area.intersect(&page_rect) else {
                return;
            };
            let page_px = scale.page_pixels(info.size, info.rotation.then(config.rotation));
            let grid = TileGrid::new(page_px, config.tile_size);
            let sx = f64::from(page_px.width) / page_rect.width;
            let sy = f64::from(page_px.height) / page_rect.height;
            let px_rect = to_pixels(&area, &page_rect, sx, sy);
            for coord in grid.tiles_intersecting(px_rect) {
                let Some((rendered, region)) = grid.rendered_region(coord, config.gutter) else {
                    continue;
                };
                let tile_center =
                    page_rect.y + (f64::from(region.y) + f64::from(region.height) / 2.0) / sy;
                let tile_layout = LayoutRect {
                    x: page_rect.x + f64::from(region.x) / sx,
                    y: page_rect.y + f64::from(region.y) / sy,
                    width: f64::from(region.width) / sx,
                    height: f64::from(region.height) / sy,
                };
                let on_screen = tile_layout.intersect(&visible).is_some();
                out.push(PlannedTile {
                    key: TileKey {
                        page: PageId::new(document, page),
                        bucket,
                        rotation: config.rotation,
                        color: config.color,
                        tile_size: grid.tile_size(),
                        coord,
                    },
                    priority: priority_for(on_screen),
                    distance: (tile_center - center_y).abs() as f32,
                    request: RenderRequest {
                        page,
                        scale,
                        rotation: config.rotation,
                        region: rendered,
                        background: config.background,
                        color_mode: config.color,
                        annotations: true,
                    },
                });
            }
        };

    for p in near_pages.clone() {
        let page = PageIndex::new(p);
        let page_visible = visible_pages.contains(&p);
        plan_area(page, &near, &|on_screen| match (on_screen, page_visible) {
            (true, _) => Priority::Visible,
            (false, true) => Priority::VisiblePage,
            (false, false) => Priority::Near,
        });
    }

    if config.prefetch_next_page && near_pages.end < layout.page_count() {
        let next = PageIndex::new(near_pages.end);
        if let Some(rect) = layout.page_rect(next) {
            let first_screen = LayoutRect {
                height: visible.height.min(rect.height),
                ..rect
            };
            plan_area(next, &first_screen, &|_| Priority::Prefetch);
        }
    }

    out.sort_by(|a, b| {
        a.priority
            .cmp(&b.priority)
            .then(a.distance.total_cmp(&b.distance))
    });
    out
}

/// Maps a layout rectangle inside `page_rect` to page pixels, rounding
/// outwards so partially covered pixels are included.
fn to_pixels(area: &LayoutRect, page_rect: &LayoutRect, sx: f64, sy: f64) -> PixelRect {
    let x0 = ((area.x - page_rect.x) * sx).floor().max(0.0);
    let y0 = ((area.y - page_rect.y) * sy).floor().max(0.0);
    let x1 = ((area.right() - page_rect.x) * sx).ceil().max(x0);
    let y1 = ((area.bottom() - page_rect.y) * sy).ceil().max(y0);
    let clamp = |v: f64| v.min(f64::from(u32::MAX)) as u32;
    PixelRect::new(clamp(x0), clamp(y0), clamp(x1 - x0), clamp(y1 - y0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ZoomLevel;
    use fastpdf_engine_api::PageSize;

    fn info(_: PageIndex) -> Option<PageInfo> {
        Some(PageInfo {
            size: PageSize::LETTER,
            rotation: Rotation::R0,
        })
    }

    fn setup(pages: u32, zoom: f32) -> (DocumentLayout, Viewport) {
        let layout = DocumentLayout::new(pages, PageSize::LETTER, 8.0);
        let viewport = Viewport::new(1280.0, 720.0, ZoomLevel::new(zoom), 1.0);
        (layout, viewport)
    }

    #[test]
    fn visible_tiles_come_first_and_cover_the_view() {
        let (layout, mut viewport) = setup(10, 1.0);
        viewport.clamp_scroll(&layout);
        let plan = plan_tiles(
            DocumentId::from_raw(1),
            &layout,
            &viewport,
            info,
            &PlanConfig::default(),
        );
        assert!(!plan.is_empty());
        assert_eq!(plan[0].priority, Priority::Visible);
        assert!(plan.windows(2).all(|w| w[0].priority <= w[1].priority));
        // At 100% a Letter page is 816 x 1056 px = 2 x 3 tiles of 512; the
        // 720-px view starts at the top of page 1 and sees its first two rows.
        let visible = plan
            .iter()
            .filter(|t| t.priority == Priority::Visible)
            .count();
        assert_eq!(visible, 4);
        // Nothing beyond the near margin + one prefetched page is planned.
        let max_page = plan.iter().map(|t| t.request.page.get()).max().unwrap();
        assert!(max_page <= 2, "{max_page}");
    }

    #[test]
    fn deep_zoom_renders_only_intersecting_tiles() {
        let (layout, mut viewport) = setup(1, 6.0);
        viewport.scroll_y = 300.0;
        viewport.scroll_x = 200.0;
        let config = PlanConfig {
            near_margin: 0.0,
            prefetch_next_page: false,
            ..PlanConfig::default()
        };
        let plan = plan_tiles(DocumentId::from_raw(1), &layout, &viewport, info, &config);
        // 1280x720 view over 512-px tiles: at most 4 x 3 tiles, never the
        // whole 4896 x 6336 px page (spec §12).
        assert!(plan.len() <= 12, "{}", plan.len());
        assert!(plan.iter().all(|t| t.priority == Priority::Visible));
        assert!(plan.iter().all(|t| t.request.region.width <= 512));
    }

    #[test]
    fn gutters_extend_requests_but_not_keys() {
        let (layout, viewport) = setup(1, 1.0);
        let config = PlanConfig {
            gutter: 2,
            ..PlanConfig::default()
        };
        let plain = plan_tiles(
            DocumentId::from_raw(1),
            &layout,
            &viewport,
            info,
            &PlanConfig::default(),
        );
        let padded = plan_tiles(DocumentId::from_raw(1), &layout, &viewport, info, &config);
        assert_eq!(plain.len(), padded.len());
        for (a, b) in plain.iter().zip(&padded) {
            assert_eq!(a.key, b.key);
            assert!(a.request.region.is_within(b.request.region));
        }
        let edge = DEFAULT_TILE_SIZE + 2; // no gutter at the page corner
        assert_eq!(padded[0].request.region, PixelRect::new(0, 0, edge, edge));
    }

    #[test]
    fn unresolved_pages_are_skipped() {
        let (layout, viewport) = setup(3, 1.0);
        let plan = plan_tiles(
            DocumentId::from_raw(1),
            &layout,
            &viewport,
            |_| None,
            &PlanConfig::default(),
        );
        assert!(plan.is_empty());
    }

    #[test]
    fn rotation_changes_tile_geometry() {
        let viewport = Viewport::new(1280.0, 720.0, ZoomLevel::ACTUAL_SIZE, 1.0);
        let config = PlanConfig {
            rotation: Rotation::R90,
            ..PlanConfig::default()
        };
        let rotated = DocumentLayout::new(1, PageSize::LETTER.rotated(Rotation::R90), 8.0);
        let plan = plan_tiles(DocumentId::from_raw(1), &rotated, &viewport, info, &config);
        assert!(plan.iter().all(|t| t.key.rotation == Rotation::R90));
        // Landscape after rotation: 1056 px wide, so a third tile column exists.
        assert!(
            plan.iter()
                .any(|t| t.request.region.x >= 2 * DEFAULT_TILE_SIZE)
        );
    }
}
