use crate::{EngineError, PixelSize, ResourceLimits};

/// Pixel layout of a render target. Both formats are 8 bits per channel with
/// premultiplied alpha, 4 bytes per pixel, rows tightly packed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum PixelFormat {
    #[default]
    Rgba8Premultiplied,
    /// Matches what Windows GPU surfaces (and GPUI) consume without a swizzle.
    Bgra8Premultiplied,
}

impl PixelFormat {
    pub const BYTES_PER_PIXEL: usize = 4;
}

/// Straight (non-premultiplied) 8-bit RGBA color.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rgba8 {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Rgba8 {
    pub const WHITE: Self = Self::new(255, 255, 255, 255);
    pub const TRANSPARENT: Self = Self::new(0, 0, 0, 0);

    pub const fn new(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    /// Premultiplied bytes in `format` channel order.
    pub fn premultiplied_bytes(self, format: PixelFormat) -> [u8; 4] {
        let pm = |c: u8| ((u16::from(c) * u16::from(self.a) + 127) / 255) as u8;
        let (r, g, b) = (pm(self.r), pm(self.g), pm(self.b));
        match format {
            PixelFormat::Rgba8Premultiplied => [r, g, b, self.a],
            PixelFormat::Bgra8Premultiplied => [b, g, r, self.a],
        }
    }
}

/// An owned bitmap produced by a render call.
#[derive(Clone, PartialEq, Eq)]
pub struct Pixmap {
    size: PixelSize,
    format: PixelFormat,
    data: Vec<u8>,
}

impl std::fmt::Debug for Pixmap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pixmap")
            .field("size", &self.size)
            .field("format", &self.format)
            .field("bytes", &self.data.len())
            .finish()
    }
}

impl Pixmap {
    /// Allocates a zeroed bitmap after checking it against `limits`, so a
    /// hostile page size can never trigger a giant allocation (spec §25).
    pub fn new(
        size: PixelSize,
        format: PixelFormat,
        limits: &ResourceLimits,
    ) -> Result<Self, EngineError> {
        limits.check_bitmap(size)?;
        // check_bitmap bounded width * height * 4 by max_bitmap_bytes.
        let len = size.width as usize * size.height as usize * PixelFormat::BYTES_PER_PIXEL;
        Ok(Self {
            size,
            format,
            data: vec![0; len],
        })
    }

    pub fn size(&self) -> PixelSize {
        self.size
    }

    pub fn format(&self) -> PixelFormat {
        self.format
    }

    pub fn data(&self) -> &[u8] {
        &self.data
    }

    pub fn into_data(self) -> Vec<u8> {
        self.data
    }

    pub fn byte_len(&self) -> usize {
        self.data.len()
    }

    pub fn as_mut(&mut self) -> PixmapMut<'_> {
        PixmapMut {
            size: self.size,
            format: self.format,
            data: &mut self.data,
        }
    }
}

/// A mutable view of a caller-owned bitmap; engines render into it so the
/// caller can pool and reuse tile buffers.
#[derive(Debug)]
pub struct PixmapMut<'a> {
    size: PixelSize,
    format: PixelFormat,
    data: &'a mut [u8],
}

impl<'a> PixmapMut<'a> {
    /// Wraps an existing buffer; fails if its length does not match `size`.
    pub fn from_slice(
        data: &'a mut [u8],
        size: PixelSize,
        format: PixelFormat,
    ) -> Result<Self, EngineError> {
        let expected = (size.width as usize)
            .checked_mul(size.height as usize)
            .and_then(|px| px.checked_mul(PixelFormat::BYTES_PER_PIXEL));
        if expected != Some(data.len()) {
            return Err(EngineError::InvalidRequest(format!(
                "buffer of {} bytes does not match {}x{} pixels",
                data.len(),
                size.width,
                size.height
            )));
        }
        Ok(Self { size, format, data })
    }

    pub fn size(&self) -> PixelSize {
        self.size
    }

    pub fn format(&self) -> PixelFormat {
        self.format
    }

    pub fn data(&self) -> &[u8] {
        self.data
    }

    pub fn data_mut(&mut self) -> &mut [u8] {
        self.data
    }

    pub fn fill(&mut self, color: Rgba8) {
        let px = color.premultiplied_bytes(self.format);
        let (pixels, _) = self
            .data
            .as_chunks_mut::<{ PixelFormat::BYTES_PER_PIXEL }>();
        pixels.fill(px);
    }

    /// Copies premultiplied RGBA rows (`stride` bytes apart) into this
    /// target, converting to the target's channel order.
    pub fn copy_from_rgba(&mut self, src: &[u8], stride: usize) -> Result<(), EngineError> {
        let row_bytes = self.size.width as usize * PixelFormat::BYTES_PER_PIXEL;
        let rows = self.size.height as usize;
        let needed = stride
            .checked_mul(rows.saturating_sub(1))
            .and_then(|b| b.checked_add(row_bytes));
        if stride < row_bytes || needed.is_none_or(|n| src.len() < n) {
            return Err(EngineError::Internal("source bitmap too small".into()));
        }
        let format = self.format;
        for (dst_row, src_row) in self
            .data
            .chunks_exact_mut(row_bytes)
            .zip(src.chunks(stride))
        {
            let src_row = &src_row[..row_bytes];
            match format {
                PixelFormat::Rgba8Premultiplied => dst_row.copy_from_slice(src_row),
                PixelFormat::Bgra8Premultiplied => {
                    let (dst_px, _) = dst_row.as_chunks_mut::<4>();
                    let (src_px, _) = src_row.as_chunks::<4>();
                    for (d, s) in dst_px.iter_mut().zip(src_px) {
                        *d = [s[2], s[1], s[0], s[3]];
                    }
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocation_is_guarded() {
        let limits = ResourceLimits::default();
        assert!(
            Pixmap::new(
                PixelSize::new(100_000, 100_000),
                PixelFormat::default(),
                &limits
            )
            .is_err()
        );
        let pm = Pixmap::new(PixelSize::new(4, 2), PixelFormat::default(), &limits).unwrap();
        assert_eq!(pm.byte_len(), 32);
    }

    #[test]
    fn fill_and_swizzle() {
        let mut buf = vec![0u8; 8];
        let mut view = PixmapMut::from_slice(
            &mut buf,
            PixelSize::new(2, 1),
            PixelFormat::Bgra8Premultiplied,
        )
        .unwrap();
        view.fill(Rgba8::new(255, 0, 0, 255));
        assert_eq!(&buf[..4], &[0, 0, 255, 255]);
    }

    #[test]
    fn copy_converts_channel_order_and_honors_stride() {
        // 1x2 source with 8-byte stride (4 bytes padding per row).
        let src = [1, 2, 3, 4, 9, 9, 9, 9, 5, 6, 7, 8, 9, 9, 9, 9];
        let mut buf = vec![0u8; 8];
        let mut view = PixmapMut::from_slice(
            &mut buf,
            PixelSize::new(1, 2),
            PixelFormat::Bgra8Premultiplied,
        )
        .unwrap();
        view.copy_from_rgba(&src, 8).unwrap();
        assert_eq!(buf, vec![3, 2, 1, 4, 7, 6, 5, 8]);
    }

    #[test]
    fn mismatched_buffer_is_rejected() {
        let mut buf = vec![0u8; 7];
        assert!(
            PixmapMut::from_slice(&mut buf, PixelSize::new(2, 1), PixelFormat::default()).is_err()
        );
    }

    #[test]
    fn premultiply_rounds() {
        let c = Rgba8::new(255, 128, 0, 128);
        assert_eq!(
            c.premultiplied_bytes(PixelFormat::Rgba8Premultiplied),
            [128, 64, 0, 128]
        );
    }
}
