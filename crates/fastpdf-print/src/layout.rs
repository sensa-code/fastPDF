//! Placing a page on the sheet and cutting its raster into bands. Pure
//! arithmetic, independent of the platform, so it is unit tested everywhere.

use fastpdf_engine_api::{PageInfo, PixelRect, PixelSize, RenderScale, Rotation};

use crate::job::{FitMode, PaperInfo, PrintError};

/// Rows rendered per band. With [`MAX_BAND_WIDTH`] this bounds one band
/// buffer to 8 MiB whatever the page size or resolution (an A0 sheet at
/// 600 dpi is about 20 000 x 28 000 pixels, 2.2 GB as one bitmap).
pub const BAND_ROWS: u32 = 256;
/// Widest band in pixels; wider pages are split into several columns. Also
/// keeps every request under the guard's bitmap dimension limit.
pub const MAX_BAND_WIDTH: u32 = 8192;

/// Sheet geometry of a printer device context, in device units (dots).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Paper {
    pub(crate) dpi_x: u32,
    pub(crate) dpi_y: u32,
    /// The whole sheet.
    pub(crate) physical_w: i64,
    pub(crate) physical_h: i64,
    /// Top-left corner of the printable area within the sheet.
    pub(crate) offset_x: i64,
    pub(crate) offset_y: i64,
    /// Printable area; device coordinates start at its top-left corner.
    pub(crate) printable_w: i64,
    pub(crate) printable_h: i64,
}

impl Paper {
    /// The printable area in device coordinates (which start at its corner).
    pub(crate) fn printable_rect(&self) -> DeviceRect {
        DeviceRect {
            x: 0,
            y: 0,
            w: self.printable_w,
            h: self.printable_h,
        }
    }

    /// The geometry in points, for reports.
    pub(crate) fn info(&self) -> PaperInfo {
        let x = |v: i64| (v as f64 * 72.0 / f64::from(self.dpi_x.max(1))) as f32;
        let y = |v: i64| (v as f64 * 72.0 / f64::from(self.dpi_y.max(1))) as f32;
        PaperInfo {
            width_pt: x(self.physical_w),
            height_pt: y(self.physical_h),
            printable_pt: [
                x(self.offset_x),
                y(self.offset_y),
                x(self.printable_w),
                y(self.printable_h),
            ],
        }
    }
}

/// A rectangle in device units, relative to the printable area.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DeviceRect {
    pub(crate) x: i64,
    pub(crate) y: i64,
    pub(crate) w: i64,
    pub(crate) h: i64,
}

/// How one page goes onto the sheet.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Placement {
    /// Extra rotation on top of the page's `/Rotate` (auto-rotate).
    pub(crate) rotation: Rotation,
    /// Points to source pixels.
    pub(crate) scale: RenderScale,
    /// The whole page rendered at `scale` (and `rotation`).
    pub(crate) source: PixelSize,
    /// Where the whole page lands on the sheet.
    pub(crate) dest: DeviceRect,
    /// The part of the source that lands inside the printable area; `None`
    /// when the page misses it entirely.
    pub(crate) visible: Option<PixelRect>,
}

impl Placement {
    /// Device rectangle covered by the source pixels `rect`. Every edge is
    /// mapped by the same rounding, so rectangles that share a source edge
    /// share the device edge too: neighbours never gap or overlap.
    pub(crate) fn device_rect(&self, rect: PixelRect) -> DeviceRect {
        let (sw, sh) = (
            i64::from(self.source.width.max(1)),
            i64::from(self.source.height.max(1)),
        );
        let map_x = |sx: u64| self.dest.x + (sx as i64) * self.dest.w / sw;
        let map_y = |sy: u64| self.dest.y + (sy as i64) * self.dest.h / sh;
        let (x0, x1) = (map_x(u64::from(rect.x)), map_x(rect.right()));
        let (y0, y1) = (map_y(u64::from(rect.y)), map_y(rect.bottom()));
        DeviceRect {
            x: x0,
            y: y0,
            w: x1 - x0,
            h: y1 - y0,
        }
    }
}

impl DeviceRect {
    pub(crate) fn is_empty(&self) -> bool {
        self.w <= 0 || self.h <= 0
    }

    /// Smallest rectangle containing both.
    pub(crate) fn union(&self, other: &Self) -> Self {
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        Self {
            x,
            y,
            w: (self.x + self.w).max(other.x + other.w) - x,
            h: (self.y + self.h).max(other.y + other.h) - y,
        }
    }
}

/// One band: a source region and the device rectangle it is stretched to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Band {
    pub(crate) source: PixelRect,
    pub(crate) dest: DeviceRect,
}

fn is_landscape(w: f64, h: f64) -> bool {
    w > h
}

/// Pages are rendered at the device resolution (of the finer axis), capped
/// at `max_dpi`; the device stretches the rest.
pub(crate) fn render_dpi(paper: &Paper, max_dpi: u32) -> u32 {
    max_dpi.min(paper.dpi_x.max(paper.dpi_y)).max(1)
}

/// Computes the placement of a page described by `info` on `paper`.
pub(crate) fn place(
    info: &PageInfo,
    paper: &Paper,
    fit: FitMode,
    auto_rotate: bool,
    max_dpi: u32,
) -> Result<Placement, PrintError> {
    let (dpi_x, dpi_y) = (f64::from(paper.dpi_x), f64::from(paper.dpi_y));
    let unrotated = info.display_size(Rotation::R0);
    let paper_landscape = is_landscape(
        paper.printable_w as f64 / dpi_x,
        paper.printable_h as f64 / dpi_y,
    );
    let page_landscape = is_landscape(f64::from(unrotated.width), f64::from(unrotated.height));
    // Counter-clockwise quarter turn: the landscape convention of most
    // printer drivers (DC_ORIENTATION = 90).
    let rotation = if auto_rotate && page_landscape != paper_landscape {
        Rotation::R270
    } else {
        Rotation::R0
    };
    let size = info.display_size(rotation);
    let natural_w = f64::from(size.width) / 72.0 * dpi_x;
    let natural_h = f64::from(size.height) / 72.0 * dpi_y;
    if !(natural_w.is_finite() && natural_h.is_finite() && natural_w > 0.0 && natural_h > 0.0) {
        return Err(PrintError::InvalidJob("page has no usable size".into()));
    }
    let fit_scale = match fit {
        FitMode::ShrinkToFit => (paper.printable_w as f64 / natural_w)
            .min(paper.printable_h as f64 / natural_h)
            .min(1.0),
        FitMode::ActualSize => 1.0,
    };
    let dest_w = ((natural_w * fit_scale).round() as i64).max(1);
    let dest_h = ((natural_h * fit_scale).round() as i64).max(1);
    let (x, y) = match fit {
        // Centered in the printable area, which it fits by construction.
        FitMode::ShrinkToFit => (
            (paper.printable_w - dest_w) / 2,
            (paper.printable_h - dest_h) / 2,
        ),
        // Centered on the sheet, like the original paper size.
        FitMode::ActualSize => (
            (paper.physical_w - dest_w) / 2 - paper.offset_x,
            (paper.physical_h - dest_h) / 2 - paper.offset_y,
        ),
    };
    let dest = DeviceRect {
        x,
        y,
        w: dest_w,
        h: dest_h,
    };
    let render_dpi = f64::from(render_dpi(paper, max_dpi));
    let raw_scale = (render_dpi / 72.0 * fit_scale) as f32;
    let scale = RenderScale::new(raw_scale.clamp(RenderScale::MIN, RenderScale::MAX))
        .ok_or_else(|| PrintError::InvalidJob("page cannot be scaled to the paper".into()))?;
    let source = scale.page_pixels(info.size, info.rotation.then(rotation));
    let visible = visible_source(source, dest, paper);
    Ok(Placement {
        rotation,
        scale,
        source,
        dest,
        visible,
    })
}

/// The source pixels whose device image intersects the printable area.
fn visible_source(source: PixelSize, dest: DeviceRect, paper: &Paper) -> Option<PixelRect> {
    let vx0 = dest.x.max(0);
    let vy0 = dest.y.max(0);
    let vx1 = (dest.x + dest.w).min(paper.printable_w);
    let vy1 = (dest.y + dest.h).min(paper.printable_h);
    if vx0 >= vx1 || vy0 >= vy1 {
        return None;
    }
    let (sw, sh) = (i64::from(source.width), i64::from(source.height));
    // Device offset -> source pixel, rounding outwards so edge pixels stay in.
    let sx0 = ((vx0 - dest.x) * sw / dest.w).clamp(0, sw);
    let sy0 = ((vy0 - dest.y) * sh / dest.h).clamp(0, sh);
    let sx1 = ceil_div((vx1 - dest.x) * sw, dest.w).clamp(0, sw);
    let sy1 = ceil_div((vy1 - dest.y) * sh, dest.h).clamp(0, sh);
    (sx0 < sx1 && sy0 < sy1).then(|| {
        PixelRect::new(
            sx0 as u32,
            sy0 as u32,
            (sx1 - sx0) as u32,
            (sy1 - sy0) as u32,
        )
    })
}

/// Ceiling division for a non-negative numerator and a positive divisor.
fn ceil_div(n: i64, d: i64) -> i64 {
    (n + d - 1) / d
}

/// Cuts the visible source into bands of at most `rows` x `max_width` pixels
/// and maps each to its device rectangle. Device edges are computed from the
/// same rounding for neighbouring bands, so they tile without gaps or
/// overlaps; bands that collapse to zero device pixels are dropped.
pub(crate) fn bands(placement: &Placement, rows: u32, max_width: u32) -> Vec<Band> {
    let Some(visible) = placement.visible else {
        return Vec::new();
    };
    let (rows, max_width) = (rows.max(1), max_width.max(1));
    let mut out = Vec::new();
    let mut y = visible.y;
    while u64::from(y) < visible.bottom() {
        let h = (visible.bottom() - u64::from(y)).min(u64::from(rows)) as u32;
        let mut x = visible.x;
        while u64::from(x) < visible.right() {
            let w = (visible.right() - u64::from(x)).min(u64::from(max_width)) as u32;
            let source = PixelRect::new(x, y, w, h);
            let dest = placement.device_rect(source);
            if !dest.is_empty() {
                out.push(Band { source, dest });
            }
            x += w;
        }
        y += h;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use fastpdf_engine_api::PageSize;

    /// A4 at 600 dpi with 1/6 inch margins, like a typical laser printer.
    fn a4_600() -> Paper {
        Paper {
            dpi_x: 600,
            dpi_y: 600,
            physical_w: 4961,
            physical_h: 7016,
            offset_x: 100,
            offset_y: 100,
            printable_w: 4761,
            printable_h: 6816,
        }
    }

    fn page(w: f32, h: f32, rotate: Rotation) -> PageInfo {
        PageInfo {
            size: PageSize::new(w, h),
            rotation: rotate,
        }
    }

    #[test]
    fn letter_page_shrinks_into_the_printable_area() {
        let p = place(
            &page(612.0, 792.0, Rotation::R0),
            &a4_600(),
            FitMode::ShrinkToFit,
            true,
            600,
        )
        .unwrap();
        assert_eq!(p.rotation, Rotation::R0);
        assert!(p.dest.w <= 4761 && p.dest.h <= 6816);
        assert!(p.dest.x >= 0 && p.dest.y >= 0);
        // Rendered at (about) the device size: no needless resampling.
        assert!((i64::from(p.source.width) - p.dest.w).abs() <= 1);
        assert_eq!(p.visible.map(|v| v.size()), Some(p.source));
    }

    #[test]
    fn small_pages_are_not_enlarged_and_actual_size_centers_on_the_sheet() {
        let card = page(252.0, 144.0, Rotation::R0); // 3.5 x 2 inch
        let fit = place(&card, &a4_600(), FitMode::ShrinkToFit, false, 600).unwrap();
        assert_eq!((fit.dest.w, fit.dest.h), (2100, 1200));
        let actual = place(&card, &a4_600(), FitMode::ActualSize, false, 600).unwrap();
        assert_eq!((actual.dest.w, actual.dest.h), (2100, 1200));
        assert_eq!(actual.dest.x, (4961 - 2100) / 2 - 100);
    }

    #[test]
    fn landscape_pages_turn_counter_clockwise_when_auto_rotating() {
        let wide = page(842.0, 595.0, Rotation::R0);
        let p = place(&wide, &a4_600(), FitMode::ShrinkToFit, true, 600).unwrap();
        assert_eq!(p.rotation, Rotation::R270);
        assert!(p.dest.h > p.dest.w);
        let keep = place(&wide, &a4_600(), FitMode::ShrinkToFit, false, 600).unwrap();
        assert_eq!(keep.rotation, Rotation::R0);
        assert!(keep.dest.w > keep.dest.h);
        // A page that is landscape only through /Rotate stays as displayed.
        let rotated = page(595.0, 842.0, Rotation::R90);
        let r = place(&rotated, &a4_600(), FitMode::ShrinkToFit, true, 600).unwrap();
        assert_eq!(r.rotation, Rotation::R270);
    }

    #[test]
    fn dpi_cap_lowers_the_render_scale_not_the_device_size() {
        let mut paper = a4_600();
        paper.dpi_x = 1200;
        paper.dpi_y = 1200;
        paper.physical_w *= 2;
        paper.physical_h *= 2;
        paper.printable_w *= 2;
        paper.printable_h *= 2;
        // A 4 x 6 inch page fits the printable area, so no fit scaling.
        let p = place(
            &page(288.0, 432.0, Rotation::R0),
            &paper,
            FitMode::ShrinkToFit,
            true,
            600,
        )
        .unwrap();
        assert!((p.scale.get() - 600.0 / 72.0).abs() < 1e-3);
        assert_eq!((p.source.width, p.source.height), (2400, 3600));
        assert_eq!((p.dest.w, p.dest.h), (4800, 7200));
    }

    #[test]
    fn oversized_actual_size_pages_only_render_the_visible_part() {
        let a0 = page(2384.0, 3370.0, Rotation::R0);
        let p = place(&a0, &a4_600(), FitMode::ActualSize, false, 600).unwrap();
        let visible = p.visible.unwrap();
        assert!(visible.width < p.source.width && visible.height < p.source.height);
        // About the printable area's size in source pixels (1:1 at 600 dpi).
        assert!((i64::from(visible.width) - 4761).abs() <= 2);
    }

    #[test]
    fn bands_tile_the_visible_area_exactly() {
        let a0 = page(2384.0, 3370.0, Rotation::R0);
        let mut paper = a4_600();
        // A huge sheet so the whole A0 page is visible at 600 dpi.
        paper.physical_w = 20_000;
        paper.physical_h = 29_000;
        paper.printable_w = 19_800;
        paper.printable_h = 28_800;
        let p = place(&a0, &paper, FitMode::ShrinkToFit, false, 600).unwrap();
        let bands = bands(&p, BAND_ROWS, MAX_BAND_WIDTH);
        let visible = p.visible.unwrap();
        let area: u64 = bands.iter().map(|b| b.source.size().area()).sum();
        assert_eq!(area, visible.size().area());
        for b in &bands {
            assert!(b.source.height <= BAND_ROWS && b.source.width <= MAX_BAND_WIDTH);
            assert!(u64::from(b.source.width) * u64::from(b.source.height) * 4 <= 8 << 20);
        }
        // Device rectangles of a band row abut horizontally, and rows abut.
        let first_row: Vec<&Band> = bands.iter().filter(|b| b.source.y == 0).collect();
        for pair in first_row.windows(2) {
            assert_eq!(pair[0].dest.x + pair[0].dest.w, pair[1].dest.x);
        }
        let column: Vec<&Band> = bands.iter().filter(|b| b.source.x == 0).collect();
        for pair in column.windows(2) {
            assert_eq!(pair[0].dest.y + pair[0].dest.h, pair[1].dest.y);
        }
        let last = column.last().unwrap();
        assert_eq!(last.dest.y + last.dest.h, p.dest.y + p.dest.h);
    }
}
