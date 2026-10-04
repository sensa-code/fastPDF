"""System font discovery and Type0 / CIDFontType2 (Identity-H) embedding.

System fonts are only *read* at generation time; the subset programs end up
inside files under fixtures/generated/, which is never committed.
"""

from __future__ import annotations

import hashlib
import io
import os
import sys
from pathlib import Path
from typing import Any

from .rawpdf import RawPdf, Ref, Hex

# role -> list of (file name, accepted family / full names); first match wins.
FONT_CANDIDATES: dict[str, list[tuple[str, tuple[str, ...]]]] = {
    # 標楷體 — the customary typeface of Taiwanese government documents (公文)
    "tc_kai": [("kaiu.ttf", ("DFKai-SB",)), ("ukai.ttc", ("AR PL UKai TW",))],
    # 新細明體 — ubiquitous body face in Taiwanese office documents
    "tc_ming": [("mingliu.ttc", ("PMingLiU", "MingLiU")), ("uming.ttc", ("AR PL UMing TW",))],
    # 微軟正黑體
    "tc_sans": [("msjh.ttc", ("Microsoft JhengHei",)), ("mingliu.ttc", ("PMingLiU",))],
    # Japanese
    "ja_gothic": [("YuGothM.ttc", ("Yu Gothic Medium", "Yu Gothic")),
                  ("msgothic.ttc", ("MS Gothic",))],
    "ja_msgothic": [("msgothic.ttc", ("MS Gothic",))],
}


def _font_dirs() -> list[Path]:
    dirs: list[Path] = []
    if sys.platform == "win32":
        windir = os.environ.get("WINDIR", r"C:\Windows")
        dirs.append(Path(windir) / "Fonts")
        local = os.environ.get("LOCALAPPDATA")
        if local:
            dirs.append(Path(local) / "Microsoft" / "Windows" / "Fonts")
    else:
        dirs += [Path("/usr/share/fonts"), Path("/usr/local/share/fonts"),
                 Path.home() / ".fonts", Path.home() / ".local/share/fonts",
                 Path("/Library/Fonts"), Path("/System/Library/Fonts")]
    return [d for d in dirs if d.is_dir()]


def _find_file(name: str) -> Path | None:
    for d in _font_dirs():
        p = d / name
        if p.is_file():
            return p
        if sys.platform != "win32":
            for root, _dirs, files in os.walk(d):
                if name in files:
                    return Path(root) / name
    return None


def _face_names(tt) -> set[str]:
    names = set()
    for rec in tt["name"].names:
        if rec.nameID in (1, 4, 16):
            try:
                names.add(rec.toUnicode())
            except Exception:
                pass
    return names


def discover_fonts() -> tuple[dict[str, dict[str, Any]], list[dict[str, Any]]]:
    """Return (role -> font info, report rows) for the manifest."""
    from fontTools.ttLib import TTCollection, TTFont

    found: dict[str, dict[str, Any]] = {}
    report: list[dict[str, Any]] = []
    sha_cache: dict[Path, tuple[int, str]] = {}
    for role, candidates in FONT_CANDIDATES.items():
        chosen = None
        for fname, families in candidates:
            path = _find_file(fname)
            if not path:
                continue
            faces = (TTCollection(str(path), lazy=True).fonts
                     if fname.lower().endswith(".ttc") else [TTFont(str(path), lazy=True)])
            # families are in priority order (e.g. PMingLiU before MingLiU in mingliu.ttc)
            ordered = [(idx, tt) for fam in families
                       for idx, tt in enumerate(faces) if fam in _face_names(tt)]
            for idx, tt in ordered:
                if "glyf" not in tt:
                    continue  # reportlab and our Type0 writer need TrueType outlines
                fs_type = tt["OS/2"].fsType if "OS/2" in tt else 0
                if fs_type == 0x0002 or (fs_type & 0x0300):
                    continue  # embedding/subsetting not permitted by the font
                if path not in sha_cache:
                    data = path.read_bytes()
                    sha_cache[path] = (len(data), hashlib.sha256(data).hexdigest())
                size, digest = sha_cache[path]
                chosen = {
                    "role": role,
                    "path": str(path),
                    "file": path.name,
                    "index": idx,
                    "family": tt["name"].getDebugName(1),
                    "postscript_name": tt["name"].getDebugName(6),
                    "fs_type": fs_type,
                    "bytes": size,
                    "sha256": digest,
                }
                break
            if chosen:
                break
        if chosen:
            found[role] = chosen
            report.append({k: v for k, v in chosen.items() if k != "path"} | {"status": "found"})
        else:
            report.append({"role": role, "status": "missing",
                           "candidates": [c[0] for c in candidates]})
    return found, report


# --------------------------------------------------------------------------
# Type0 / CIDFontType2 / Identity-H embedding (what Word, Chrome, LibreOffice emit)
# --------------------------------------------------------------------------
def _subset_tag(gids: list[int]) -> str:
    digest = hashlib.sha256(",".join(map(str, gids)).encode()).digest()
    return "".join(chr(ord("A") + b % 26) for b in digest[:6])


def tounicode_cmap(mapping: dict[int, str]) -> bytes:
    lines = [
        "/CIDInit /ProcSet findresource begin",
        "12 dict begin",
        "begincmap",
        "/CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def",
        "/CMapName /Adobe-Identity-UCS def",
        "/CMapType 2 def",
        "1 begincodespacerange",
        "<0000> <FFFF>",
        "endcodespacerange",
    ]
    items = sorted(mapping.items())
    for i in range(0, len(items), 100):
        chunk = items[i:i + 100]
        lines.append(f"{len(chunk)} beginbfchar")
        for gid, text in chunk:
            lines.append(f"<{gid:04X}> <{text.encode('utf-16-be').hex().upper()}>")
        lines.append("endbfchar")
    lines += ["endcmap", "CMapName currentdict /CMap defineresource pop", "end", "end"]
    return ("\n".join(lines) + "\n").encode("ascii")


class Type0Font:
    """Embeds a TrueType font as Type0 + CIDFontType2 with Identity-H encoding.

    Glyph IDs are retained in the subset (fontTools ``retain_gids``), so the
    content streams can be written before the subset is built.
    """

    def __init__(self, pdf: RawPdf, info: dict[str, Any], res_name: str):
        from fontTools.ttLib import TTFont

        self.info = info
        self.res_name = res_name
        self.ref: Ref = pdf.alloc()
        tt = TTFont(info["path"], fontNumber=info["index"], lazy=True,
                    recalcTimestamp=False, recalcBBoxes=False)
        self.cmap = tt.getBestCmap()
        self.order = tt.getGlyphOrder()
        self.gid_of = {name: i for i, name in enumerate(self.order)}
        self.metrics = tt["hmtx"].metrics
        self.upem = tt["head"].unitsPerEm
        self.ps_name = (tt["name"].getDebugName(6) or "EmbeddedFont").replace(" ", "")
        self.used: dict[int, str] = {}
        self._adv_cache: dict[str, float] = {}

    def gid(self, ch: str) -> int:
        name = self.cmap.get(ord(ch))
        return self.gid_of.get(name, 0) if name else 0

    def encode(self, text: str) -> bytes:
        out = bytearray()
        for ch in text:
            g = self.gid(ch)
            if g:
                self.used.setdefault(g, ch)
            out += g.to_bytes(2, "big")
        return bytes(out)

    def _adv(self, ch: str) -> float:
        a = self._adv_cache.get(ch)
        if a is None:
            a = self.metrics[self.order[self.gid(ch)]][0]
            self._adv_cache[ch] = a
        return a

    def width(self, text: str, size: float) -> float:
        return sum(self._adv(ch) for ch in text) * size / self.upem

    def finish(self, pdf: RawPdf) -> None:
        import logging

        from fontTools import subset
        from fontTools.ttLib import TTFont

        logging.getLogger("fontTools").setLevel(logging.ERROR)
        gids = sorted(set(self.used) | {0})
        opts = subset.Options()
        opts.retain_gids = True
        opts.notdef_outline = True
        opts.layout_features = []
        opts.recalc_bounds = False
        opts.recalc_timestamp = False
        font = TTFont(self.info["path"], fontNumber=self.info["index"],
                      recalcTimestamp=False, recalcBBoxes=False)
        sub = subset.Subsetter(opts)
        sub.populate(gids=gids)
        sub.subset(font)
        buf = io.BytesIO()
        font.save(buf, reorderTables=True)
        data = buf.getvalue()

        base = f"{_subset_tag(gids)}+{self.ps_name}"
        scale = 1000.0 / self.upem
        head, hhea = font["head"], font["hhea"]
        os2 = font["OS/2"] if "OS/2" in font else None
        cap = getattr(os2, "sCapHeight", 0) if os2 is not None else 0
        ff = pdf.stream(data, {"Length1": len(data)})
        fd = pdf.obj({
            "Type": "FontDescriptor", "FontName": base, "Flags": 4,
            "FontBBox": [round(head.xMin * scale), round(head.yMin * scale),
                         round(head.xMax * scale), round(head.yMax * scale)],
            "ItalicAngle": 0, "Ascent": round(hhea.ascent * scale),
            "Descent": round(hhea.descent * scale),
            "CapHeight": round((cap or hhea.ascent) * scale), "StemV": 80,
            "FontFile2": ff,
        })
        widths: list[Any] = []
        for g in gids:
            widths += [g, [round(self.metrics[self.order[g]][0] * scale)]]
        cid = pdf.obj({
            "Type": "Font", "Subtype": "CIDFontType2", "BaseFont": base,
            "CIDSystemInfo": {"Registry": b"Adobe", "Ordering": b"Identity", "Supplement": 0},
            "FontDescriptor": fd, "DW": 1000, "W": widths, "CIDToGIDMap": "Identity",
        })
        tu = pdf.stream(tounicode_cmap(self.used))
        pdf.obj({"Type": "Font", "Subtype": "Type0", "BaseFont": base, "Encoding": "Identity-H",
                 "DescendantFonts": [cid], "ToUnicode": tu}, self.ref)


class StdFont:
    """Standard-14 font usable by RawPainter (WinAnsi text, AFM widths via reportlab)."""

    def __init__(self, pdf: RawPdf, base: str, res_name: str):
        self.base = base
        self.res_name = res_name
        self.ref = pdf.alloc()

    def encode(self, text: str) -> bytes:
        return text.encode("cp1252", errors="replace")

    def width(self, text: str, size: float) -> float:
        from reportlab.pdfbase.pdfmetrics import stringWidth
        return stringWidth(text, self.base, size)

    def finish(self, pdf: RawPdf) -> None:
        pdf.obj({"Type": "Font", "Subtype": "Type1", "BaseFont": self.base,
                 "Encoding": "WinAnsiEncoding"}, self.ref)


__all__ = ["discover_fonts", "Type0Font", "StdFont", "tounicode_cmap", "Hex"]
