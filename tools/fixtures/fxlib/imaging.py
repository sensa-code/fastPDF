"""Deterministic image synthesis and encoding helpers (Pillow only)."""

from __future__ import annotations

import io
import random
from typing import Any

from PIL import Image, ImageChops, ImageDraw, ImageFilter, ImageOps


def noise(r: random.Random, size: tuple[int, int], amp: int) -> Image.Image:
    """Uniform noise in [0, amp] as an 'L' image (seeded, no C-level RNG)."""
    img = Image.frombytes("L", size, r.randbytes(size[0] * size[1]))
    return img.point([v * amp // 255 for v in range(256)])


def add_noise(img: Image.Image, r: random.Random, amp: int) -> Image.Image:
    n = noise(r, img.size, amp)
    if img.mode != "L":
        n = Image.merge(img.mode, [n] * len(img.getbands()))
    return ImageChops.add(img, n, 1.0, -(amp // 2))


def synth_photo(r: random.Random, w: int, h: int) -> Image.Image:
    """Photo-like RGB image: colour gradient, soft blobs, texture noise."""
    top = tuple(r.randint(30, 220) for _ in range(3))
    bottom = tuple(r.randint(30, 220) for _ in range(3))
    grad = Image.linear_gradient("L").resize((w, h))
    img = ImageOps.colorize(grad, black=top, white=bottom)
    d = ImageDraw.Draw(img)
    for _ in range(36):
        cx, cy = r.randrange(w), r.randrange(h)
        rx, ry = r.randint(w // 40, w // 5), r.randint(h // 40, h // 5)
        d.ellipse([cx - rx, cy - ry, cx + rx, cy + ry],
                  fill=tuple(r.randint(0, 255) for _ in range(3)))
    for _ in range(18):
        pts = [(r.randrange(w), r.randrange(h)) for _ in range(r.randint(3, 6))]
        d.polygon(pts, fill=tuple(r.randint(0, 255) for _ in range(3)))
    img = img.filter(ImageFilter.GaussianBlur(max(1.0, w / 260)))
    return add_noise(img, r, 22)


def synth_chart(r: random.Random, w: int, h: int) -> Image.Image:
    """Screenshot/chart-like RGB image: flat colours, compresses well with Flate."""
    img = Image.new("RGB", (w, h), (250, 250, 252))
    d = ImageDraw.Draw(img)
    for x in range(0, w, max(8, w // 24)):
        d.line([(x, 0), (x, h)], fill=(225, 228, 235))
    for y in range(0, h, max(8, h // 16)):
        d.line([(0, y), (w, y)], fill=(225, 228, 235))
    for series in range(4):
        color = [(31, 119, 180), (255, 127, 14), (44, 160, 44), (214, 39, 40)][series]
        y = r.uniform(0.3, 0.7) * h
        pts = []
        for x in range(0, w + 1, max(4, w // 120)):
            y = min(h - 2, max(2, y + r.uniform(-h / 40, h / 40)))
            pts.append((x, y))
        d.line(pts, fill=color, width=max(2, w // 400))
    for i in range(12):
        x0 = int(w * 0.05 + i * w * 0.075)
        bh = int(r.uniform(0.05, 0.3) * h)
        d.rectangle([x0, h - bh, x0 + int(w * 0.05), h], fill=(120, 120 + i * 8, 200))
    return img


def jpeg(img: Image.Image, quality: int = 85, **kw: Any) -> bytes:
    buf = io.BytesIO()
    img.save(buf, "JPEG", quality=quality, optimize=False, **kw)
    return buf.getvalue()


def png_parts(img: Image.Image) -> dict[str, Any]:
    """Return IDAT payload (PNG-predicted zlib stream), bit depth and palette."""
    buf = io.BytesIO()
    img.save(buf, "PNG", compress_level=6)
    data = buf.getvalue()
    pos, idat, plte, bits = 8, bytearray(), b"", 8
    while pos < len(data):
        length = int.from_bytes(data[pos:pos + 4], "big")
        ctype = data[pos + 4:pos + 8]
        body = data[pos + 8:pos + 8 + length]
        if ctype == b"IHDR":
            bits = body[8]
        elif ctype == b"PLTE":
            plte = bytes(body)
        elif ctype == b"IDAT":
            idat += body
        pos += 12 + length
    return {"idat": bytes(idat), "bits": bits, "palette": plte}


def scan_effects(img: Image.Image, r: random.Random, *, max_angle: float = 1.1,
                 noise_amp: int = 14, blur: float = 0.55, paper: int = 238,
                 speckles: int = 400) -> Image.Image:
    """Simulate a flatbed scan: skew, paper tone, sensor noise, blur, dust."""
    fill = 255 if img.mode == "L" else (255,) * len(img.getbands())
    img = img.rotate(r.uniform(-max_angle, max_angle), resample=Image.Resampling.BICUBIC,
                     fillcolor=fill)
    lut = [v * paper // 255 for v in range(256)]
    img = img.point(lut * len(img.getbands()))
    img = add_noise(img, r, noise_amp)
    img = img.filter(ImageFilter.GaussianBlur(blur))
    d = ImageDraw.Draw(img)
    w, h = img.size
    dark = 40 if img.mode == "L" else (40,) * len(img.getbands())
    for _ in range(speckles):
        x, y, s = r.randrange(w), r.randrange(h), r.choice((1, 1, 1, 2, 3))
        d.ellipse([x, y, x + s, y + s], fill=dark)
    # darker scanner-lid shadow along one edge
    edge = r.choice(("left", "right"))
    for i in range(18):
        x = i if edge == "left" else w - 1 - i
        d.line([(x, 0), (x, h)], fill=(90 + i * 8) if img.mode == "L" else (90 + i * 8,) * 3)
    return img


def g4_encode(bilevel: Image.Image) -> bytes:
    """CCITT Group 4 data (single strip) where ink is encoded as *black* runs.

    Pillow stores mode "1" as BlackIsZero, and libtiff codes 0-bits as white
    runs, so the image is inverted first to get the semantic fax polarity that
    PDF's CCITTFaxDecode expects with /BlackIs1 false.
    """
    inv = ImageOps.invert(bilevel.convert("L")).convert("1", dither=Image.Dither.NONE)
    buf = io.BytesIO()
    inv.save(buf, "TIFF", compression="group4", strip_size=1 << 30)
    buf.seek(0)
    tif = Image.open(buf)
    offsets, counts = tif.tag_v2[273], tif.tag_v2[279]
    if len(offsets) != 1:
        raise RuntimeError("expected a single TIFF strip")
    raw = buf.getvalue()
    return raw[offsets[0]:offsets[0] + counts[0]]


def g4_decode_check(data: bytes, w: int, h: int) -> Image.Image:
    """Decode G4 data via a minimal WhiteIsZero TIFF (used for self-checks)."""
    import struct

    entries = [(256, 4, 1, w), (257, 4, 1, h), (258, 3, 1, 1), (259, 3, 1, 4),
               (262, 3, 1, 0), (273, 4, 1, 0), (277, 3, 1, 1), (278, 4, 1, h),
               (279, 4, 1, len(data))]
    ifd_off = 8
    data_off = ifd_off + 2 + len(entries) * 12 + 4
    out = bytearray(b"II*\x00" + struct.pack("<I", ifd_off) + struct.pack("<H", len(entries)))
    for tag, typ, cnt, val in entries:
        if tag == 273:
            val = data_off
        if typ == 3:
            out += struct.pack("<HHIHH", tag, typ, cnt, val, 0)
        else:
            out += struct.pack("<HHII", tag, typ, cnt, val)
    out += b"\x00\x00\x00\x00" + data
    img = Image.open(io.BytesIO(bytes(out)))
    img.load()
    return img


def srgb_icc_profile() -> bytes:
    """sRGB ICC profile from LittleCMS with the creation date/ID zeroed."""
    from PIL import ImageCms

    prof = ImageCms.ImageCmsProfile(ImageCms.createProfile("sRGB"))
    data = bytearray(prof.tobytes())
    data[24:36] = bytes(12)   # dateTimeNumber
    data[84:100] = bytes(16)  # profile ID (MD5)
    return bytes(data)
