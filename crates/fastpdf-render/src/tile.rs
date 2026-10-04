use fastpdf_engine_api::{ColorMode, PageId, PixelRect, PixelSize, Rotation};

use crate::ScaleBucket;

/// Default tile edge in device pixels. Provisional until the tile-size
/// benchmark (256 vs 512) settles it (spec §12).
pub const DEFAULT_TILE_SIZE: u32 = 512;

/// Column/row of a tile within a page's tile grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TileCoord {
    pub col: u32,
    pub row: u32,
}

/// Identity of one rendered tile (spec §17): document, page, tile position,
/// render scale, rotation and color mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TileKey {
    pub page: PageId,
    pub bucket: ScaleBucket,
    /// User rotation (the page's intrinsic rotation is fixed per page).
    pub rotation: Rotation,
    pub color: ColorMode,
    pub tile_size: u32,
    pub coord: TileCoord,
}

/// Splits a rendered page of `page_px` pixels into square tiles; edge tiles
/// are clipped to the page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TileGrid {
    page_px: PixelSize,
    tile: u32,
}

impl TileGrid {
    pub const MIN_TILE: u32 = 64;

    pub fn new(page_px: PixelSize, tile_size: u32) -> Self {
        Self {
            page_px,
            tile: tile_size.max(Self::MIN_TILE),
        }
    }

    pub fn tile_size(&self) -> u32 {
        self.tile
    }

    pub fn page_pixels(&self) -> PixelSize {
        self.page_px
    }

    pub fn columns(&self) -> u32 {
        self.page_px.width.div_ceil(self.tile)
    }

    pub fn rows(&self) -> u32 {
        self.page_px.height.div_ceil(self.tile)
    }

    pub fn tile_count(&self) -> u64 {
        u64::from(self.columns()) * u64::from(self.rows())
    }

    /// Pixel rectangle of a tile, clipped to the page; `None` if out of grid.
    pub fn tile_rect(&self, coord: TileCoord) -> Option<PixelRect> {
        if coord.col >= self.columns() || coord.row >= self.rows() {
            return None;
        }
        let x = coord.col * self.tile;
        let y = coord.row * self.tile;
        let w = self.tile.min(self.page_px.width - x);
        let h = self.tile.min(self.page_px.height - y);
        Some(PixelRect::new(x, y, w, h))
    }

    /// The region actually rendered for a tile: its rectangle grown by
    /// `gutter` pixels on every side, clipped to the page. Drawing only the
    /// inner part of such a bitmap keeps bilinear sampling at tile edges on
    /// real content, so scaled tiles meet without seams (GPUI atlas entries
    /// have no padding). Returns `(rendered, inner)`.
    pub fn rendered_region(&self, coord: TileCoord, gutter: u32) -> Option<(PixelRect, PixelRect)> {
        let inner = self.tile_rect(coord)?;
        let x0 = inner.x.saturating_sub(gutter);
        let y0 = inner.y.saturating_sub(gutter);
        let x1 = (inner.right() + u64::from(gutter)).min(u64::from(self.page_px.width));
        let y1 = (inner.bottom() + u64::from(gutter)).min(u64::from(self.page_px.height));
        // x1/y1 are bounded by the page size, which fits in u32.
        let rendered = PixelRect::new(
            x0,
            y0,
            (x1 - u64::from(x0)) as u32,
            (y1 - u64::from(y0)) as u32,
        );
        Some((rendered, inner))
    }

    /// Tiles intersecting `rect` (page pixel space), row by row.
    pub fn tiles_intersecting(&self, rect: PixelRect) -> impl Iterator<Item = TileCoord> + use<> {
        let clipped = rect.intersect(self.page_px.bounds());
        let tile = self.tile;
        let (c0, c1, r0, r1) = match clipped {
            Some(r) => (
                r.x / tile,
                (r.right() - 1) as u32 / tile,
                r.y / tile,
                (r.bottom() - 1) as u32 / tile,
            ),
            None => (1, 0, 1, 0), // empty ranges
        };
        (r0..=r1).flat_map(move |row| (c0..=c1).map(move |col| TileCoord { col, row }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_clips_edge_tiles() {
        let g = TileGrid::new(PixelSize::new(1000, 600), 512);
        assert_eq!((g.columns(), g.rows()), (2, 2));
        assert_eq!(
            g.tile_rect(TileCoord { col: 1, row: 1 }),
            Some(PixelRect::new(512, 512, 488, 88))
        );
        assert_eq!(g.tile_rect(TileCoord { col: 2, row: 0 }), None);
    }

    #[test]
    fn only_intersecting_tiles_are_listed() {
        let g = TileGrid::new(PixelSize::new(2048, 2048), 256);
        let tiles: Vec<_> = g
            .tiles_intersecting(PixelRect::new(300, 300, 300, 10))
            .collect();
        assert_eq!(
            tiles,
            vec![TileCoord { col: 1, row: 1 }, TileCoord { col: 2, row: 1 }]
        );
        assert_eq!(
            g.tiles_intersecting(PixelRect::new(5000, 0, 10, 10))
                .count(),
            0
        );
        assert_eq!(
            g.tiles_intersecting(g.page_pixels().bounds()).count() as u64,
            g.tile_count()
        );
    }

    #[test]
    fn zoomed_view_touches_few_tiles() {
        // A 600% Letter page is ~4896 x 6336 px; a 1920x1080 view needs at
        // most 5 x 4 = 20 tiles of 512, never the whole page (spec §12).
        let g = TileGrid::new(PixelSize::new(4896, 6336), 512);
        let visible = g
            .tiles_intersecting(PixelRect::new(1000, 2000, 1920, 1080))
            .count();
        assert!(visible <= 20, "{visible}");
        assert!(g.tile_count() > 100);
    }

    #[test]
    fn gutters_grow_inside_the_page_only() {
        let g = TileGrid::new(PixelSize::new(1000, 600), 512);
        let (r, inner) = g.rendered_region(TileCoord { col: 0, row: 0 }, 2).unwrap();
        assert_eq!(inner, PixelRect::new(0, 0, 512, 512));
        assert_eq!(r, PixelRect::new(0, 0, 514, 514));
        let (r, _) = g.rendered_region(TileCoord { col: 1, row: 1 }, 2).unwrap();
        assert_eq!(r, PixelRect::new(510, 510, 490, 90));
        assert_eq!(g.rendered_region(TileCoord { col: 9, row: 0 }, 2), None);
    }

    #[test]
    fn tiny_tile_sizes_are_clamped() {
        let g = TileGrid::new(PixelSize::new(100, 100), 1);
        assert_eq!(g.tile_size(), TileGrid::MIN_TILE);
    }
}
