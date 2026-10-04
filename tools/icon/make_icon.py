# /// script
# requires-python = ">=3.12"
# dependencies = [
#     "pillow==12.3.0",
# ]
# [tool.uv]
# exclude-newer = "2026-10-05T00:00:00Z"
# ///
"""Generate the FastPDF application icon (spec §33, Explorer integration).

    uv run tools/icon/make_icon.py [--out crates/fastpdf-app/assets/fastpdf.ico]
                                   [--msix-assets packaging/msix/Assets | --no-msix-assets]
                                   [--preview preview.png]

Original artwork drawn here from simple polygons (no third-party icon assets):
a white page with a folded corner and a lightning bolt ("fast").

Each size is rendered separately at 8x supersampling and box-filtered down, so
small sizes stay crisp. The .ico container is written by hand: 16-64 px as
32-bit BMP (DIB + AND mask, the most compatible form) and 256 px as PNG.
The same artwork is written as the MSIX logos that packaging/msix/AppxManifest.xml
references (scale-100 base images, ADR 0010). Output is deterministic for a
given Pillow version.
"""

from __future__ import annotations

import argparse
import hashlib
import io
import struct
import sys
from pathlib import Path

from PIL import Image, ImageDraw

SIZES = (16, 24, 32, 48, 64, 256)
SUPERSAMPLE = 8
GRID = 256.0  # design coordinates

PAGE_FILL = (255, 255, 255, 255)
PAGE_EDGE = (31, 78, 140, 255)  # slate blue
FOLD_FILL = (214, 226, 242, 255)
LINE_FILL = (185, 201, 222, 255)
BOLT_FILL = (255, 179, 0, 255)  # amber
BOLT_EDGE = (163, 82, 0, 255)

# Page with a folded top-right corner (design grid 0..256).
PAGE = [(40, 14), (160, 14), (210, 64), (210, 242), (40, 242)]
FOLD = [(160, 14), (160, 64), (210, 64)]
# Classic seven-point lightning bolt, sitting on the page.
BOLT = [(116, 46), (178, 46), (148, 112), (192, 112), (100, 236), (124, 140), (82, 140)]
# "Text" lines on the page (only drawn where they stay readable).
LINES = [(62, 92, 112, 104), (62, 124, 104, 136), (62, 172, 92, 184), (150, 200, 190, 212)]


# Page outline width in output pixels; other sizes use 9 design units.
EDGE_PX = {16: 1, 24: 1, 32: 1, 44: 2, 48: 2, 50: 2, 64: 2, 104: 4}

# MSIX logos: file name -> (image px, icon px). The icon is centred on a
# transparent canvas; tiles get padding, list/taskbar sizes are full-bleed.
MSIX_LOGOS = {
    "StoreLogo.png": (50, 50),
    "Square44x44Logo.png": (44, 44),
    "Square150x150Logo.png": (150, 104),
}


def inset(poly: list[tuple[float, float]], t: float) -> list[tuple[float, float]]:
    """Offset a convex polygon inwards by t (edge lines moved along inward normals, then re-intersected)."""
    n = len(poly)
    cx = sum(x for x, _ in poly) / n
    cy = sum(y for _, y in poly) / n
    lines = []
    for i in range(n):
        (x0, y0), (x1, y1) = poly[i], poly[(i + 1) % n]
        dx, dy = x1 - x0, y1 - y0
        length = (dx * dx + dy * dy) ** 0.5
        nx, ny = -dy / length, dx / length
        if nx * (cx - (x0 + x1) / 2) + ny * (cy - (y0 + y1) / 2) < 0:
            nx, ny = -nx, -ny
        lines.append(((x0 + nx * t, y0 + ny * t), (dx, dy)))
    out = []
    for i in range(n):
        (px, py), (dx1, dy1) = lines[i - 1]
        (qx, qy), (dx2, dy2) = lines[i]
        det = dx1 * dy2 - dy1 * dx2
        u = ((qx - px) * dy2 - (qy - py) * dx2) / det
        out.append((px + dx1 * u, py + dy1 * u))
    return out


def render(size: int) -> Image.Image:
    """Render one icon size with supersampling."""
    ss = SUPERSAMPLE
    canvas = size * ss
    s = canvas / GRID  # design unit -> canvas unit
    img = Image.new("RGBA", (canvas, canvas), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)

    # Page geometry snapped to whole output pixels. PIL fills polygons
    # inclusively, so a right/bottom boundary at output pixel R is R*ss - 1.
    k = size / GRID
    left, right = round(40 * k), round(210 * k)
    top, bottom = round(14 * k), round(242 * k)
    corner = max(2, round(50 * k))
    x0, x1, y0, y1, f = left * ss, right * ss - 1, top * ss, bottom * ss - 1, corner * ss
    page = [(x0, y0), (x1 - f, y0), (x1, y0 + f), (x1, y1), (x0, y1)]
    fold = [(x1 - f, y0), (x1 - f, y0 + f), (x1, y0 + f)]
    t = EDGE_PX.get(size, 9 * k) * ss  # outline width in canvas pixels

    d.polygon(page, fill=PAGE_EDGE)
    d.polygon(inset(page, t), fill=PAGE_FILL)
    if size >= 32:
        for lx0, ly0, lx1, ly1 in LINES:
            d.rectangle([lx0 * s, ly0 * s, lx1 * s, ly1 * s], fill=LINE_FILL)
    d.polygon(fold, fill=PAGE_EDGE)
    d.polygon(inset(fold, t * 0.8), fill=FOLD_FILL)

    bolt = [(x * s, y * s) for x, y in BOLT]
    cx = sum(x for x, _ in bolt) / len(bolt)
    cy = sum(y for _, y in bolt) / len(bolt)
    if size < 32:
        # Small sizes: a slightly fatter bolt with a solid dark rim reads better
        # than a thin outline.
        bolt = [(cx + (x - cx) * 1.12, cy + (y - cy) * 1.0) for x, y in bolt]
        d.polygon(bolt, fill=BOLT_EDGE)
        inner = [(cx + (x - cx) * 0.84, cy + (y - cy) * 0.88) for x, y in bolt]
        d.polygon(inner, fill=BOLT_FILL)
    else:
        bolt_w = max(round(6.0 * s), ss)  # at least one output pixel
        d.polygon(bolt, fill=BOLT_FILL)
        d.line(bolt + [bolt[0]], fill=BOLT_EDGE, width=bolt_w, joint="curve")

    return img.resize((size, size), Image.Resampling.BOX)


def bmp_entry(img: Image.Image) -> bytes:
    """32-bit DIB for an ICO entry: BITMAPINFOHEADER + BGRA rows (bottom-up) + AND mask."""
    w, h = img.size
    header = struct.pack("<IiiHHIIiiII", 40, w, h * 2, 1, 32, 0, 0, 0, 0, 0, 0)
    px = img.load()
    rows = bytearray()
    for y in range(h - 1, -1, -1):
        for x in range(w):
            r, g, b, a = px[x, y]
            rows += bytes((b, g, r, a))
    stride = ((w + 31) // 32) * 4
    mask = bytearray()
    for y in range(h - 1, -1, -1):
        row = bytearray(stride)
        for x in range(w):
            if px[x, y][3] == 0:
                row[x // 8] |= 0x80 >> (x % 8)
        mask += row
    return header + bytes(rows) + bytes(mask)


def png_entry(img: Image.Image) -> bytes:
    buf = io.BytesIO()
    img.save(buf, format="PNG", optimize=True)
    return buf.getvalue()


def msix_logo(size: int, icon: int) -> Image.Image:
    """The icon rendered at `icon` px, centred on a transparent `size` px canvas."""
    canvas = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    offset = (size - icon) // 2
    canvas.alpha_composite(render(icon), (offset, offset))
    return canvas


def build_ico(images: dict[int, Image.Image]) -> bytes:
    entries = []
    for size in SIZES:
        data = png_entry(images[size]) if size >= 256 else bmp_entry(images[size])
        entries.append((size, data))
    header = struct.pack("<HHH", 0, 1, len(entries))
    offset = 6 + 16 * len(entries)
    directory = bytearray()
    blobs = bytearray()
    for size, data in entries:
        dim = 0 if size >= 256 else size
        directory += struct.pack("<BBBBHHII", dim, dim, 0, 0, 1, 32, len(data), offset + len(blobs))
        blobs += data
    return header + bytes(directory) + bytes(blobs)


def main() -> int:
    root = Path(__file__).resolve().parents[2]
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--out", type=Path, default=root / "crates/fastpdf-app/assets/fastpdf.ico")
    ap.add_argument("--msix-assets", type=Path, default=root / "packaging/msix/Assets",
                    help="directory for the MSIX logo PNGs")
    ap.add_argument("--no-msix-assets", action="store_true", help="write only the .ico")
    ap.add_argument("--preview", type=Path, help="also write a PNG contact sheet (not committed)")
    args = ap.parse_args()

    images = {size: render(size) for size in SIZES}
    ico = build_ico(images)
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_bytes(ico)

    if args.preview:
        cell = 272
        sheet = Image.new("RGBA", (cell * len(SIZES), cell * 2), (0, 0, 0, 0))
        for i, size in enumerate(SIZES):
            img = images[size]
            big = img.resize((min(256, size * (256 // size)),) * 2, Image.Resampling.NEAREST)
            for row, bg in enumerate(((255, 255, 255, 255), (32, 32, 32, 255))):
                tile = Image.new("RGBA", (cell, cell), bg)
                tile.alpha_composite(big, ((cell - big.width) // 2, (cell - big.height) // 2))
                sheet.paste(tile, (i * cell, row * cell))
        sheet.save(args.preview)

    digest = hashlib.sha256(ico).hexdigest()
    print(f"wrote {args.out} ({len(ico)} bytes, sizes {', '.join(map(str, SIZES))}) sha256 {digest}")

    if not args.no_msix_assets:
        args.msix_assets.mkdir(parents=True, exist_ok=True)
        for name, (size, icon) in MSIX_LOGOS.items():
            data = png_entry(msix_logo(size, icon))
            (args.msix_assets / name).write_bytes(data)
            print(f"wrote {args.msix_assets / name} ({size}x{size}, {len(data)} bytes) "
                  f"sha256 {hashlib.sha256(data).hexdigest()}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
