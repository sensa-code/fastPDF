use fastpdf_engine_api::RenderScale;

use crate::POINTS_TO_PX;

/// UI zoom factor; `1.0` shows a page at its physical size on a 96-dpi display.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub struct ZoomLevel(f32);

impl ZoomLevel {
    pub const MIN: f32 = 0.10;
    pub const MAX: f32 = 32.0;
    pub const ACTUAL_SIZE: Self = Self(1.0);

    /// Preset steps for Ctrl+Plus / Ctrl+Minus.
    const STEPS: [f32; 24] = [
        0.10, 0.25, 0.33, 0.50, 0.67, 0.75, 0.80, 0.90, 1.00, 1.10, 1.25, 1.50, 1.75, 2.00, 2.50,
        3.00, 4.00, 5.00, 6.00, 8.00, 12.0, 16.0, 24.0, 32.0,
    ];

    /// Clamps into the supported range; NaN becomes 100%.
    pub fn new(zoom: f32) -> Self {
        if zoom.is_nan() {
            return Self::ACTUAL_SIZE;
        }
        Self(zoom.clamp(Self::MIN, Self::MAX))
    }

    pub const fn get(self) -> f32 {
        self.0
    }

    pub fn percent(self) -> u32 {
        (self.0 * 100.0).round() as u32
    }

    /// Next preset step above the current zoom.
    pub fn zoom_in(self) -> Self {
        let next = Self::STEPS.iter().copied().find(|&s| s > self.0 * 1.001);
        Self::new(next.unwrap_or(Self::MAX))
    }

    /// Next preset step below the current zoom.
    pub fn zoom_out(self) -> Self {
        let prev = Self::STEPS
            .iter()
            .rev()
            .copied()
            .find(|&s| s < self.0 * 0.999);
        Self::new(prev.unwrap_or(Self::MIN))
    }

    /// Multiplies by `factor` (Ctrl + mouse wheel, pinch).
    pub fn scaled(self, factor: f32) -> Self {
        Self::new(self.0 * factor)
    }
}

/// A quantized display scale used in tile cache keys (spec §17).
///
/// The display scale is `zoom × window scale factor` (1.0 = 100% zoom on a
/// 96-dpi display). The UI may use any zoom; tiles are rendered at the
/// nearest bucket and scaled slightly on screen, so a continuous zoom
/// gesture re-renders only when it crosses a bucket boundary. Buckets cover
/// the common Windows scale factors (1.25, 1.5, 1.75, 2.0) exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ScaleBucket(u8);

impl ScaleBucket {
    const VALUES: [f32; 26] = [
        0.125, 0.25, 0.375, 0.5, 0.625, 0.75, 0.875, 1.0, 1.125, 1.25, 1.5, 1.75, 2.0, 2.5, 3.0,
        3.5, 4.0, 5.0, 6.0, 8.0, 10.0, 12.0, 16.0, 24.0, 32.0, 48.0,
    ];

    /// Rendering up to this much smaller than needed (then upscaling on
    /// screen) is accepted to avoid jumping to a much larger bucket.
    const UPSCALE_TOLERANCE: f32 = 1.06;

    /// Picks the bucket for a display scale: the bucket just below when the
    /// required upscale stays within tolerance, otherwise the bucket above
    /// (downscaling on screen keeps text crisp).
    pub fn for_display_scale(display_scale: f32) -> Self {
        let s = if display_scale.is_finite() {
            display_scale
        } else {
            1.0
        };
        let last = Self::VALUES.len() - 1;
        let above = Self::VALUES.iter().position(|&v| v >= s).unwrap_or(last);
        if above > 0 && s <= Self::VALUES[above - 1] * Self::UPSCALE_TOLERANCE {
            Self((above - 1) as u8)
        } else {
            Self(above as u8)
        }
    }

    pub fn display_scale(self) -> f32 {
        Self::VALUES[usize::from(self.0)]
    }

    /// Engine render scale (device pixels per PDF point).
    pub fn render_scale(self) -> RenderScale {
        RenderScale::new(self.display_scale() * POINTS_TO_PX).unwrap_or(RenderScale::IDENTITY)
    }

    /// On-screen scale factor to apply to a tile of this bucket when the
    /// actual display scale is `display_scale`.
    pub fn screen_factor(self, display_scale: f32) -> f32 {
        display_scale / self.display_scale()
    }

    pub fn all() -> impl Iterator<Item = Self> {
        (0..Self::VALUES.len()).map(|i| Self(i as u8))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn common_scale_factors_map_exactly() {
        for s in [1.0f32, 1.25, 1.5, 1.75, 2.0, 3.0, 4.0] {
            assert_eq!(ScaleBucket::for_display_scale(s).display_scale(), s);
        }
    }

    #[test]
    fn arbitrary_zoom_snaps_up_unless_close_below() {
        // 137%: 1.37 / 1.25 = 1.096 > tolerance -> 1.5
        assert_eq!(ScaleBucket::for_display_scale(1.37).display_scale(), 1.5);
        // 205%: 2.05 / 2.0 = 1.025 -> stay at 2.0
        assert_eq!(ScaleBucket::for_display_scale(2.05).display_scale(), 2.0);
        assert_eq!(ScaleBucket::for_display_scale(0.01).display_scale(), 0.125);
        assert_eq!(ScaleBucket::for_display_scale(1e9).display_scale(), 48.0);
        assert_eq!(
            ScaleBucket::for_display_scale(f32::NAN).display_scale(),
            1.0
        );
    }

    #[test]
    fn bucket_count_is_bounded() {
        let distinct: std::collections::BTreeSet<_> = (1..=3200)
            .map(|p| ScaleBucket::for_display_scale(p as f32 / 100.0))
            .collect();
        assert!(distinct.len() <= ScaleBucket::all().count());
    }

    #[test]
    fn render_scale_converts_points_to_pixels() {
        let b = ScaleBucket::for_display_scale(1.0);
        assert!((b.render_scale().get() - 96.0 / 72.0).abs() < 1e-6);
    }

    #[test]
    fn zoom_steps_round_trip() {
        let z = ZoomLevel::ACTUAL_SIZE;
        assert_eq!(z.zoom_in().get(), 1.10);
        assert_eq!(z.zoom_out().get(), 0.90);
        assert_eq!(ZoomLevel::new(1.37).zoom_in().get(), 1.50);
        assert_eq!(ZoomLevel::new(100.0).get(), ZoomLevel::MAX);
        assert_eq!(
            ZoomLevel::new(ZoomLevel::MAX).zoom_in().get(),
            ZoomLevel::MAX
        );
        assert_eq!(ZoomLevel::new(f32::NAN), ZoomLevel::ACTUAL_SIZE);
        assert_eq!(ZoomLevel::new(1.234).percent(), 123);
    }
}
