"""Tiny drawing abstraction so one layout can be emitted by several backends.

Backends:
  * ReportLabPainter - reportlab canvas (standard / CID / TrueType-subset fonts)
  * RawPainter       - rawpdf writer (Type0 Identity-H embedded fonts)
  * RasterPainter    - Pillow image at a given dpi (used to simulate scans)

Coordinates are PDF points with the origin at the bottom-left corner.
"""

from __future__ import annotations

from typing import Any, Callable

from .rawpdf import Content, RawPdf, Ref, info_dict, num

Color = tuple[float, float, float]
BLACK: Color = (0.0, 0.0, 0.0)
A4 = (595.2756, 841.8898)


class ReportLabPainter:
    def __init__(self, path: str, fonts: dict[str, str], *, title: str, subject: str = "",
                 pagesize: tuple[float, float] = A4):
        from reportlab.pdfgen.canvas import Canvas

        self.c = Canvas(path, pagesize=pagesize, invariant=1, pageCompression=1)
        self.c.setTitle(title)
        self.c.setSubject(subject)
        self.c.setCreator("tools/fixtures/generate.py")
        self.c.setAuthor("FastPDF fixture generator (synthetic content)")
        self.fonts = fonts
        self.page_w, self.page_h = pagesize
        self.pages = 1

    def text(self, x: float, y: float, s: str, size: float, role: str = "body",
             color: Color = BLACK) -> None:
        self.c.setFont(self.fonts[role], size)
        self.c.setFillColorRGB(*color)
        self.c.drawString(x, y, s)

    def width(self, s: str, size: float, role: str = "body") -> float:
        from reportlab.pdfbase.pdfmetrics import stringWidth
        return stringWidth(s, self.fonts[role], size)

    def line(self, x0: float, y0: float, x1: float, y1: float, lw: float = 0.6,
             color: Color = BLACK) -> None:
        self.c.setLineWidth(lw)
        self.c.setStrokeColorRGB(*color)
        self.c.line(x0, y0, x1, y1)

    def rect(self, x: float, y: float, w: float, h: float, lw: float = 0.6,
             stroke: Color | None = BLACK, fill: Color | None = None) -> None:
        if fill is not None:
            self.c.setFillColorRGB(*fill)
        if stroke is not None:
            self.c.setStrokeColorRGB(*stroke)
            self.c.setLineWidth(lw)
        self.c.rect(x, y, w, h, stroke=1 if stroke is not None else 0,
                    fill=1 if fill is not None else 0)

    def new_page(self) -> None:
        self.c.showPage()
        self.pages += 1

    def close(self) -> int:
        self.c.showPage()
        self.c.save()
        return self.pages


class RawPainter:
    """Writes pages through RawPdf; fonts are Type0Font / StdFont objects."""

    def __init__(self, pdf: RawPdf, fonts: dict[str, Any], *, title: str, id_seed: str,
                 pagesize: tuple[float, float] = A4):
        self.pdf = pdf
        self.fonts = fonts
        self.title = title
        self.id_seed = id_seed
        self.page_w, self.page_h = pagesize
        self.pages_ref: Ref = pdf.alloc()
        self.page_refs: list[Ref] = []
        self.cs = Content()

    def _rgb(self, color: Color, op: str) -> str:
        return f"{num(color[0])} {num(color[1])} {num(color[2])} {op}"

    def text(self, x: float, y: float, s: str, size: float, role: str = "body",
             color: Color = BLACK) -> None:
        f = self.fonts[role]
        data = f.encode(s).hex().upper()
        self.cs(f"BT /{f.res_name} {num(size)} Tf {self._rgb(color, 'rg')} "
                f"{num(x)} {num(y)} Td <{data}> Tj ET")

    def width(self, s: str, size: float, role: str = "body") -> float:
        return self.fonts[role].width(s, size)

    def line(self, x0: float, y0: float, x1: float, y1: float, lw: float = 0.6,
             color: Color = BLACK) -> None:
        self.cs(f"{num(lw)} w {self._rgb(color, 'RG')} {num(x0)} {num(y0)} m "
                f"{num(x1)} {num(y1)} l S")

    def rect(self, x: float, y: float, w: float, h: float, lw: float = 0.6,
             stroke: Color | None = BLACK, fill: Color | None = None) -> None:
        ops = []
        if fill is not None:
            ops.append(self._rgb(fill, "rg"))
        if stroke is not None:
            ops.append(f"{num(lw)} w {self._rgb(stroke, 'RG')}")
        ops.append(f"{num(x)} {num(y)} {num(w)} {num(h)} re")
        ops.append("B" if (fill is not None and stroke is not None) else ("f" if fill is not None else "S"))
        self.cs(" ".join(ops))

    def _flush(self) -> None:
        content = self.pdf.stream(self.cs.data())
        fonts = {f.res_name: f.ref for f in self.fonts.values()}
        ref = self.pdf.obj({
            "Type": "Page", "Parent": self.pages_ref,
            "MediaBox": [0, 0, round(self.page_w, 4), round(self.page_h, 4)],
            "Resources": {"Font": fonts, "ProcSet": ["PDF", "Text"]},
            "Contents": content,
        })
        self.page_refs.append(ref)
        self.cs = Content()

    def new_page(self) -> None:
        self._flush()

    def close(self) -> int:
        self._flush()
        seen = set()
        for f in self.fonts.values():
            if id(f) not in seen:
                seen.add(id(f))
                f.finish(self.pdf)
        self.pdf.obj({"Type": "Pages", "Kids": self.page_refs, "Count": len(self.page_refs)},
                     self.pages_ref)
        root = self.pdf.obj({"Type": "Catalog", "Pages": self.pages_ref})
        info = self.pdf.obj(info_dict(self.title))
        self.pdf.finish(root, info, id_seed=self.id_seed)
        return len(self.page_refs)


class RasterPainter:
    """Draws into a Pillow image; ``on_page(image, index)`` receives each page."""

    def __init__(self, fonts: dict[str, tuple[str, int]], on_page: Callable[[Any, int], None], *,
                 dpi: int = 300, mode: str = "L", paper: Any = 255,
                 pagesize: tuple[float, float] = A4):
        self.fonts = fonts
        self.on_page = on_page
        self.dpi = dpi
        self.mode = mode
        self.paper = paper
        self.scale = dpi / 72.0
        self.page_w, self.page_h = pagesize
        self.px = (round(self.page_w * self.scale), round(self.page_h * self.scale))
        self._font_cache: dict[tuple[str, int], Any] = {}
        self.index = 0
        self._new_image()

    def _new_image(self) -> None:
        from PIL import Image, ImageDraw

        self.img = Image.new(self.mode, self.px, self.paper)
        self.draw = ImageDraw.Draw(self.img)

    def _font(self, role: str, size: float):
        from PIL import ImageFont

        px = max(1, round(size * self.scale))
        key = (role, px)
        f = self._font_cache.get(key)
        if f is None:
            path, index = self.fonts[role]
            if path == "<pillow-default>":
                f = ImageFont.load_default(size=px)
            else:
                f = ImageFont.truetype(path, px, index=index,
                                       layout_engine=ImageFont.Layout.BASIC)
            self._font_cache[key] = f
        return f

    def _c(self, color: Color):
        if self.mode == "L":
            return round(255 * (0.299 * color[0] + 0.587 * color[1] + 0.114 * color[2]))
        return tuple(round(255 * c) for c in color)

    def _pt(self, x: float, y: float) -> tuple[float, float]:
        return x * self.scale, (self.page_h - y) * self.scale

    def text(self, x: float, y: float, s: str, size: float, role: str = "body",
             color: Color = BLACK) -> None:
        self.draw.text(self._pt(x, y), s, font=self._font(role, size), fill=self._c(color),
                       anchor="ls")

    def width(self, s: str, size: float, role: str = "body") -> float:
        return self._font(role, size).getlength(s) / self.scale

    def line(self, x0: float, y0: float, x1: float, y1: float, lw: float = 0.6,
             color: Color = BLACK) -> None:
        self.draw.line([self._pt(x0, y0), self._pt(x1, y1)], fill=self._c(color),
                       width=max(1, round(lw * self.scale)))

    def rect(self, x: float, y: float, w: float, h: float, lw: float = 0.6,
             stroke: Color | None = BLACK, fill: Color | None = None) -> None:
        x0, y0 = self._pt(x, y + h)
        x1, y1 = self._pt(x + w, y)
        self.draw.rectangle([x0, y0, x1, y1],
                            fill=self._c(fill) if fill is not None else None,
                            outline=self._c(stroke) if stroke is not None else None,
                            width=max(1, round(lw * self.scale)) if stroke is not None else 0)

    def new_page(self) -> None:
        self.on_page(self.img, self.index)
        self.index += 1
        self._new_image()

    def close(self) -> int:
        self.on_page(self.img, self.index)
        return self.index + 1
