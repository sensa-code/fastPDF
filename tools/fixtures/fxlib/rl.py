"""reportlab setup shared by all generators (determinism switches)."""

from __future__ import annotations

from pathlib import Path
from typing import Callable

from reportlab import rl_config

# Fixed timestamps and document IDs (CreationDate becomes 2000-01-01).
rl_config.invariant = 1
# Binary streams instead of ASCII85 wrappers (closer to real-world producers).
rl_config.useA85 = 0
rl_config.pageCompression = 1

from reportlab.lib.pagesizes import A4, letter  # noqa: E402
from reportlab.pdfbase.pdfmetrics import stringWidth  # noqa: E402

from .common import wrap  # noqa: E402

AUTHOR = "FastPDF fixture generator (synthetic content)"


def rl_canvas(out: Path | str, title: str, pagesize=A4, subject: str = ""):
    from reportlab.pdfgen.canvas import Canvas

    c = Canvas(str(out), pagesize=pagesize, invariant=1, pageCompression=1)
    c.setTitle(title)
    c.setAuthor(AUTHOR)
    c.setCreator("tools/fixtures/generate.py")
    if subject:
        c.setSubject(subject)
    return c


def width_fn(font: str, size: float) -> Callable[[str], float]:
    cache: dict[str, float] = {}

    def w(s: str) -> float:
        v = cache.get(s)
        if v is None:
            v = cache[s] = stringWidth(s, font, size)
        return v

    return w


def wrap_text(text: str, font: str, size: float, max_w: float) -> list[str]:
    return wrap(text, width_fn(font, size), max_w)


__all__ = ["A4", "letter", "rl_canvas", "wrap_text", "width_fn", "stringWidth"]
