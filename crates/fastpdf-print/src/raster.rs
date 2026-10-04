//! Band post-processing before it goes to the spooler: find the inked part
//! and pack it in the smallest lossless DIB layout, so white paper is never
//! sent and gray pages cost one byte per pixel. Platform independent.

use fastpdf_engine_api::{PixelRect, PixelSize};

/// An opaque white pixel in any 4-byte premultiplied format.
const WHITE: [u8; 4] = [u8::MAX; 4];

/// Bottom-up DIBs need every row padded to a multiple of 4 bytes; top-down
/// ones too.
fn dib_stride(row_bytes: usize) -> usize {
    row_bytes.div_ceil(4) * 4
}

/// Pixel layout of a packed band.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DibFormat {
    /// 8-bit gray levels (a DIB with a gray palette).
    Gray8,
    /// 24-bit B, G, R.
    Bgr24,
}

/// A band packed at the start of its buffer: `height` rows of `stride`
/// bytes (rows padded to 4 bytes, as DIBs require).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Packed {
    pub(crate) format: DibFormat,
    pub(crate) stride: usize,
    pub(crate) len: usize,
}

/// Bounding box of the pixels that are not paper-white, or `None` for an
/// all-white band. `pixels` holds tightly packed 4-byte pixels.
pub(crate) fn ink_bounds(pixels: &[u8], size: PixelSize) -> Option<PixelRect> {
    let width = size.width as usize;
    if width == 0 {
        return None;
    }
    let (px, _) = pixels.as_chunks::<4>();
    let rows = px.chunks_exact(width).take(size.height as usize);
    let mut first_row = None;
    let (mut last_row, mut left, mut right) = (0, width, 0);
    for (y, row) in rows.enumerate() {
        let Some(l) = row.iter().position(|p| *p != WHITE) else {
            continue;
        };
        // `l` found, so a last inked pixel exists too.
        let r = row.iter().rposition(|p| *p != WHITE).unwrap_or(l);
        first_row.get_or_insert(y);
        last_row = y;
        left = left.min(l);
        right = right.max(r + 1);
    }
    let first_row = first_row?;
    Some(PixelRect::new(
        left as u32,
        first_row as u32,
        (right - left) as u32,
        (last_row + 1 - first_row) as u32,
    ))
}

/// Packs the opaque BGRA pixels of `rect` (within a `width`-pixel-wide band)
/// to the start of `pixels`: as 8-bit gray when every pixel is gray, else as
/// 24-bit BGR. Alpha is dropped (the engine paints an opaque background).
pub(crate) fn pack(pixels: &mut [u8], width: u32, rect: PixelRect) -> Packed {
    let src_stride = width as usize * 4;
    let (x0, y0) = (rect.x as usize, rect.y as usize);
    let (w, h) = (rect.width as usize, rect.height as usize);
    let src_row = |r: usize| (y0 + r) * src_stride + x0 * 4;
    let gray = (0..h).all(|r| {
        let (row, _) = pixels[src_row(r)..src_row(r) + w * 4].as_chunks::<4>();
        row.iter().all(|p| p[0] == p[1] && p[1] == p[2])
    });
    let (format, bytes_per_pixel) = if gray {
        (DibFormat::Gray8, 1)
    } else {
        (DibFormat::Bgr24, 3)
    };
    let stride = dib_stride(w * bytes_per_pixel);
    // In place, front to back: a packed row never starts after its source
    // row (stride <= 4 * w), and inside a row pixel `i` is written to bytes
    // that pixels `i..` have already been read from, so nothing is
    // overwritten before it is read. Row padding ends before the next
    // source row starts.
    for r in 0..h {
        let (src, dst) = (src_row(r), r * stride);
        for i in 0..w {
            let s = src + i * 4;
            let d = dst + i * bytes_per_pixel;
            if gray {
                pixels[d] = pixels[s];
            } else {
                pixels.copy_within(s..s + 3, d);
            }
        }
        pixels[dst + w * bytes_per_pixel..dst + stride].fill(0);
    }
    Packed {
        format,
        stride,
        len: h * stride,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A white band with colored ink at the given pixels.
    fn band(width: u32, height: u32, ink: &[(u32, u32)]) -> Vec<u8> {
        let mut px = vec![u8::MAX; (width * height * 4) as usize];
        for &(x, y) in ink {
            let i = ((y * width + x) * 4) as usize;
            px[i..i + 4].copy_from_slice(&[x as u8, y as u8, 7, 255]);
        }
        px
    }

    #[test]
    fn white_bands_have_no_ink() {
        let px = band(5, 3, &[]);
        assert_eq!(ink_bounds(&px, PixelSize::new(5, 3)), None);
    }

    #[test]
    fn ink_bounds_cover_every_inked_pixel() {
        let px = band(10, 6, &[(3, 1), (7, 4), (5, 2)]);
        assert_eq!(
            ink_bounds(&px, PixelSize::new(10, 6)),
            Some(PixelRect::new(3, 1, 5, 4))
        );
        // A translucent-white pixel is ink too (it is not paper color).
        let mut px = band(4, 2, &[]);
        px[4 * 4 + 3] = 254; // (0, 1) alpha
        assert_eq!(
            ink_bounds(&px, PixelSize::new(4, 2)),
            Some(PixelRect::new(0, 1, 1, 1))
        );
    }

    #[test]
    fn color_ink_packs_as_padded_bgr_rows() {
        let mut px = band(6, 4, &[(1, 1), (3, 2), (2, 3)]);
        let rect = PixelRect::new(1, 1, 3, 3);
        let expected: Vec<u8> = (1..4)
            .flat_map(|y| {
                let mut row: Vec<u8> = (1..4)
                    .flat_map(|x| {
                        let i = ((y * 6 + x) * 4) as usize;
                        px[i..i + 3].to_vec()
                    })
                    .collect();
                row.extend([0, 0, 0]); // 9 bytes padded to 12
                row
            })
            .collect();
        let packed = pack(&mut px, 6, rect);
        assert_eq!(packed.format, DibFormat::Bgr24);
        assert_eq!((packed.stride, packed.len), (12, 36));
        assert_eq!(&px[..packed.len], expected.as_slice());
    }

    #[test]
    fn gray_ink_packs_as_one_byte_per_pixel() {
        let mut px = band(5, 3, &[]);
        for (i, level) in [(6usize, 10u8), (7, 20), (12, 30)] {
            px[i * 4..i * 4 + 3].fill(level);
        }
        // Pixels 6, 7 are (1, 1), (2, 1); pixel 12 is (2, 2).
        let packed = pack(&mut px, 5, PixelRect::new(1, 1, 2, 2));
        assert_eq!(packed.format, DibFormat::Gray8);
        assert_eq!((packed.stride, packed.len), (4, 8));
        assert_eq!(&px[..8], &[10, 20, 0, 0, 255, 30, 0, 0]);
    }
}
