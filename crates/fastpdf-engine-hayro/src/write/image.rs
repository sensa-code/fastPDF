//! Stamp images as PDF image XObjects.
//!
//! JPEG files with 1 or 3 components (baseline, extended or progressive) are
//! embedded as they are (`/DCTDecode`): no quality loss, no re-encoding.
//! Everything else (PNG, other JPEGs) is decoded with the `image` crate,
//! already linked for GPUI, into 8-bit RGB plus an alpha soft mask when the
//! image has transparency, both Flate-compressed. Image files are user
//! input too: sizes are bounded before anything is decoded.

use std::io::Write as _;
use std::path::Path;

use flate2::Compression;
use flate2::write::ZlibEncoder;

/// Larger stamp files are refused before reading.
const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;
/// Largest decoded image (width x height); a seal at 1200 dpi over 10 cm
/// is about 4700 x 4700.
const MAX_PIXELS: u64 = 64 * 1024 * 1024;
const MAX_SIDE: u32 = 30_000;

/// An image XObject (and its soft mask) ready to be written.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ImageObject {
    /// Dictionary entries without `/Length` and without `/SMask`.
    pub dict: String,
    pub data: Vec<u8>,
    /// `/DeviceGray` alpha channel: dictionary entries and data.
    pub smask: Option<(String, Vec<u8>)>,
    pub width: u32,
    pub height: u32,
}

/// Reads and converts the image at `path`.
pub(crate) fn load(path: &Path) -> Result<ImageObject, String> {
    let size = std::fs::metadata(path).map_err(|e| e.to_string())?.len();
    if size > MAX_FILE_BYTES {
        return Err(format!("larger than {} MB", MAX_FILE_BYTES >> 20));
    }
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    from_bytes(&bytes)
}

pub(crate) fn from_bytes(bytes: &[u8]) -> Result<ImageObject, String> {
    if let Some((width, height, components)) = jpeg_info(bytes)
        && matches!(components, 1 | 3)
    {
        check_size(width, height)?;
        let color = if components == 1 {
            "/DeviceGray"
        } else {
            "/DeviceRGB"
        };
        return Ok(ImageObject {
            dict: format!(
                "/Type /XObject /Subtype /Image /Width {width} /Height {height} \
                 /ColorSpace {color} /BitsPerComponent 8 /Filter /DCTDecode"
            ),
            data: bytes.to_vec(),
            smask: None,
            width,
            height,
        });
    }
    let reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    let (width, height) = reader
        .into_dimensions()
        .map_err(|e| format!("not a supported image: {e}"))?;
    check_size(width, height)?;
    let decoded = image::load_from_memory(bytes)
        .map_err(|e| format!("cannot decode: {e}"))?
        .to_rgba8();
    let pixels = decoded.as_raw();
    let count = width as usize * height as usize;
    let mut rgb = Vec::with_capacity(count * 3);
    let mut alpha = Vec::with_capacity(count);
    for px in pixels.as_chunks::<4>().0 {
        rgb.extend_from_slice(&px[..3]);
        alpha.push(px[3]);
    }
    let smask = alpha.iter().any(|&a| a != 255).then(|| {
        (
            format!(
                "/Type /XObject /Subtype /Image /Width {width} /Height {height} \
                 /ColorSpace /DeviceGray /BitsPerComponent 8 /Filter /FlateDecode"
            ),
            deflate(&alpha),
        )
    });
    Ok(ImageObject {
        dict: format!(
            "/Type /XObject /Subtype /Image /Width {width} /Height {height} \
             /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /FlateDecode"
        ),
        data: deflate(&rgb),
        smask,
        width,
        height,
    })
}

fn check_size(width: u32, height: u32) -> Result<(), String> {
    if width == 0 || height == 0 {
        return Err("empty image".into());
    }
    if width > MAX_SIDE || height > MAX_SIDE || u64::from(width) * u64::from(height) > MAX_PIXELS {
        return Err(format!("{width} x {height} pixels is too large"));
    }
    Ok(())
}

pub(crate) fn deflate(data: &[u8]) -> Vec<u8> {
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::new(6));
    // Writing into a Vec cannot fail.
    let _ = encoder.write_all(data);
    encoder.finish().unwrap_or_default()
}

/// Width, height and components of an 8-bit baseline, extended or
/// progressive JPEG (the kinds `/DCTDecode` takes as they are).
fn jpeg_info(data: &[u8]) -> Option<(u32, u32, u8)> {
    if !data.starts_with(&[0xFF, 0xD8]) {
        return None;
    }
    let mut i = 2;
    while i + 4 <= data.len() {
        if data[i] != 0xFF {
            return None;
        }
        let marker = data[i + 1];
        if marker == 0xFF {
            i += 1;
            continue;
        }
        if marker == 0x01 || (0xD0..=0xD8).contains(&marker) {
            i += 2;
            continue;
        }
        let len = usize::from(u16::from_be_bytes([data[i + 2], data[i + 3]]));
        if len < 2 {
            return None;
        }
        if matches!(marker, 0xC0..=0xCF) && !matches!(marker, 0xC4 | 0xC8 | 0xCC) {
            let p = i + 4;
            let sof = data.get(p..p + 6)?;
            let supported = matches!(marker, 0xC0..=0xC2) && sof[0] == 8;
            let height = u32::from(u16::from_be_bytes([sof[1], sof[2]]));
            let width = u32::from(u16::from_be_bytes([sof[3], sof[4]]));
            return supported.then_some((width, height, sof[5]));
        }
        i += 2 + len;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(width: u32, height: u32, alpha: u8) -> Vec<u8> {
        let mut img = image::RgbaImage::new(width, height);
        for (x, y, px) in img.enumerate_pixels_mut() {
            *px = image::Rgba([(x * 40) as u8, (y * 40) as u8, 200, alpha]);
        }
        let mut out = std::io::Cursor::new(Vec::new());
        img.write_to(&mut out, image::ImageFormat::Png).unwrap();
        out.into_inner()
    }

    #[test]
    fn transparent_pngs_get_a_soft_mask() {
        let image = from_bytes(&png(4, 3, 128)).unwrap();
        assert_eq!((image.width, image.height), (4, 3));
        assert!(image.dict.contains("/FlateDecode"));
        let (_, mask) = image.smask.expect("alpha below 255");
        let mut alpha = Vec::new();
        use std::io::Read as _;
        flate2::read::ZlibDecoder::new(mask.as_slice())
            .read_to_end(&mut alpha)
            .unwrap();
        assert_eq!(alpha, vec![128; 12]);
        assert!(from_bytes(&png(4, 3, 255)).unwrap().smask.is_none());
    }

    #[test]
    fn jpegs_are_embedded_as_they_are() {
        let img = image::RgbImage::from_pixel(5, 7, image::Rgb([200, 20, 20]));
        let mut out = std::io::Cursor::new(Vec::new());
        img.write_to(&mut out, image::ImageFormat::Jpeg).unwrap();
        let jpeg = out.into_inner();
        let image = from_bytes(&jpeg).unwrap();
        assert_eq!((image.width, image.height), (5, 7));
        assert!(image.dict.contains("/DCTDecode"));
        assert!(image.dict.contains("/DeviceRGB"));
        assert_eq!(image.data, jpeg);
    }

    #[test]
    fn junk_and_bombs_are_refused() {
        assert!(from_bytes(b"not an image").is_err());
        assert!(from_bytes(&[0xFF, 0xD8, 0xFF]).is_err());
        assert!(check_size(0, 10).is_err());
        assert!(check_size(20_000, 20_000).is_err());
        assert!(check_size(4_000, 4_000).is_ok());
        // A JPEG header claiming 65535 x 65535 pixels.
        let mut bomb = vec![
            0xFF, 0xD8, 0xFF, 0xC0, 0x00, 0x11, 8, 0xFF, 0xFF, 0xFF, 0xFF, 3,
        ];
        bomb.extend_from_slice(&[0; 12]);
        assert!(from_bytes(&bomb).is_err());
    }
}
