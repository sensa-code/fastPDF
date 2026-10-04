//! Coordinate conversions between PDF user space, FastPDF page space and
//! device pixels (see the conventions in `fastpdf_engine_api`).

use fastpdf_engine_api::{PageRect, PageSize, PixelRect, RenderScale, Rotation};
use hayro::hayro_interpret::util::TransformExt;
use hayro::hayro_syntax::page::{Page, Rotation as HayroRotation};
use hayro::kurbo::{Affine, Point, Rect};

/// Geometry of one page as hayro renders it.
///
/// hayro sizes a page by the intersection of CropBox and MediaBox (falling
/// back to A4 for zero-area boxes) and ignores `/UserUnit`; the adapter
/// applies `/UserUnit` itself so that page sizes, renders, text and link
/// coordinates all agree.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct PageGeom {
    /// Unrotated visible box in user-space units (hayro's `base_dimensions`).
    pub(crate) base_w: f64,
    pub(crate) base_h: f64,
    /// Lower-left corner of the visible box in user space.
    pub(crate) crop_x0: f64,
    pub(crate) crop_y0: f64,
    /// Points per user-space unit (`/UserUnit`, default 1).
    pub(crate) user_unit: f64,
    /// Intrinsic `/Rotate`.
    pub(crate) rotation: Rotation,
}

impl PageGeom {
    pub(crate) fn from_page(page: &Page<'_>) -> Self {
        let crop = page.intersected_crop_box();
        let (base_w, base_h) = page.base_dimensions();
        Self {
            base_w: f64::from(base_w),
            base_h: f64::from(base_h),
            crop_x0: crop.x0,
            crop_y0: crop.y0,
            user_unit: user_unit(page),
            rotation: map_rotation(page.rotation()),
        }
    }

    /// Visible box in points, before rotation.
    pub(crate) fn page_size(&self) -> PageSize {
        PageSize::new(
            to_f32(self.base_w * self.user_unit),
            to_f32(self.base_h * self.user_unit),
        )
    }

    /// Maps PDF user space to page space (points, top-left origin, y down,
    /// unrotated). Mirrors hayro's `initial_transform(true)` without the
    /// rotation part, scaled by `/UserUnit`.
    pub(crate) fn user_to_page(&self) -> Affine {
        let u = self.user_unit;
        Affine::new([
            u,
            0.0,
            0.0,
            -u,
            -self.crop_x0 * u,
            (self.crop_y0 + self.base_h) * u,
        ])
    }

    /// Converts a user-space rectangle to a normalized page-space rectangle.
    pub(crate) fn page_rect(&self, r: Rect) -> PageRect {
        let m = self.user_to_page();
        let a = m * Point::new(r.x0, r.y0);
        let b = m * Point::new(r.x1, r.y1);
        PageRect::new(to_f32(a.x), to_f32(a.y), to_f32(b.x), to_f32(b.y))
    }

    /// Converts a user-space point to page space.
    pub(crate) fn page_point(&self, x: f64, y: f64) -> (f32, f32) {
        let p = self.user_to_page() * Point::new(x, y);
        (to_f32(p.x), to_f32(p.y))
    }

    /// Device transform for rendering `region` of the page at `scale` with
    /// the user's extra `rotation`: PDF user space → device pixels of the
    /// region's top-left corner.
    pub(crate) fn device_transform(
        &self,
        page: &Page<'_>,
        scale: RenderScale,
        rotation: Rotation,
        region: PixelRect,
    ) -> Affine {
        // hayro's initial transform maps user space onto the intrinsically
        // rotated page box (W x H user units, origin top-left, y down).
        let initial = page.initial_transform(true).to_kurbo();
        let (w, h) = if self.rotation.swaps_axes() {
            (self.base_h, self.base_w)
        } else {
            (self.base_w, self.base_h)
        };
        let user = user_rotation(rotation, w, h);
        let s = f64::from(scale.get()) * self.user_unit;
        Affine::translate((-f64::from(region.x), -f64::from(region.y)))
            * Affine::scale(s)
            * user
            * initial
    }
}

/// Clockwise quarter turns of a `w` x `h` box (origin top-left, y down).
fn user_rotation(rotation: Rotation, w: f64, h: f64) -> Affine {
    match rotation {
        Rotation::R0 => Affine::IDENTITY,
        // (x, y) -> (h - y, x)
        Rotation::R90 => Affine::new([0.0, 1.0, -1.0, 0.0, h, 0.0]),
        // (x, y) -> (w - x, h - y)
        Rotation::R180 => Affine::new([-1.0, 0.0, 0.0, -1.0, w, h]),
        // (x, y) -> (y, w - x)
        Rotation::R270 => Affine::new([0.0, -1.0, 1.0, 0.0, 0.0, w]),
    }
}

pub(crate) fn map_rotation(rotation: HayroRotation) -> Rotation {
    match rotation {
        HayroRotation::None => Rotation::R0,
        HayroRotation::Horizontal => Rotation::R90,
        HayroRotation::Flipped => Rotation::R180,
        HayroRotation::FlippedHorizontal => Rotation::R270,
    }
}

/// `/UserUnit` of the page; anything that is not a finite positive number
/// is treated as the default 1.0. Absurdly large values are kept so the
/// guard's page-dimension limit rejects the page instead of rendering it.
fn user_unit(page: &Page<'_>) -> f64 {
    match page.raw().get::<f64>(b"UserUnit") {
        Some(u) if u.is_finite() && u > 0.0 => u,
        _ => 1.0,
    }
}

/// Saturating f64 -> f32 conversion that maps NaN to 0.
pub(crate) fn to_f32(v: f64) -> f32 {
    if v.is_nan() {
        0.0
    } else {
        v.clamp(f64::from(f32::MIN), f64::from(f32::MAX)) as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rot(r: Rotation, w: f64, h: f64, x: f64, y: f64) -> (f64, f64) {
        let p = user_rotation(r, w, h) * Point::new(x, y);
        (p.x, p.y)
    }

    #[test]
    fn user_rotation_maps_corners() {
        // A 100 x 50 box: the top-left corner moves to the top-right after a
        // clockwise quarter turn (new box 50 x 100).
        assert_eq!(rot(Rotation::R90, 100.0, 50.0, 0.0, 0.0), (50.0, 0.0));
        assert_eq!(rot(Rotation::R90, 100.0, 50.0, 0.0, 50.0), (0.0, 0.0));
        assert_eq!(rot(Rotation::R180, 100.0, 50.0, 0.0, 0.0), (100.0, 50.0));
        assert_eq!(rot(Rotation::R270, 100.0, 50.0, 0.0, 0.0), (0.0, 100.0));
        assert_eq!(rot(Rotation::R270, 100.0, 50.0, 100.0, 0.0), (0.0, 0.0));
    }

    #[test]
    fn to_f32_saturates() {
        assert_eq!(to_f32(f64::NAN), 0.0);
        assert_eq!(to_f32(1e300), f32::MAX);
        assert_eq!(to_f32(1.5), 1.5);
    }
}
