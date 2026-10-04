"""Categories: scanned, image-heavy, large-file."""

from __future__ import annotations

import io
import math
from pathlib import Path
from typing import Any, Callable

from PIL import Image, ImageDraw

from .common import Ctx, Job, latin_paragraph, latin_sentence, rng, wrap
from .imaging import (g4_encode, jpeg, png_parts, scan_effects, srgb_icc_profile, synth_chart,
                      synth_photo)
from .painter import A4, RasterPainter
from .rawpdf import Content, Hex, RawPdf, Ref, num, pdf_lit, simple_document, std_font
from .rl import rl_canvas, stringWidth

PILLOW_FONT = ("<pillow-default>", 0)


# --------------------------------------------------------------------------
# Generic "image per page" writer (used by scans and by large-file)
# --------------------------------------------------------------------------
def image_xobject(pdf: RawPdf, page: dict[str, Any]) -> Ref:
    d: dict[str, Any] = {"Type": "XObject", "Subtype": "Image", "Width": page["w"],
                         "Height": page["h"], "ColorSpace": page.get("cs", "DeviceGray"),
                         "BitsPerComponent": page.get("bpc", 8), "Filter": page["filter"]}
    if page.get("decode_parms"):
        d["DecodeParms"] = page["decode_parms"]
    if page.get("decode"):
        d["Decode"] = page["decode"]
    return pdf.stream(page["data"], d, compress=False)


def write_scanned_pdf(out: Path, pages: list[dict[str, Any]], *, title: str, id_seed: str,
                      pagesize: tuple[float, float] = A4) -> int:
    W, H = pagesize
    with open(out, "wb") as fp:
        pdf = RawPdf(fp, "1.7")
        font = pdf.obj(std_font("Helvetica"))
        pages_ref = pdf.alloc()
        kids = []
        for page in pages:
            img = image_xobject(pdf, page)
            cs = Content()
            cs(f"q {num(W)} 0 0 {num(H)} 0 0 cm /Im0 Do Q")
            if page.get("ocr"):
                cs("BT 3 Tr")
                for x, y, s, size, tz in page["ocr"]:
                    cs(f"/F1 {num(size)} Tf {num(tz)} Tz 1 0 0 1 {num(x)} {num(y)} Tm "
                       f"{pdf_lit(s.encode('cp1252', 'replace'))} Tj")
                cs("ET")
            content = pdf.stream(cs.data())
            kids.append(pdf.obj({"Type": "Page", "Parent": pages_ref, "MediaBox": [0, 0, W, H],
                                 "Resources": {"XObject": {"Im0": img}, "Font": {"F1": font}},
                                 "Contents": content}))
        simple_document(pdf, kids, pages_ref, title=title, id_seed=id_seed)
    return len(kids)


class RecordingPainter:
    """Forwards to a painter and records text runs (for OCR text layers)."""

    def __init__(self, inner: Any):
        self.inner = inner
        self.page_w, self.page_h = inner.page_w, inner.page_h
        self.records: list[list[tuple[float, float, str, float, float]]] = [[]]

    def text(self, x, y, s, size, role="body", color=(0, 0, 0)):
        self.inner.text(x, y, s, size, role, color)
        natural = stringWidth(s, "Helvetica", size)
        tz = 100.0 * self.inner.width(s, size, role) / natural if natural else 100.0
        self.records[-1].append((x, y, s, size, tz))

    def width(self, s, size, role="body"):
        return self.inner.width(s, size, role)

    def line(self, *a, **k):
        self.inner.line(*a, **k)

    def rect(self, *a, **k):
        self.inner.rect(*a, **k)

    def new_page(self):
        self.inner.new_page()
        self.records.append([])

    def close(self):
        return self.inner.close()


def draw_scan_page(p: Any, r, page_no: int, total: int) -> None:
    """A synthetic business record page (Latin text, table, signature line)."""
    W, H = p.page_w, p.page_h
    L = 64.0
    p.text(L, H - 64, "FICTITIOUS SAMPLE CO. - SCANNED RECORD", 15, "title")
    p.text(L, H - 82, f"Ref. FX-{2026_0000 + page_no * 17:09d}   Date 2026-01-{page_no % 28 + 1:02d}"
                      "   (synthetic test document)", 9)
    p.line(L, H - 90, W - L, H - 90, lw=1.0)
    y = H - 112
    for _ in range(3):
        for line in wrap(latin_paragraph(r, 2, 4), lambda t: p.width(t, 10.5), W - 2 * L):
            p.text(L, y, line, 10.5)
            y -= 14
        y -= 8
    # table
    cols = [60, 220, 90, 97]
    rows = [["No.", "Item", "Quantity", "Amount"]] + [
        [str(i + 1), " ".join(latin_sentence(r).split()[:3]).rstrip(".;:?!"),
         str(r.randint(1, 40)), f"{r.uniform(10, 9999):.2f}"] for i in range(8)]
    for row in rows:
        x = L
        p.rect(L, y - 18, sum(cols), 18, lw=0.8)
        for i, cell in enumerate(row):
            if i:
                p.line(x, y, x, y - 18, lw=0.8)
            p.text(x + 4, y - 13, cell, 9.5)
            x += cols[i]
        y -= 18
    y -= 18
    while y > 170:
        for line in wrap(latin_paragraph(r, 2, 4), lambda t: p.width(t, 10.5), W - 2 * L):
            if y <= 150:
                break
            p.text(L, y, line, 10.5)
            y -= 14
        y -= 8
    p.line(W - L - 180, 120, W - L, 120, lw=0.8)
    p.text(W - L - 180, 106, "Authorized signature (fictitious)", 8)
    p.text(W / 2 - 30, 50, f"Page {page_no} of {total}", 9)


def render_raster_pages(layout: Callable[[Any], None], fonts: dict[str, tuple[str, int]],
                        encode: Callable[[Image.Image, int], dict[str, Any]], *, dpi: int,
                        mode: str = "L", paper: Any = 255, record: bool = False
                        ) -> tuple[list[dict[str, Any]], list]:
    pages: list[dict[str, Any]] = []

    def on_page(img: Image.Image, idx: int) -> None:
        pages.append(encode(img, idx))

    raster = RasterPainter(fonts, on_page, dpi=dpi, mode=mode, paper=paper)
    painter: Any = RecordingPainter(raster) if record else raster
    layout(painter)
    painter.close()
    return pages, (painter.records if record else [])


def _jpeg_encoder(r, quality: int, **effects) -> Callable[[Image.Image, int], dict[str, Any]]:
    def enc(img: Image.Image, idx: int) -> dict[str, Any]:
        img = scan_effects(img, r, **effects)
        return {"data": jpeg(img, quality), "w": img.size[0], "h": img.size[1],
                "filter": "DCTDecode", "cs": "DeviceGray" if img.mode == "L" else "DeviceRGB"}
    return enc


def _g4_encoder(r) -> Callable[[Image.Image, int], dict[str, Any]]:
    def enc(img: Image.Image, idx: int) -> dict[str, Any]:
        img = scan_effects(img, r, noise_amp=10, blur=0.45, paper=250, speckles=250)
        bw = img.point(lambda v: 255 if v > 150 else 0).convert("1", dither=Image.Dither.NONE)
        w, h = bw.size
        return {"data": g4_encode(bw), "w": w, "h": h, "filter": "CCITTFaxDecode", "bpc": 1,
                "cs": "DeviceGray",
                "decode_parms": {"K": -1, "Columns": w, "Rows": h, "BlackIs1": False}}
    return enc


def scanned_gray_jpeg(out: Path, ctx: Ctx, pages: int = 20, ocr: bool = False,
                      key: str = "scan-gray") -> dict:
    r = rng(key)

    def layout(p):
        for i in range(pages):
            if i:
                p.new_page()
            draw_scan_page(p, r, i + 1, pages)

    imgs, records = render_raster_pages(layout, {"body": PILLOW_FONT, "title": PILLOW_FONT},
                                        _jpeg_encoder(rng(key + "-fx"), 72), dpi=300,
                                        record=ocr)
    if ocr:
        for page, rec in zip(imgs, records):
            page["ocr"] = rec
    n = write_scanned_pdf(out, imgs, title=f"FastPDF fixture: scanned {pages} pages",
                          id_seed=key)
    return {"pages": n}


def scanned_ccitt(out: Path, ctx: Ctx, pages: int = 10) -> dict:
    r = rng("scan-ccitt")

    def layout(p):
        for i in range(pages):
            if i:
                p.new_page()
            draw_scan_page(p, r, i + 1, pages)

    imgs, _ = render_raster_pages(layout, {"body": PILLOW_FONT, "title": PILLOW_FONT},
                                  _g4_encoder(rng("scan-ccitt-fx")), dpi=300)
    n = write_scanned_pdf(out, imgs, title="FastPDF fixture: bilevel CCITT G4 scan",
                          id_seed="scan-ccitt")
    return {"pages": n}


# --------------------------------------------------------------------------
# image-heavy
# --------------------------------------------------------------------------
def images_photos(out: Path, ctx: Ctx) -> dict:
    from reportlab.lib.utils import ImageReader

    r = rng("img-photos")
    c = rl_canvas(out, "FastPDF fixture: large RGB JPEG photos")
    W, H = A4
    for i in range(6):
        data = jpeg(synth_photo(r, 3000, 2000), 88)
        dw = W - 60
        c.drawImage(ImageReader(io.BytesIO(data)), 30, H - 60 - dw * 2 / 3, width=dw,
                    height=dw * 2 / 3)
        c.setFont("Helvetica", 10)
        c.drawString(30, 60, f"Photo {i + 1}/6: synthetic 3000x2000 RGB JPEG (q88), "
                             f"{len(data) // 1024} KiB, DCTDecode passthrough")
        c.showPage()
    c.save()
    return {"pages": 6}


def images_many_small(out: Path, ctx: Ctx) -> dict:
    from reportlab.lib.utils import ImageReader

    r = rng("img-many-small")
    c = rl_canvas(out, "FastPDF fixture: 800 small distinct images")
    W, H = A4
    for page in range(2):
        c.setFont("Helvetica-Bold", 12)
        c.drawString(40, H - 40, f"400 distinct 64x64 JPEG thumbnails (page {page + 1}/2)")
        for row in range(20):
            for col in range(20):
                im = Image.new("RGB", (64, 64), tuple(r.randint(0, 255) for _ in range(3)))
                d = ImageDraw.Draw(im)
                d.ellipse([r.randint(0, 20), r.randint(0, 20), r.randint(40, 63), r.randint(40, 63)],
                          fill=tuple(r.randint(0, 255) for _ in range(3)))
                d.text((4, 48), f"{page * 400 + row * 20 + col}", fill=(0, 0, 0))
                c.drawImage(ImageReader(io.BytesIO(jpeg(im, 80))), 40 + col * 26,
                            H - 80 - row * 36, width=24, height=24)
        c.showPage()
    c.save()
    return {"pages": 2}


def _png_image(pdf: RawPdf, img: Image.Image, cs: Any, *, extra: dict | None = None) -> Ref:
    parts = png_parts(img)
    colors = {"L": 1, "RGB": 3, "P": 1, "1": 1, "I;16": 1}[img.mode]
    bpc = parts["bits"]
    d: dict[str, Any] = {"Type": "XObject", "Subtype": "Image", "Width": img.size[0],
                         "Height": img.size[1], "BitsPerComponent": bpc, "Filter": "FlateDecode",
                         "DecodeParms": {"Predictor": 15, "Colors": colors,
                                         "BitsPerComponent": bpc, "Columns": img.size[0]}}
    if cs is not None:
        d["ColorSpace"] = cs
    if extra:
        d.update(extra)
    return pdf.stream(parts["idat"], d, compress=False)


def images_formats(out: Path, ctx: Ctx) -> dict:
    """PNG-predicted Flate images: RGB, SMask alpha, 16-bit, indexed, masks, ICC."""
    r = rng("img-formats")
    W, H = A4
    with open(out, "wb") as fp:
        pdf = RawPdf(fp, "1.7")
        font = pdf.obj(std_font("Helvetica"))
        pages_ref = pdf.alloc()
        kids: list[Ref] = []

        def page(xobjs: dict[str, Ref], draws: list[tuple[str, float, float, float, float, str]],
                 pre: str = "") -> None:
            cs = Content()
            if pre:
                cs(pre)
            for name, x, y, w, h, caption in draws:
                cs(f"q {num(w)} 0 0 {num(h)} {num(x)} {num(y)} cm /{name} Do Q")
                cs(f"BT /F1 8 Tf {num(x)} {num(y - 11)} Td {pdf_lit(caption.encode())} Tj ET")
            content = pdf.stream(cs.data())
            kids.append(pdf.obj({"Type": "Page", "Parent": pages_ref, "MediaBox": [0, 0, W, H],
                                 "Resources": {"XObject": xobjs, "Font": {"F1": font}},
                                 "Contents": content}))

        # 1: large RGB chart (Flate + PNG predictor)
        chart = _png_image(pdf, synth_chart(r, 2400, 1600), "DeviceRGB")
        page({"I1": chart}, [("I1", 40, 360, 515, 343, "2400x1600 RGB, FlateDecode + PNG predictor 15")])

        # 2: RGBA via SMask over a checkerboard
        photo = synth_photo(r, 1200, 900)
        alpha = Image.radial_gradient("L").resize((1200, 900)).point(lambda v: 255 - v)
        smask = _png_image(pdf, alpha, "DeviceGray")
        rgba = _png_image(pdf, photo, "DeviceRGB", extra={"SMask": smask})
        checker = "\n".join(f"{0.75 if (i + j) % 2 else 0.95} g {40 + i * 40} {300 + j * 40} 40 40 re f"
                            for i in range(13) for j in range(10))
        page({"I2": rgba}, [("I2", 40, 300, 515, 386, "RGB + /SMask (radial alpha) over checkerboard")],
             pre=checker)

        # 3: 16-bit gray, 8-bit and 4-bit indexed
        w16, h16 = 1200, 800
        import array
        vals = array.array("H", (((x * 65535) // (w16 - 1)) ^ ((y * 977) & 0x00FF)
                                 for y in range(h16) for x in range(w16)))
        g16 = _png_image(pdf, Image.frombytes("I;16", (w16, h16), vals.tobytes()), "DeviceGray")
        ph = synth_photo(r, 800, 600)
        idx8 = ph.quantize(colors=64, method=Image.Quantize.MEDIANCUT, dither=Image.Dither.NONE)
        p8 = png_parts(idx8)
        i8 = _png_image(pdf, idx8, ["Indexed", "DeviceRGB", len(p8["palette"]) // 3 - 1,
                                    Hex(p8["palette"])])
        idx4 = ph.quantize(colors=16, method=Image.Quantize.MEDIANCUT, dither=Image.Dither.NONE)
        p4 = png_parts(idx4)
        i4 = _png_image(pdf, idx4, ["Indexed", "DeviceRGB", len(p4["palette"]) // 3 - 1,
                                    Hex(p4["palette"])])
        page({"G16": g16, "P8": i8, "P4": i4},
             [("G16", 40, 520, 515, 270, "1200x800 DeviceGray, 16 bits per component"),
              ("P8", 40, 250, 250, 188, f"Indexed 8-bit ({p8['bits']} bpc), 64 colours"),
              ("P4", 305, 250, 250, 188, f"Indexed {p4['bits']}-bit, 16 colours")])

        # 4: stencil mask, colour-key mask, Decode inversion, Interpolate
        stencil_img = Image.new("1", (600, 400), 1)
        d = ImageDraw.Draw(stencil_img)
        for _ in range(40):
            x, y = r.randrange(600), r.randrange(400)
            d.ellipse([x, y, x + r.randint(10, 80), y + r.randint(10, 80)], fill=0)
        stencil = _png_image(pdf, stencil_img, None, extra={"ImageMask": True})
        key_img = Image.new("RGB", (400, 300), (255, 0, 255))
        d = ImageDraw.Draw(key_img)
        for _ in range(25):
            x, y = r.randrange(400), r.randrange(300)
            d.rectangle([x, y, x + 60, y + 40], fill=tuple(r.randint(0, 200) for _ in range(3)))
        keyed = _png_image(pdf, key_img, "DeviceRGB", extra={"Mask": [255, 255, 0, 0, 255, 255]})
        inv = _png_image(pdf, Image.linear_gradient("L").resize((300, 200)), "DeviceGray",
                         extra={"Decode": [1, 0]})
        tiny = Image.new("RGB", (16, 12))
        tiny.putdata([tuple(r.randint(0, 255) for _ in range(3)) for _ in range(16 * 12)])
        t_on = _png_image(pdf, tiny, "DeviceRGB", extra={"Interpolate": True})
        t_off = _png_image(pdf, tiny, "DeviceRGB")
        page({"S": stencil, "K": keyed, "D": inv, "T1": t_on, "T0": t_off},
             [("K", 40, 560, 240, 180, "Colour-key /Mask (magenta transparent)"),
              ("D", 315, 560, 240, 160, "/Decode [1 0] (inverted gray)"),
              ("T1", 40, 300, 240, 180, "16x12 upscaled, /Interpolate true"),
              ("T0", 315, 300, 240, 180, "16x12 upscaled, no interpolation")],
             pre="0.85 0.1 0.1 rg q 515 0 0 120 40 120 cm /S Do Q "
                 "BT /F1 8 Tf 40 109 Td (1-bit /ImageMask stencil painted with red fill) Tj ET")

        # 5: ICCBased colour space (image + vector fills)
        icc = pdf.stream(srgb_icc_profile(), {"N": 3, "Alternate": "DeviceRGB"})
        icc_cs = ["ICCBased", icc]
        icc_img = _png_image(pdf, synth_photo(r, 900, 600), icc_cs)
        cs = Content()
        cs("/CS0 cs")
        for i in range(10):
            cs(f"{i / 9:.3f} {1 - i / 9:.3f} 0.5 sc {40 + i * 51} 120 48 48 re f")
        cs("q 515 0 0 343 40 300 cm /I Do Q")
        cs("BT /F1 8 Tf 40 289 Td (ICCBased RGB image \\(sRGB profile\\) and ICCBased fills) Tj ET")
        content = pdf.stream(cs.data())
        kids.append(pdf.obj({"Type": "Page", "Parent": pages_ref, "MediaBox": [0, 0, W, H],
                             "Resources": {"XObject": {"I": icc_img}, "Font": {"F1": font},
                                           "ColorSpace": {"CS0": icc_cs}},
                             "Contents": content}))
        simple_document(pdf, kids, pages_ref, title="FastPDF fixture: image formats",
                        id_seed="img-formats")
    return {"pages": len(kids)}


def images_jpeg_variants(out: Path, ctx: Ctx) -> dict:
    """24 MP JPEG, Adobe CMYK, progressive, grayscale, 4:4:4, restart markers."""
    r = rng("img-jpeg-variants")
    W, H = A4
    with open(out, "wb") as fp:
        pdf = RawPdf(fp, "1.7")
        font = pdf.obj(std_font("Helvetica"))
        pages_ref = pdf.alloc()
        kids: list[Ref] = []

        def xobj(data: bytes, w: int, h: int, cs: str, extra: dict | None = None) -> Ref:
            d = {"Type": "XObject", "Subtype": "Image", "Width": w, "Height": h,
                 "ColorSpace": cs, "BitsPerComponent": 8, "Filter": "DCTDecode"}
            d.update(extra or {})
            return pdf.stream(data, d, compress=False)

        def page(xobjs: dict[str, Ref], draws: list[tuple[str, float, float, float, float, str]]):
            cs = Content()
            for name, x, y, w, h, caption in draws:
                cs(f"q {num(w)} 0 0 {num(h)} {num(x)} {num(y)} cm /{name} Do Q")
                cs(f"BT /F1 8 Tf {num(x)} {num(y - 11)} Td {pdf_lit(caption.encode())} Tj ET")
            content = pdf.stream(cs.data())
            kids.append(pdf.obj({"Type": "Page", "Parent": pages_ref, "MediaBox": [0, 0, W, H],
                                 "Resources": {"XObject": xobjs, "Font": {"F1": font}},
                                 "Contents": content}))

        big = synth_photo(r, 6000, 4000)
        big_data = jpeg(big, 80)
        del big
        page({"B": xobj(big_data, 6000, 4000, "DeviceRGB")},
             [("B", 30, 300, 535, 357, f"6000x4000 (24 MP) RGB JPEG q80, {len(big_data) // 1024} KiB")])

        src = synth_photo(r, 1600, 1200)
        cmyk = jpeg(src.convert("CMYK"), 85)
        prog = jpeg(src, 85, progressive=True)
        gray = jpeg(src.convert("L"), 85)
        full = jpeg(src, 92, subsampling=0)
        rst = jpeg(src, 85, restart_marker_rows=1)
        page({"C": xobj(cmyk, 1600, 1200, "DeviceCMYK", {"Decode": [1, 0, 1, 0, 1, 0, 1, 0]}),
              "P": xobj(prog, 1600, 1200, "DeviceRGB"),
              "G": xobj(gray, 1600, 1200, "DeviceGray"),
              "F": xobj(full, 1600, 1200, "DeviceRGB"),
              "R": xobj(rst, 1600, 1200, "DeviceRGB")},
             [("C", 30, 600, 260, 195, "Adobe CMYK JPEG (inverted) + /Decode [1 0 ...]"),
              ("P", 305, 600, 260, 195, "Progressive JPEG"),
              ("G", 30, 360, 260, 195, "Grayscale JPEG"),
              ("F", 305, 360, 260, 195, "4:4:4 (no chroma subsampling) q92"),
              ("R", 30, 120, 260, 195, "Restart markers every MCU row")])
        simple_document(pdf, kids, pages_ref, title="FastPDF fixture: JPEG variants",
                        id_seed="img-jpeg-variants")
    return {"pages": len(kids)}


# --------------------------------------------------------------------------
# large-file (full profile only): streamed to disk, ~2000 pages of JPEGs
# --------------------------------------------------------------------------
def _calibrated_pool(target_bytes: float, count: int) -> list[tuple[bytes, int, int]]:
    """Synthetic JPEGs whose average size approximates ``target_bytes``."""
    cal = rng("large-file-calibration")
    w = 800
    for _ in range(4):
        h = w * 3 // 4
        size = len(jpeg(synth_photo(cal, w, h), 85))
        w = max(64, int(w * math.sqrt(target_bytes / size)))
    r = rng("large-file-pool")
    pool = []
    for i in range(count):
        ww = max(64, int(w * (0.9 + 0.2 * (i % 5) / 4)))
        hh = ww * 3 // 4
        pool.append((jpeg(synth_photo(r, ww, hh), 85), ww, hh))
    avg = sum(len(p[0]) for p in pool) / len(pool)
    scale = math.sqrt(target_bytes / avg)
    if abs(scale - 1) > 0.08:  # one correction pass keeps the total close to target
        r = rng("large-file-pool-2")
        pool = []
        for i in range(count):
            ww = max(64, int(w * scale * (0.9 + 0.2 * (i % 5) / 4)))
            hh = ww * 3 // 4
            pool.append((jpeg(synth_photo(r, ww, hh), 85), ww, hh))
    return pool


def large_file(out: Path, ctx: Ctx, pages: int = 2000) -> dict:
    target = ctx.large_file_mb * 1024 * 1024
    per_page = max(4096.0, (target - pages * 900) / pages)
    pool = _calibrated_pool(per_page, 40)
    W, H = A4
    with open(out, "wb", buffering=1 << 20) as fp:
        pdf = RawPdf(fp, "1.7")
        font = pdf.obj(std_font("Helvetica-Bold"))
        pages_ref = pdf.alloc()
        kids: list[Ref] = []
        for i in range(pages):
            data, w, h = pool[(i * 7) % len(pool)]
            img = pdf.stream(data, {"Type": "XObject", "Subtype": "Image", "Width": w, "Height": h,
                                    "ColorSpace": "DeviceRGB", "BitsPerComponent": 8,
                                    "Filter": "DCTDecode"}, compress=False)
            ih = (W - 40) * h / w
            ops = (f"q {num(W - 40)} 0 0 {num(ih)} 20 {num(H - 60 - ih)} cm /Im0 Do Q\n"
                   f"BT /F1 28 Tf 20 {num(H - 45)} Td (Page {i + 1} / {pages}) Tj ET\n"
                   f"BT /F1 9 Tf 20 30 Td (large-file fixture, image {w}x{h}, "
                   f"{len(data)} bytes) Tj ET")
            content = pdf.stream(ops.encode("ascii"))
            kids.append(pdf.obj({"Type": "Page", "Parent": pages_ref, "MediaBox": [0, 0, W, H],
                                 "Resources": {"XObject": {"Im0": img}, "Font": {"F1": font}},
                                 "Contents": content}))
        simple_document(pdf, kids, pages_ref,
                        title=f"FastPDF fixture: large file (~{ctx.large_file_mb} MB, {pages} pages)",
                        id_seed=f"large-file-{ctx.large_file_mb}")
    return {"pages": pages, "features": [f"target-{ctx.large_file_mb}MB"]}


def jobs() -> list[Job]:
    return [
        Job("scanned/scan-gray-jpeg-20p.pdf", scanned_gray_jpeg, "open_ok",
            "模擬掃描：20 頁、每頁一張 300 dpi 灰階 JPEG（2480x3508，含雜訊、輕微旋轉、模糊、灰塵、掃描器陰影），無文字層。",
            pages=20, features=("DCTDecode", "DeviceGray", "300dpi", "one-image-per-page"),
            kwargs={"pages": 20}, cost=8),
        Job("scanned/scan-gray-ocr-layer-5p.pdf", scanned_gray_jpeg, "open_ok",
            "模擬 OCR 後的可搜尋掃描檔：灰階 JPEG 影像＋隱形文字層（Tr 3、Helvetica、Tz 對齊寬度）。",
            pages=5, features=("DCTDecode", "invisible-text", "Tr3", "searchable-scan"),
            kwargs={"pages": 5, "ocr": True, "key": "scan-ocr"}, cost=2.5),
        Job("scanned/scan-bilevel-ccitt-g4-10p.pdf", scanned_ccitt, "open_ok",
            "黑白掃描：10 頁 300 dpi 1-bit CCITT Group 4（CCITTFaxDecode，K -1，BlackIs1 false），公文掃描常見格式。",
            pages=10, features=("CCITTFaxDecode", "G4", "1-bit"), cost=4.5),
        Job("image-heavy/photos-rgb-jpeg-6p.pdf", images_photos, "open_ok",
            "6 頁、每頁一張 3000x2000 合成照片（RGB JPEG q88，DCT passthrough）。",
            pages=6, features=("DCTDecode", "DeviceRGB", "large-images"), producer="reportlab",
            cost=5),
        Job("image-heavy/many-small-images-800.pdf", images_many_small, "open_ok",
            "2 頁共 800 張互不相同的 64x64 JPEG 縮圖，測試大量 XObject 的 per-image overhead。",
            pages=2, features=("DCTDecode", "many-xobjects"), producer="reportlab", cost=1.5),
        Job("image-heavy/image-formats-flate.pdf", images_formats, "open_ok",
            "影像格式涵蓋：Flate+PNG predictor（RGB 2400x1600）、SMask alpha、16-bit 灰階、8／4-bit Indexed、1-bit ImageMask、color-key /Mask、/Decode 反相、/Interpolate、ICCBased。",
            pages=5, features=("FlateDecode", "PNG-predictor", "SMask", "16bpc", "Indexed",
                               "ImageMask", "ColorKeyMask", "Decode", "Interpolate", "ICCBased"),
            cost=3),
        Job("image-heavy/jpeg-variants.pdf", images_jpeg_variants, "open_ok",
            "JPEG 變體：6000x4000（24 MP）大圖、Adobe CMYK（反相＋/Decode）、progressive、灰階、4:4:4、restart markers。CMYK 顏色在各 reader 間的慣例不一，主要測解碼路徑。",
            pages=2, features=("DCTDecode", "24MP", "CMYK", "progressive", "restart-markers"),
            cost=7),
        Job("large-file/large-file-2000p.pdf", large_file, "open_ok",
            "大型檔案（僅 full profile）：約 2000 頁、每頁一張獨立 JPEG XObject，總大小約為 --large-file-mb；以串流方式寫出。",
            pages=2000, features=("large-file", "streamed", "DCTDecode"), full_only=True, cost=20),
    ]
