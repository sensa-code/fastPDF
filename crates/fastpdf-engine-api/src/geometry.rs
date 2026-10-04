use std::fmt;

/// Size of a page's visible box (CropBox) in PDF points, before rotation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PageSize {
    pub width: f32,
    pub height: f32,
}

impl PageSize {
    /// US Letter, used as an estimate for pages whose size is not known yet.
    pub const LETTER: Self = Self::new(612.0, 792.0);

    pub const fn new(width: f32, height: f32) -> Self {
        Self { width, height }
    }

    /// The size after applying `rotation` (width/height swap on odd quarter turns).
    pub fn rotated(self, rotation: Rotation) -> Self {
        if rotation.swaps_axes() {
            Self::new(self.height, self.width)
        } else {
            self
        }
    }

    /// True when both sides are finite, positive and within `max_side` points.
    pub fn is_sane(self, max_side: f32) -> bool {
        let ok = |v: f32| v.is_finite() && v > 0.0 && v <= max_side;
        ok(self.width) && ok(self.height)
    }
}

/// Clockwise page rotation in quarter turns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub enum Rotation {
    #[default]
    R0,
    R90,
    R180,
    R270,
}

impl Rotation {
    /// Normalizes any multiple of 90 degrees (including negative values, as
    /// found in real-world `/Rotate` entries). Other values return `None`.
    pub fn from_degrees(degrees: i64) -> Option<Self> {
        if degrees % 90 != 0 {
            return None;
        }
        Some(match (degrees / 90).rem_euclid(4) {
            0 => Self::R0,
            1 => Self::R90,
            2 => Self::R180,
            _ => Self::R270,
        })
    }

    pub const fn degrees(self) -> u32 {
        match self {
            Self::R0 => 0,
            Self::R90 => 90,
            Self::R180 => 180,
            Self::R270 => 270,
        }
    }

    pub const fn quarter_turns(self) -> u8 {
        match self {
            Self::R0 => 0,
            Self::R90 => 1,
            Self::R180 => 2,
            Self::R270 => 3,
        }
    }

    /// Composes two rotations (e.g. the page's `/Rotate` plus the user's rotation).
    pub fn then(self, other: Self) -> Self {
        match (self.quarter_turns() + other.quarter_turns()) % 4 {
            0 => Self::R0,
            1 => Self::R90,
            2 => Self::R180,
            _ => Self::R270,
        }
    }

    pub fn clockwise(self) -> Self {
        self.then(Self::R90)
    }

    pub fn counter_clockwise(self) -> Self {
        self.then(Self::R270)
    }

    pub const fn swaps_axes(self) -> bool {
        matches!(self, Self::R90 | Self::R270)
    }
}

/// Device pixels per PDF point. `1.0` renders at 72 dpi; 100% zoom on a
/// 96-dpi display is `96/72`.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub struct RenderScale(f32);

impl RenderScale {
    /// Smallest scale accepted (thumbnails of huge pages).
    pub const MIN: f32 = 1.0 / 64.0;
    /// Largest scale accepted; bitmap limits still apply per request.
    pub const MAX: f32 = 64.0;
    pub const IDENTITY: Self = Self(1.0);

    /// Returns `None` for non-finite or out-of-range values.
    pub fn new(scale: f32) -> Option<Self> {
        (scale.is_finite() && (Self::MIN..=Self::MAX).contains(&scale)).then_some(Self(scale))
    }

    pub const fn get(self) -> f32 {
        self.0
    }

    /// Pixel size of a page rendered at this scale with `rotation` applied.
    /// Each side is at least one pixel.
    pub fn page_pixels(self, size: PageSize, rotation: Rotation) -> PixelSize {
        let rotated = size.rotated(rotation);
        PixelSize::new(
            scale_to_pixels(rotated.width, self.0),
            scale_to_pixels(rotated.height, self.0),
        )
    }
}

fn scale_to_pixels(points: f32, scale: f32) -> u32 {
    // Round up partial pixels, but not float noise: 612 pt at 160/612
    // computes to 160.0000005 and must stay 160 px.
    let px = (f64::from(points) * f64::from(scale) - 1e-3).ceil();
    if !px.is_finite() || px < 1.0 {
        1
    } else if px >= f64::from(u32::MAX) {
        u32::MAX
    } else {
        px as u32
    }
}

/// Integer size in device pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct PixelSize {
    pub width: u32,
    pub height: u32,
}

impl PixelSize {
    pub const fn new(width: u32, height: u32) -> Self {
        Self { width, height }
    }

    pub const fn area(self) -> u64 {
        self.width as u64 * self.height as u64
    }

    pub const fn bounds(self) -> PixelRect {
        PixelRect::new(0, 0, self.width, self.height)
    }
}

/// Rectangle in device pixels of a rendered (scaled + rotated) page; origin top-left.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct PixelRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl PixelRect {
    pub const fn new(x: u32, y: u32, width: u32, height: u32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    pub const fn size(self) -> PixelSize {
        PixelSize::new(self.width, self.height)
    }

    pub const fn is_empty(self) -> bool {
        self.width == 0 || self.height == 0
    }

    /// Exclusive right edge, computed without overflow.
    pub const fn right(self) -> u64 {
        self.x as u64 + self.width as u64
    }

    /// Exclusive bottom edge, computed without overflow.
    pub const fn bottom(self) -> u64 {
        self.y as u64 + self.height as u64
    }

    /// True when `self` lies entirely inside `outer`.
    pub fn is_within(self, outer: Self) -> bool {
        self.x >= outer.x
            && self.y >= outer.y
            && self.right() <= outer.right()
            && self.bottom() <= outer.bottom()
    }

    pub fn intersect(self, other: Self) -> Option<Self> {
        let x0 = self.x.max(other.x);
        let y0 = self.y.max(other.y);
        let x1 = self.right().min(other.right());
        let y1 = self.bottom().min(other.bottom());
        if u64::from(x0) >= x1 || u64::from(y0) >= y1 {
            return None;
        }
        // x1/y1 are bounded by an input's right/bottom edge, and x0/y0 by an
        // input origin, so the differences fit in u32.
        Some(Self::new(
            x0,
            y0,
            (x1 - u64::from(x0)) as u32,
            (y1 - u64::from(y0)) as u32,
        ))
    }
}

/// Rectangle in page space: points, origin at the top-left of the unrotated
/// visible box, y down. Engines convert from PDF user space into this space.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct PageRect {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

impl PageRect {
    /// Builds a normalized rectangle (x0 <= x1, y0 <= y1).
    pub fn new(x0: f32, y0: f32, x1: f32, y1: f32) -> Self {
        Self {
            x0: x0.min(x1),
            y0: y0.min(y1),
            x1: x0.max(x1),
            y1: y0.max(y1),
        }
    }

    pub fn width(self) -> f32 {
        self.x1 - self.x0
    }

    pub fn height(self) -> f32 {
        self.y1 - self.y0
    }

    pub fn contains(self, x: f32, y: f32) -> bool {
        x >= self.x0 && x <= self.x1 && y >= self.y0 && y <= self.y1
    }

    pub fn union(self, other: Self) -> Self {
        Self {
            x0: self.x0.min(other.x0),
            y0: self.y0.min(other.y0),
            x1: self.x1.max(other.x1),
            y1: self.y1.max(other.y1),
        }
    }
}

impl fmt::Display for PageRect {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "[{:.1}, {:.1} – {:.1}, {:.1}]",
            self.x0, self.y0, self.x1, self.y1
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotation_normalizes_real_world_values() {
        assert_eq!(Rotation::from_degrees(-90), Some(Rotation::R270));
        assert_eq!(Rotation::from_degrees(450), Some(Rotation::R90));
        assert_eq!(Rotation::from_degrees(45), None);
        assert_eq!(Rotation::R270.then(Rotation::R180), Rotation::R90);
        assert_eq!(Rotation::R0.counter_clockwise(), Rotation::R270);
    }

    #[test]
    fn page_pixels_rotate_and_round_up() {
        let s = RenderScale::new(2.0).unwrap_or(RenderScale::IDENTITY);
        let px = s.page_pixels(PageSize::new(100.2, 50.0), Rotation::R90);
        assert_eq!(px, PixelSize::new(100, 201));
    }

    #[test]
    fn float_noise_does_not_add_a_pixel() {
        let s = RenderScale::new(160.0 / 612.0).unwrap();
        assert_eq!(s.page_pixels(PageSize::LETTER, Rotation::R0).width, 160);
        let s = RenderScale::new(96.0 / 72.0).unwrap();
        assert_eq!(
            s.page_pixels(PageSize::LETTER, Rotation::R0),
            PixelSize::new(816, 1056)
        );
    }

    #[test]
    fn render_scale_rejects_garbage() {
        assert!(RenderScale::new(f32::NAN).is_none());
        assert!(RenderScale::new(0.0).is_none());
        assert!(RenderScale::new(1e6).is_none());
    }

    #[test]
    fn page_pixels_saturate_instead_of_overflowing() {
        let s = RenderScale::new(RenderScale::MAX).unwrap_or(RenderScale::IDENTITY);
        let px = s.page_pixels(PageSize::new(f32::MAX, 1.0), Rotation::R0);
        assert_eq!(px.width, u32::MAX);
    }

    #[test]
    fn pixel_rect_intersection() {
        let a = PixelRect::new(0, 0, 100, 100);
        let b = PixelRect::new(50, 80, 100, 100);
        assert_eq!(a.intersect(b), Some(PixelRect::new(50, 80, 50, 20)));
        assert_eq!(a.intersect(PixelRect::new(100, 0, 5, 5)), None);
        assert!(PixelRect::new(10, 10, 5, 5).is_within(a));
        assert!(!PixelRect::new(u32::MAX, 0, 5, 5).is_within(a));
    }

    #[test]
    fn page_size_sanity() {
        assert!(PageSize::LETTER.is_sane(14_400.0));
        assert!(!PageSize::new(0.0, 10.0).is_sane(14_400.0));
        assert!(!PageSize::new(f32::INFINITY, 10.0).is_sane(14_400.0));
        assert!(!PageSize::new(1e7, 10.0).is_sane(14_400.0));
    }
}
