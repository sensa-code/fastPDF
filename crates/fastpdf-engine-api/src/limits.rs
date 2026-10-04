use std::time::Duration;

use crate::{EngineError, LimitKind, PageSize, PixelSize};

/// Guardrails for hostile input (spec §25). The guard layer enforces the
/// generic ones (bitmap size, page dimensions, page count); adapters pass the
/// rest to their engine where the engine supports it.
#[derive(Debug, Clone, PartialEq)]
pub struct ResourceLimits {
    /// Largest single render target in bytes.
    pub max_bitmap_bytes: u64,
    /// Largest render target side in pixels (D3D11 textures top out at 16384).
    pub max_bitmap_dimension: u32,
    /// Largest accepted page side in points. PDF caps user space at 14 400
    /// units, but `/UserUnit` can legitimately scale beyond it.
    pub max_page_dimension_pt: f32,
    /// Largest page count accepted; protects per-page bookkeeping.
    pub max_page_count: u32,
    /// Largest decoded image an engine should materialize, in pixels.
    pub max_decoded_image_pixels: u64,
    /// Maximum object/array/dictionary nesting depth.
    pub max_nesting_depth: u32,
    /// Maximum recursion depth for form XObjects, patterns, etc.
    pub max_recursion_depth: u32,
    /// Largest single object/stream an engine should decode, in bytes.
    pub max_object_bytes: u64,
    /// Soft deadline for one render call; engines degrade to a partial render.
    pub max_render_time: Option<Duration>,
}

impl Default for ResourceLimits {
    fn default() -> Self {
        Self {
            max_bitmap_bytes: 256 * 1024 * 1024,
            max_bitmap_dimension: 16_384,
            max_page_dimension_pt: 200_000.0,
            max_page_count: 1_000_000,
            max_decoded_image_pixels: 256 * 1024 * 1024,
            max_nesting_depth: 256,
            max_recursion_depth: 64,
            max_object_bytes: 512 * 1024 * 1024,
            max_render_time: Some(Duration::from_secs(20)),
        }
    }
}

impl ResourceLimits {
    /// Bytes per pixel of every supported [`crate::PixelFormat`].
    const BYTES_PER_PIXEL: u64 = 4;

    /// Validates a render target size before any allocation happens.
    pub fn check_bitmap(&self, size: PixelSize) -> Result<(), EngineError> {
        if size.width == 0 || size.height == 0 {
            return Err(EngineError::InvalidRequest("empty bitmap".into()));
        }
        if size.width > self.max_bitmap_dimension || size.height > self.max_bitmap_dimension {
            return Err(EngineError::LimitExceeded(LimitKind::BitmapDimension));
        }
        match size.area().checked_mul(Self::BYTES_PER_PIXEL) {
            Some(bytes) if bytes <= self.max_bitmap_bytes => Ok(()),
            _ => Err(EngineError::LimitExceeded(LimitKind::BitmapBytes)),
        }
    }

    pub fn check_page_size(&self, size: PageSize) -> Result<(), EngineError> {
        if size.is_sane(self.max_page_dimension_pt) {
            Ok(())
        } else {
            Err(EngineError::LimitExceeded(LimitKind::PageDimension))
        }
    }

    pub fn check_page_count(&self, count: u32) -> Result<(), EngineError> {
        if count <= self.max_page_count {
            Ok(())
        } else {
            Err(EngineError::LimitExceeded(LimitKind::PageCount))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bitmap_limits() {
        let limits = ResourceLimits::default();
        assert!(limits.check_bitmap(PixelSize::new(512, 512)).is_ok());
        assert_eq!(
            limits.check_bitmap(PixelSize::new(20_000, 10)),
            Err(EngineError::LimitExceeded(LimitKind::BitmapDimension))
        );
        assert_eq!(
            limits.check_bitmap(PixelSize::new(16_384, 16_384)),
            Err(EngineError::LimitExceeded(LimitKind::BitmapBytes))
        );
        assert!(limits.check_bitmap(PixelSize::new(0, 10)).is_err());
    }

    #[test]
    fn page_limits() {
        let limits = ResourceLimits::default();
        assert!(limits.check_page_size(PageSize::LETTER).is_ok());
        assert!(limits.check_page_size(PageSize::new(1e7, 1e7)).is_err());
        assert!(limits.check_page_count(2_000).is_ok());
        assert!(limits.check_page_count(u32::MAX).is_err());
    }
}
