"""A tiny, dependency-free, deterministic PDF writer.

It is intentionally low level: callers build dictionaries/arrays from plain
Python values and decide exactly which bytes end up in the file. This makes it
suitable for hand-built documents (Type3 fonts, CCITT images, object streams,
multi-hundred-MB streaming files) as well as for deliberately broken files.

Serialization rules (see ``ser``):
  * ``dict`` keys are always names; ``str`` values are names as well.
  * ``bytes`` values become literal strings ``(...)``; ``Hex`` becomes ``<...>``.
  * ``Raw`` is emitted verbatim (used to inject invalid syntax on purpose).
  * ``Ref`` becomes an indirect reference ``n g R``.
"""

from __future__ import annotations

import hashlib
import zlib
from dataclasses import dataclass
from typing import Any, BinaryIO, Callable, Iterable

# Fixed metadata date for every raw-written file (determinism).
FIXED_PDF_DATE = b"D:20260101000000Z"
PRODUCER = b"FastPDF fixture generator (rawpdf)"


class Name(str):
    """Explicit PDF name object (plain ``str`` values are names too)."""

    __slots__ = ()


class Raw(bytes):
    """Bytes emitted verbatim: pre-serialized or deliberately invalid tokens."""

    __slots__ = ()


class Hex(bytes):
    """Byte string serialized as a hexadecimal string ``<...>``."""

    __slots__ = ()


@dataclass(frozen=True, slots=True)
class Ref:
    num: int
    gen: int = 0


_DELIMS = frozenset(b"()<>[]{}/%#")


def name_bytes(name: str) -> bytes:
    out = bytearray(b"/")
    for b in name.encode("utf-8"):
        if b < 0x21 or b > 0x7E or b in _DELIMS:
            out += b"#%02X" % b
        else:
            out.append(b)
    return bytes(out)


def literal(data: bytes) -> bytes:
    data = data.replace(b"\\", b"\\\\").replace(b"(", b"\\(").replace(b")", b"\\)")
    data = data.replace(b"\r", b"\\r")
    return b"(" + data + b")"


def num(x: float) -> str:
    """Format a number the way PDF expects (no exponent, trimmed zeros)."""
    if isinstance(x, bool):
        raise TypeError("bool is not a PDF number")
    if isinstance(x, int):
        return str(x)
    r = round(x)
    if abs(x - r) < 1e-9:
        return str(int(r))
    s = f"{x:.4f}".rstrip("0").rstrip(".")
    return "0" if s in ("", "-0") else s


def ser(o: Any) -> bytes:
    if o is None:
        return b"null"
    if o is True:
        return b"true"
    if o is False:
        return b"false"
    if isinstance(o, Raw):
        return bytes(o)
    if isinstance(o, Hex):
        return b"<" + bytes(o).hex().upper().encode("ascii") + b">"
    if isinstance(o, (bytes, bytearray)):
        return literal(bytes(o))
    if isinstance(o, str):
        return name_bytes(o)
    if isinstance(o, (int, float)):
        return num(o).encode("ascii")
    if isinstance(o, Ref):
        return b"%d %d R" % (o.num, o.gen)
    if isinstance(o, (list, tuple)):
        return b"[" + b" ".join(ser(x) for x in o) + b"]"
    if isinstance(o, dict):
        return b"<<" + b"".join(name_bytes(k) + b" " + ser(v) for k, v in o.items()) + b">>"
    raise TypeError(f"cannot serialize {type(o)!r}")


def text_string(s: str) -> bytes:
    """PDF text string: PDFDocEncoding-compatible ASCII or UTF-16BE with BOM."""
    try:
        return s.encode("ascii")
    except UnicodeEncodeError:
        return Hex(b"\xfe\xff" + s.encode("utf-16-be"))


def pdf_lit(data: bytes) -> str:
    """Literal string for use inside a content stream built as ``str``."""
    return literal(data).decode("latin-1")


def doc_id(seed: str) -> Hex:
    return Hex(hashlib.md5(seed.encode("utf-8")).digest())


def info_dict(title: str, subject: str = "", extra: dict | None = None) -> dict:
    d: dict[str, Any] = {
        "Title": text_string(title),
        "Producer": PRODUCER,
        "Creator": b"tools/fixtures/generate.py",
        "CreationDate": FIXED_PDF_DATE,
        "ModDate": FIXED_PDF_DATE,
    }
    if subject:
        d["Subject"] = text_string(subject)
    if extra:
        d.update(extra)
    return d


class RawPdf:
    """Sequential PDF writer: every object is written to ``fp`` immediately.

    Only the cross-reference offsets are kept in memory, so very large files
    can be produced in streaming fashion.
    """

    def __init__(self, fp: BinaryIO, version: str = "1.7", *, header: bytes | None = None,
                 objstm_size: int = 100):
        self.fp = fp
        self.pos = 0
        self.next_num = 1
        # num -> (type, field2, field3); type 1 = offset, type 2 = in object stream
        self.xref: dict[int, tuple[int, int, int]] = {}
        self.objstm_size = objstm_size
        self._objstm: list[tuple[int, bytes]] = []
        if header is None:
            header = b"%PDF-" + version.encode("ascii") + b"\n%\xe2\xe3\xcf\xd3\n"
        self.write(header)

    # -- low level -----------------------------------------------------
    def write(self, data: bytes) -> None:
        self.fp.write(data)
        self.pos += len(data)

    def alloc(self) -> Ref:
        ref = Ref(self.next_num)
        self.next_num += 1
        return ref

    def raw_obj(self, body: bytes, ref: Ref | None = None) -> Ref:
        """Write ``n g obj <body> endobj`` with an arbitrary (possibly invalid) body."""
        ref = ref or self.alloc()
        self.xref[ref.num] = (1, self.pos, ref.gen)
        self.write(b"%d %d obj\n" % (ref.num, ref.gen) + body + b"\nendobj\n")
        return ref

    def obj(self, value: Any, ref: Ref | None = None) -> Ref:
        return self.raw_obj(ser(value), ref)

    def stream(self, data: bytes, d: dict | None = None, ref: Ref | None = None, *,
               compress: bool = True, level: int = 6, length: Any = None) -> Ref:
        """Write a stream object. ``compress`` applies FlateDecode.

        ``length`` overrides the /Length value (used by malformed fixtures).
        """
        d = dict(d or {})
        if compress:
            data = zlib.compress(data, level)
            prev = d.get("Filter")
            if prev is None:
                d["Filter"] = "FlateDecode"
            else:
                d["Filter"] = ["FlateDecode"] + (list(prev) if isinstance(prev, list) else [prev])
        d["Length"] = len(data) if length is None else length
        ref = ref or self.alloc()
        self.xref[ref.num] = (1, self.pos, ref.gen)
        self.write(b"%d %d obj\n" % (ref.num, ref.gen) + ser(d) + b"\nstream\n")
        self.write(data)
        self.write(b"\nendstream\nendobj\n")
        return ref

    # -- object streams (PDF 1.5) ---------------------------------------
    def objstm_obj(self, value: Any, ref: Ref | None = None) -> Ref:
        """Queue a non-stream object for inclusion in an object stream."""
        ref = ref or self.alloc()
        self._objstm.append((ref.num, ser(value)))
        if len(self._objstm) >= self.objstm_size:
            self.flush_objstm()
        return ref

    def flush_objstm(self) -> None:
        if not self._objstm:
            return
        heads, body = [], bytearray()
        for n, data in self._objstm:
            heads.append(f"{n} {len(body)}")
            body += data + b"\n"
        header = (" ".join(heads) + "\n").encode("ascii")
        ref = self.stream(header + bytes(body),
                          {"Type": "ObjStm", "N": len(self._objstm), "First": len(header)})
        for idx, (n, _) in enumerate(self._objstm):
            self.xref[n] = (2, ref.num, idx)
        self._objstm.clear()

    # -- trailers ---------------------------------------------------------
    def finish(self, root: Ref, info: Ref | None = None, *, id_seed: str = "",
               xref_stream: bool = False, trailer_extra: dict | None = None,
               offset_fn: Callable[[int, int], int] | None = None,
               startxref_override: int | None = None) -> None:
        """Write the cross-reference section, trailer and ``%%EOF``.

        ``offset_fn`` / ``startxref_override`` exist to produce broken files.
        """
        ident = doc_id(id_seed or "fastpdf")
        if xref_stream:
            self.flush_objstm()
            xref_ref = self.alloc()
            size = self.next_num
            start = self.pos
            self.xref[xref_ref.num] = (1, start, 0)
            rows = bytearray()
            for n in range(size):
                if n == 0:
                    t, a, b = 0, 0, 65535
                else:
                    t, a, b = self.xref.get(n, (0, 0, 0))
                rows += bytes([t]) + a.to_bytes(4, "big") + b.to_bytes(2, "big")
            d: dict[str, Any] = {"Type": "XRef", "Size": size, "W": [1, 4, 2], "Root": root}
            if info:
                d["Info"] = info
            d["ID"] = [ident, ident]
            if trailer_extra:
                d.update(trailer_extra)
            self.stream(bytes(rows), d, xref_ref)
            self.write(b"startxref\n%d\n%%%%EOF\n" % (start if startxref_override is None
                                                      else startxref_override))
            return

        size = self.next_num
        start = self.pos
        out = [b"xref\n0 %d\n" % size, b"0000000000 65535 f \n"]
        for n in range(1, size):
            e = self.xref.get(n)
            if e and e[0] == 1:
                off = e[1] if offset_fn is None else offset_fn(n, e[1])
                out.append(b"%010d %05d n \n" % (off, e[2]))
            else:
                out.append(b"0000000000 00000 f \n")
        trailer: dict[str, Any] = {"Size": size, "Root": root}
        if info:
            trailer["Info"] = info
        trailer["ID"] = [ident, ident]
        if trailer_extra:
            trailer.update(trailer_extra)
        out.append(b"trailer\n" + ser(trailer) + b"\n")
        out.append(b"startxref\n%d\n%%%%EOF\n" % (start if startxref_override is None
                                                  else startxref_override))
        self.write(b"".join(out))


class Content:
    """Accumulates content-stream operators as text lines."""

    __slots__ = ("lines",)

    def __init__(self) -> None:
        self.lines: list[str] = []

    def __call__(self, line: str) -> None:
        self.lines.append(line)

    def extend(self, lines: Iterable[str]) -> None:
        self.lines.extend(lines)

    def data(self) -> bytes:
        return ("\n".join(self.lines) + "\n").encode("latin-1")


def page_tree(pdf: RawPdf, pages_ref: Ref, kids: list[Ref], extra: dict | None = None) -> None:
    d: dict[str, Any] = {"Type": "Pages", "Kids": kids, "Count": len(kids)}
    if extra:
        d.update(extra)
    pdf.obj(d, pages_ref)


def simple_document(pdf: RawPdf, pages: list[Ref], pages_ref: Ref, *, title: str,
                    id_seed: str, catalog_extra: dict | None = None,
                    pages_extra: dict | None = None, xref_stream: bool = False,
                    info_extra: dict | None = None) -> None:
    """Write the page tree, catalog, info and trailer for a flat document."""
    page_tree(pdf, pages_ref, pages, pages_extra)
    cat: dict[str, Any] = {"Type": "Catalog", "Pages": pages_ref}
    if catalog_extra:
        cat.update(catalog_extra)
    root = pdf.obj(cat)
    info = pdf.obj(info_dict(title, extra=info_extra))
    pdf.finish(root, info, id_seed=id_seed, xref_stream=xref_stream)


def add_page(pdf: RawPdf, pages_ref: Ref, kids: list[Ref], content: bytes, resources: dict,
             mediabox: list | tuple = (0, 0, 595.2756, 841.8898), extra: dict | None = None,
             compress: bool = True) -> Ref:
    """Write a content stream plus its page dictionary and append it to ``kids``."""
    c = pdf.stream(content, compress=compress)
    d: dict[str, Any] = {"Type": "Page", "Parent": pages_ref, "MediaBox": list(mediabox),
                         "Resources": resources, "Contents": c}
    if extra:
        d.update(extra)
    ref = pdf.obj(d)
    kids.append(ref)
    return ref


STD14 = (
    "Times-Roman", "Times-Bold", "Times-Italic", "Times-BoldItalic",
    "Helvetica", "Helvetica-Bold", "Helvetica-Oblique", "Helvetica-BoldOblique",
    "Courier", "Courier-Bold", "Courier-Oblique", "Courier-BoldOblique",
    "Symbol", "ZapfDingbats",
)


def std_font(base: str, encoding: Any = "WinAnsiEncoding") -> dict:
    d: dict[str, Any] = {"Type": "Font", "Subtype": "Type1", "BaseFont": base}
    if encoding is not None and base not in ("Symbol", "ZapfDingbats"):
        d["Encoding"] = encoding
    return d
