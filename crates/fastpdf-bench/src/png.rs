//! Minimal PNG writer (stored deflate blocks) for visual checks of renders.
//! Output is large but needs no compression dependency.

use std::io::Write;
use std::path::Path;

use fastpdf_engine_api::{PixelFormat, Pixmap};

pub(crate) fn write_rgba(path: &Path, pixmap: &Pixmap) -> std::io::Result<()> {
    let size = pixmap.size();
    let (w, h) = (size.width as usize, size.height as usize);
    let mut raw = Vec::with_capacity(h * (w * 4 + 1));
    for row in pixmap.data().chunks_exact(w * 4) {
        raw.push(0); // filter: none
        let (pixels, _) = row.as_chunks::<4>();
        for px in pixels {
            let [r, g, b, a] = match pixmap.format() {
                PixelFormat::Rgba8Premultiplied => *px,
                PixelFormat::Bgra8Premultiplied => [px[2], px[1], px[0], px[3]],
            };
            raw.extend_from_slice(&unpremultiply([r, g, b, a]));
        }
    }

    let mut out = Vec::with_capacity(raw.len() + raw.len() / 65_535 * 5 + 64);
    out.extend_from_slice(b"\x89PNG\r\n\x1a\n");
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&(w as u32).to_be_bytes());
    ihdr.extend_from_slice(&(h as u32).to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]); // 8-bit RGBA, no interlace
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &zlib_stored(&raw));
    chunk(&mut out, b"IEND", &[]);
    std::fs::File::create(path)?.write_all(&out)
}

fn unpremultiply([r, g, b, a]: [u8; 4]) -> [u8; 4] {
    if a == 0 || a == 255 {
        return [r, g, b, a];
    }
    let un = |c: u8| ((u16::from(c) * 255 + u16::from(a) / 2) / u16::from(a)).min(255) as u8;
    [un(r), un(g), un(b), a]
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let start = out.len();
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let crc = crc32(&out[start..]);
    out.extend_from_slice(&crc.to_be_bytes());
}

fn zlib_stored(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x78, 0x01];
    let mut blocks = data.chunks(65_535).peekable();
    if blocks.peek().is_none() {
        out.extend_from_slice(&[1, 0, 0, 0xff, 0xff]);
    }
    while let Some(block) = blocks.next() {
        out.push(u8::from(blocks.peek().is_none()));
        let len = block.len() as u16;
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&(!len).to_le_bytes());
        out.extend_from_slice(block);
    }
    out.extend_from_slice(&adler32(data).to_be_bytes());
    out
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xedb8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for chunk in data.chunks(5552) {
        for &byte in chunk {
            a += u32::from(byte);
            b += a;
        }
        a %= 65_521;
        b %= 65_521;
    }
    (b << 16) | a
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksums_match_known_values() {
        assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
        assert_eq!(adler32(b"Wikipedia"), 0x11e6_0398);
    }

    #[test]
    fn unpremultiply_restores_color() {
        assert_eq!(unpremultiply([64, 0, 0, 128]), [128, 0, 0, 128]);
        assert_eq!(unpremultiply([0, 0, 0, 0]), [0, 0, 0, 0]);
    }
}
