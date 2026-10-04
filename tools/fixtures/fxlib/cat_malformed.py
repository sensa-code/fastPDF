"""Category: malformed (hostile / damaged inputs for spec §24 and §25 guardrails).

Everything is written with rawpdf or as raw bytes, so each defect is exact and
reproducible. Bombs are produced incrementally and never inflated in memory.
"""

from __future__ import annotations

import io
import zlib
from functools import lru_cache
from pathlib import Path
from typing import Any

from .common import Ctx, Job, rng
from .rawpdf import (Content, Raw, RawPdf, Ref, add_page, info_dict, pdf_lit, ser,
                     simple_document, std_font)

LETTER = (0, 0, 612, 792)
MIB = 1024 * 1024


def text_ops(*lines: str, size: int = 18, top: int = 720) -> bytes:
    cs = Content()
    cs(f"BT /F1 {size} Tf 72 {top} Td {int(size * 1.4)} TL")
    for line in lines:
        cs(f"{pdf_lit(line.encode('latin-1', 'replace'))} Tj T*")
    cs("ET")
    return cs.data()


def _doc(out: Path | io.BytesIO, label: str, pages: int, *, header: bytes | None = None,
         **finish: Any) -> bytes | None:
    """A small valid document; returns bytes when ``out`` is a BytesIO."""
    fp = out if isinstance(out, io.BytesIO) else open(out, "wb")
    try:
        pdf = RawPdf(fp, "1.7", header=header)
        font = pdf.obj(std_font("Helvetica"))
        pages_ref = pdf.alloc()
        kids: list[Ref] = []
        filler = " ".join(f"filler-{i:04d}" for i in range(220))
        for i in range(pages):
            add_page(pdf, pages_ref, kids,
                     text_ops(f"Malformed fixture: {label}", f"Page {i + 1} of {pages}",
                              filler[:90], filler[90:180]) + b"%" + filler.encode() + b"\n",
                     {"Font": {"F1": font}}, LETTER)
        pdf.obj({"Type": "Pages", "Kids": kids, "Count": len(kids)}, pages_ref)
        root = pdf.obj({"Type": "Catalog", "Pages": pages_ref})
        info = pdf.obj(info_dict(f"FastPDF malformed fixture: {label}"))
        pdf.finish(root, info, id_seed="malformed-" + label, **finish)
    finally:
        if not isinstance(out, io.BytesIO):
            fp.close()
    return out.getvalue() if isinstance(out, io.BytesIO) else None


def _write(out: Path, data: bytes) -> None:
    with open(out, "wb") as f:
        f.write(data)


@lru_cache(maxsize=2)
def zero_bomb(total: int, level: int = 9) -> bytes:
    """Deflate stream of ``total`` zero bytes, produced chunk by chunk."""
    comp = zlib.compressobj(level)
    chunk = bytes(MIB)
    parts = []
    for _ in range(total // MIB):
        parts.append(comp.compress(chunk))
    parts.append(comp.flush())
    return b"".join(parts)


# --------------------------------------------------------------------------
# structure / xref damage
# --------------------------------------------------------------------------
def m_truncated_mid(out: Path, ctx: Ctx) -> dict:
    """Catalog and page tree first, then pages; cut at 60 % of the file."""
    buf = io.BytesIO()
    pdf = RawPdf(buf)
    font = pdf.obj(std_font("Helvetica"))
    pages_ref = pdf.alloc()
    root = pdf.obj({"Type": "Catalog", "Pages": pages_ref})
    kid_refs = [pdf.alloc() for _ in range(3)]
    pdf.obj({"Type": "Pages", "Kids": kid_refs, "Count": 3}, pages_ref)
    r = rng("m-truncated")
    for i, ref in enumerate(kid_refs):
        body = text_ops("Malformed fixture: truncated at 60%", f"Page {i + 1} of 3")
        body += b"%" + bytes(r.randrange(65, 90) for _ in range(6000)) + b"\n"
        content = pdf.stream(body)
        pdf.obj({"Type": "Page", "Parent": pages_ref, "MediaBox": list(LETTER),
                 "Resources": {"Font": {"F1": font}}, "Contents": content}, ref)
    info = pdf.obj(info_dict("FastPDF malformed fixture: truncated"))
    pdf.finish(root, info, id_seed="m-truncated")
    data = buf.getvalue()
    _write(out, data[: int(len(data) * 0.6)])
    return {"pages": None}


def m_truncated_before_xref(out: Path, ctx: Ctx) -> dict:
    data = _doc(io.BytesIO(), "truncated before xref", 3)
    _write(out, data[: data.rindex(b"xref\n0 ")])
    return {"pages": 3}


def m_bad_xref_offsets(out: Path, ctx: Ctx) -> dict:
    _doc(out, "xref offsets shifted by +7", 3, offset_fn=lambda n, off: off + 7)
    return {"pages": 3}


def m_startxref_beyond_eof(out: Path, ctx: Ctx) -> dict:
    _doc(out, "startxref beyond EOF", 3, startxref_override=99_999_999)
    return {"pages": 3}


def m_xref_garbled(out: Path, ctx: Ctx) -> dict:
    data = _doc(io.BytesIO(), "garbled xref table", 2)
    start = data.rindex(b"xref\n0 ")
    end = data.index(b"trailer", start)
    lines = data[start:end].split(b"\n")
    garbled = [lines[0], b"0 99"]  # subsection claims more entries than exist
    for i, line in enumerate(lines[2:]):
        if not line:
            continue
        if i % 3 == 0:
            garbled.append(line[:-3] + b"x")      # bad type keyword, wrong width
        elif i % 3 == 1:
            garbled.append(b"  " + line.strip())  # misaligned entry
        # every third entry is dropped entirely
    _write(out, data[:start] + b"\n".join(garbled) + b"\n" + data[end:])
    return {"pages": 2}


def m_xref_prev_cycle(out: Path, ctx: Ctx) -> dict:
    """Incremental update whose xref chain loops: A -> /Prev B -> /Prev A."""
    buf = io.BytesIO()
    pdf = RawPdf(buf)
    font = pdf.obj(std_font("Helvetica"))
    pages_ref = pdf.alloc()
    kids: list[Ref] = []
    page = add_page(pdf, pages_ref, kids, text_ops("Malformed fixture: xref /Prev cycle",
                                                   "Original revision"),
                    {"Font": {"F1": font}}, LETTER)
    pdf.obj({"Type": "Pages", "Kids": kids, "Count": 1}, pages_ref)
    root = pdf.obj({"Type": "Catalog", "Pages": pages_ref})
    placeholder = Raw(b"9999999999")
    pdf.finish(root, None, id_seed="m-prev-cycle", trailer_extra={"Prev": placeholder})
    first_xref = buf.getvalue().rindex(b"xref\n0 ")
    # incremental update: replace the page's content stream
    new_content = pdf.stream(text_ops("Malformed fixture: xref /Prev cycle",
                                      "Updated revision (incremental save)"))
    page_new = pdf.obj({"Type": "Page", "Parent": pages_ref, "MediaBox": list(LETTER),
                        "Resources": {"Font": {"F1": font}}, "Contents": new_content}, page)
    second_xref = pdf.pos
    rows = [b"xref\n0 1\n0000000000 65535 f \n"]
    for ref in (new_content, page_new):
        rows.append(b"%d 1\n%010d 00000 n \n" % (ref.num, pdf.xref[ref.num][1]))
    trailer = {"Size": pdf.next_num, "Root": root, "Prev": first_xref}
    pdf.write(b"".join(rows) + b"trailer\n" + ser(trailer) + b"\nstartxref\n%d\n%%%%EOF\n"
              % second_xref)
    data = buf.getvalue().replace(b"/Prev 9999999999", b"/Prev %010d" % second_xref, 1)
    _write(out, data)
    return {"pages": 1}


def m_no_header(out: Path, ctx: Ctx) -> dict:
    _doc(out, "missing %PDF header", 2, header=b"")
    return {"pages": 2}


def m_leading_junk(out: Path, ctx: Ctx) -> dict:
    data = _doc(io.BytesIO(), "junk before header", 2)
    r = rng("m-leading-junk")
    junk = (b"HTTP/1.1 200 OK\r\nContent-Type: application/pdf\r\nX-Note: fixture\r\n\r\n"
            + bytes(r.randrange(256) for _ in range(600)))
    _write(out, junk.replace(b"%PDF", b"%PDX") + data)
    return {"pages": 2}


def m_garbage_after_eof(out: Path, ctx: Ctx, size: int) -> dict:
    data = _doc(io.BytesIO(), f"{size} bytes of garbage after %%EOF", 2)
    r = rng(f"m-garbage-{size}")
    _write(out, data + bytes(r.randrange(256) for _ in range(size)))
    return {"pages": 2}


def m_no_catalog(out: Path, ctx: Ctx) -> dict:
    buf = io.BytesIO()
    pdf = RawPdf(buf)
    font = pdf.obj(std_font("Helvetica"))
    pages_ref = pdf.alloc()
    kids: list[Ref] = []
    add_page(pdf, pages_ref, kids, text_ops("orphan page, no catalog"), {"Font": {"F1": font}},
             LETTER)
    pdf.obj({"Type": "Pages", "Kids": kids, "Count": 1}, pages_ref)
    info = pdf.obj(info_dict("FastPDF malformed fixture: no catalog"))
    data = buf.getvalue()
    # trailer without /Root; written by hand
    start = pdf.pos
    rows = [b"xref\n0 %d\n0000000000 65535 f \n" % pdf.next_num]
    for n in range(1, pdf.next_num):
        rows.append(b"%010d 00000 n \n" % pdf.xref[n][1])
    rows.append(b"trailer\n" + ser({"Size": pdf.next_num, "Info": info}) +
                b"\nstartxref\n%d\n%%%%EOF\n" % start)
    _write(out, data + b"".join(rows))
    return {"pages": None}


def m_random_bytes(out: Path, ctx: Ctx) -> dict:
    r = rng("m-random")
    _write(out, bytes(r.randrange(256) for _ in range(16384)))
    return {"pages": None}


def m_empty(out: Path, ctx: Ctx) -> dict:
    _write(out, b"")
    return {"pages": None}


# --------------------------------------------------------------------------
# object-level damage
# --------------------------------------------------------------------------
def m_missing_object(out: Path, ctx: Ctx) -> dict:
    with open(out, "wb") as fp:
        pdf = RawPdf(fp)
        font = pdf.obj(std_font("Helvetica"))
        missing_font = pdf.alloc()       # allocated, never written (free xref entry)
        missing_content = pdf.alloc()
        pages_ref = pdf.alloc()
        kids: list[Ref] = []
        add_page(pdf, pages_ref, kids, text_ops("Missing object fixture", "Page 1: /Annots -> 9999 0 R"),
                 {"Font": {"F1": font}}, LETTER, extra={"Annots": [Ref(9999)]})
        kids.append(pdf.obj({"Type": "Page", "Parent": pages_ref, "MediaBox": list(LETTER),
                             "Resources": {"Font": {"F1": font}}, "Contents": missing_content}))
        add_page(pdf, pages_ref, kids, text_ops("Page 3: font resource is missing"),
                 {"Font": {"F1": missing_font}}, LETTER)
        add_page(pdf, pages_ref, kids, text_ops("Page 4: control page (valid)"),
                 {"Font": {"F1": font}}, LETTER)
        simple_document(pdf, kids, pages_ref, title="FastPDF malformed fixture: missing objects",
                        id_seed="m-missing")
    return {"pages": 4}


def m_corrupt_flate(out: Path, ctx: Ctx) -> dict:
    with open(out, "wb") as fp:
        pdf = RawPdf(fp)
        font = pdf.obj(std_font("Helvetica"))
        pages_ref = pdf.alloc()
        kids: list[Ref] = []
        res = {"Font": {"F1": font}}
        body = text_ops("Corrupt Flate fixture", *[f"line {i}" for i in range(30)], size=14)
        good = zlib.compress(body)
        flipped = bytearray(good)
        for i in range(len(flipped) // 3, len(flipped) // 3 + 24):
            flipped[i] ^= 0x5A
        add_page(pdf, pages_ref, kids, text_ops("Page 1: valid"), res, LETTER)
        for data, label in ((bytes(flipped), "bit flips in the middle"),
                            (good[: len(good) // 2], "truncated deflate data"),
                            (b"\x78\x9c" + bytes(range(256)) * 4, "not deflate at all")):
            c = pdf.stream(data, {"Filter": "FlateDecode"}, compress=False)
            kids.append(pdf.obj({"Type": "Page", "Parent": pages_ref, "MediaBox": list(LETTER),
                                 "Resources": res, "Contents": c,
                                 "FastPDFNote": label.encode()}))
        add_page(pdf, pages_ref, kids, text_ops("Page 5: valid control page"), res, LETTER)
        simple_document(pdf, kids, pages_ref, title="FastPDF malformed fixture: corrupt Flate",
                        id_seed="m-flate")
    return {"pages": 5}


def m_wrong_length(out: Path, ctx: Ctx) -> dict:
    with open(out, "wb") as fp:
        pdf = RawPdf(fp)
        font = pdf.obj(std_font("Helvetica"))
        pages_ref = pdf.alloc()
        kids: list[Ref] = []
        res = {"Font": {"F1": font}}
        missing = pdf.alloc()
        cases = [(lambda n: 12, "/Length too small"), (lambda n: n + 4000, "/Length too large"),
                 (lambda n: missing, "/Length -> missing indirect object"),
                 (lambda n: Raw(b"99999999999999999999"), "/Length overflows 64-bit"),
                 (lambda n: -50, "/Length negative")]
        for i, (length, label) in enumerate(cases):
            body = text_ops(f"Wrong /Length fixture, page {i + 1}", label)
            c = pdf.stream(body, {}, compress=False, length=length(len(body)))
            kids.append(pdf.obj({"Type": "Page", "Parent": pages_ref, "MediaBox": list(LETTER),
                                 "Resources": res, "Contents": c}))
        simple_document(pdf, kids, pages_ref, title="FastPDF malformed fixture: wrong /Length",
                        id_seed="m-length")
    return {"pages": 5}


def m_kids_cycle(out: Path, ctx: Ctx) -> dict:
    with open(out, "wb") as fp:
        pdf = RawPdf(fp)
        font = pdf.obj(std_font("Helvetica"))
        root_pages, mid = pdf.alloc(), pdf.alloc()
        res = {"Font": {"F1": font}}
        p1 = pdf.obj({"Type": "Page", "Parent": root_pages, "MediaBox": list(LETTER),
                      "Resources": res, "Contents": pdf.stream(text_ops("Kids cycle: page 1"))})
        p2 = pdf.obj({"Type": "Page", "Parent": mid, "MediaBox": list(LETTER), "Resources": res,
                      "Contents": pdf.stream(text_ops("Kids cycle: page 2"))})
        pdf.obj({"Type": "Pages", "Kids": [p1, mid], "Count": 3}, root_pages)
        pdf.obj({"Type": "Pages", "Parent": root_pages, "Kids": [p2, root_pages], "Count": 2}, mid)
        root = pdf.obj({"Type": "Catalog", "Pages": root_pages})
        pdf.finish(root, pdf.obj(info_dict("FastPDF malformed fixture: page tree cycle")),
                   id_seed="m-kids-cycle")
    return {"pages": None}


def m_parent_cycle(out: Path, ctx: Ctx) -> dict:
    with open(out, "wb") as fp:
        pdf = RawPdf(fp)
        root_pages, a, b = pdf.alloc(), pdf.alloc(), pdf.alloc()
        page = pdf.obj({"Type": "Page", "Parent": a,
                        "Contents": pdf.stream(b"0.8 0 0 rg 100 100 200 200 re f")})
        pdf.obj({"Type": "Pages", "Kids": [page], "Count": 1}, root_pages)
        pdf.obj({"Type": "Pages", "Parent": b, "Kids": [page], "Count": 1}, a)
        pdf.obj({"Type": "Pages", "Parent": a, "Kids": [a], "Count": 1}, b)
        root = pdf.obj({"Type": "Catalog", "Pages": root_pages})
        pdf.finish(root, pdf.obj(info_dict("FastPDF malformed fixture: /Parent cycle")),
                   id_seed="m-parent-cycle")
    return {"pages": 1}


def m_count_lie(out: Path, ctx: Ctx) -> dict:
    with open(out, "wb") as fp:
        pdf = RawPdf(fp)
        font = pdf.obj(std_font("Helvetica"))
        pages_ref = pdf.alloc()
        kids: list[Ref] = []
        for i in range(2):
            add_page(pdf, pages_ref, kids, text_ops("/Count claims 2147483647 pages",
                                                    f"real page {i + 1} of 2"),
                     {"Font": {"F1": font}}, LETTER)
        pdf.obj({"Type": "Pages", "Kids": kids, "Count": 2147483647}, pages_ref)
        root = pdf.obj({"Type": "Catalog", "Pages": pages_ref})
        pdf.finish(root, pdf.obj(info_dict("FastPDF malformed fixture: /Count lie")),
                   id_seed="m-count-lie")
    return {"pages": 2}


def m_indirect_cycle(out: Path, ctx: Ctx) -> dict:
    with open(out, "wb") as fp:
        pdf = RawPdf(fp)
        font = pdf.obj(std_font("Helvetica"))
        a, b, selfref = pdf.alloc(), pdf.alloc(), pdf.alloc()
        pdf.obj(b, a)              # a -> b
        pdf.obj(a, b)              # b -> a
        pdf.obj(selfref, selfref)  # self reference
        pages_ref = pdf.alloc()
        kids: list[Ref] = []
        body = text_ops("Indirect reference cycles", "/Length 7 0 R -> 8 0 R -> 7 0 R")
        c = pdf.stream(body, {}, compress=False, length=a)
        kids.append(pdf.obj({"Type": "Page", "Parent": pages_ref, "MediaBox": list(LETTER),
                             "Resources": selfref, "Contents": c}))
        add_page(pdf, pages_ref, kids, text_ops("Control page (valid)"), {"Font": {"F1": font}},
                 LETTER)
        simple_document(pdf, kids, pages_ref, title="FastPDF malformed fixture: reference cycles",
                        id_seed="m-ref-cycle")
    return {"pages": 2}


def _deep(kind: str, depth: int) -> Raw:
    if kind == "array":
        return Raw(b"[" * depth + b"0" + b"]" * depth)
    return Raw(b"<</A " * depth + b"0" + b">>" * depth)


def m_deep_nesting(out: Path, ctx: Ctx, kind: str, depth: int) -> dict:
    with open(out, "wb") as fp:
        pdf = RawPdf(fp)
        font = pdf.obj(std_font("Helvetica"))
        res = {"Font": {"F1": font}}
        pages_ref = pdf.alloc()
        kids: list[Ref] = []
        deep_ref = pdf.raw_obj(bytes(_deep(kind, depth)))
        add_page(pdf, pages_ref, kids, text_ops(f"{depth}-deep {kind} in an indirect object",
                                                "referenced from /FastPDFJunk"),
                 res, LETTER, extra={"FastPDFJunk": deep_ref})
        add_page(pdf, pages_ref, kids, text_ops(f"{depth}-deep {kind} inline in this page dict"),
                 res, LETTER, extra={"FastPDFJunk": _deep(kind, depth)})
        add_page(pdf, pages_ref, kids, text_ops("Control page (valid)"), res, LETTER)
        simple_document(pdf, kids, pages_ref,
                        title=f"FastPDF malformed fixture: deep {kind} nesting",
                        id_seed=f"m-deep-{kind}-{depth}")
    return {"pages": 3}


def m_deep_content(out: Path, ctx: Ctx, depth: int = 100_000) -> dict:
    with open(out, "wb") as fp:
        pdf = RawPdf(fp)
        font = pdf.obj(std_font("Helvetica"))
        res = {"Font": {"F1": font}}
        pages_ref = pdf.alloc()
        kids: list[Ref] = []
        nested = b"[" * depth + b"]" * depth + b" 0 d\n"
        add_page(pdf, pages_ref, kids, text_ops(f"{depth}-deep array operand in content") + nested
                 + text_ops("(text after the nested array)", top=600), res, LETTER)
        qs = b"q " * depth + b"\n" + text_ops(f"{depth} nested q (graphics state stack)") \
            + b"Q " * depth + b"\n"
        add_page(pdf, pages_ref, kids, qs, res, LETTER)
        unbalanced = b"q " * depth + b"\n" + text_ops(f"{depth} q without Q")
        add_page(pdf, pages_ref, kids, unbalanced, res, LETTER)
        add_page(pdf, pages_ref, kids, text_ops("Control page (valid)"), res, LETTER)
        simple_document(pdf, kids, pages_ref, title="FastPDF malformed fixture: deep content",
                        id_seed="m-deep-content")
    return {"pages": 4}


# --------------------------------------------------------------------------
# resource exhaustion
# --------------------------------------------------------------------------
def m_huge_mediabox(out: Path, ctx: Ctx) -> dict:
    with open(out, "wb") as fp:
        pdf = RawPdf(fp)
        font = pdf.obj(std_font("Helvetica"))
        res = {"Font": {"F1": font}}
        pages_ref = pdf.alloc()
        kids: list[Ref] = []
        boxes = [
            ([0, 0, 10_000_000, 10_000_000], {}, "MediaBox 1e7 x 1e7 pt"),
            ([0, 0, 0, 0], {}, "zero-area MediaBox"),
            ([612, 792, 0, 0], {}, "inverted MediaBox (must be normalized)"),
            (Raw(b"[0 0 99999999999999999999 792]"), {}, "integer overflow width"),
            ([0, 0, 14400, 14400], {"UserUnit": 75000}, "14400 pt with /UserUnit 75000"),
            ([-5_000_000, -5_000_000, 5_000_000, 5_000_000], {"Rotate": 90},
             "negative origin, rotated"),
            (list(LETTER), {}, "control page (valid Letter)"),
        ]
        for box, extra, label in boxes:
            add_page(pdf, pages_ref, kids, text_ops(f"MediaBox: {label}", size=12, top=100),
                     res, (0, 0, 1, 1), extra={**extra, "MediaBox": box})
        simple_document(pdf, kids, pages_ref, title="FastPDF malformed fixture: huge MediaBox",
                        id_seed="m-mediabox")
    return {"pages": len(boxes)}


def _patched_huge_jpeg() -> bytes:
    from PIL import Image

    from .imaging import jpeg

    data = bytearray(jpeg(Image.new("RGB", (64, 64), (200, 30, 30)), 75))
    sof = data.index(b"\xff\xc0")
    data[sof + 5:sof + 9] = (65535).to_bytes(2, "big") + (65535).to_bytes(2, "big")
    return bytes(data)


def m_huge_image(out: Path, ctx: Ctx) -> dict:
    with open(out, "wb") as fp:
        pdf = RawPdf(fp)
        font = pdf.obj(std_font("Helvetica"))
        pages_ref = pdf.alloc()
        kids: list[Ref] = []
        small = zlib.compress(bytes(4096))
        images = [
            ({"Width": 100_000, "Height": 100_000, "ColorSpace": "DeviceRGB",
              "BitsPerComponent": 8, "Filter": "FlateDecode"}, small,
             "100000 x 100000 RGB declared (30 GB), 4 KiB of data"),
            ({"Width": 2_147_483_647, "Height": 2, "ColorSpace": "DeviceRGB",
              "BitsPerComponent": 8, "Filter": "FlateDecode"}, small,
             "Width 2^31-1 (w*h*3 overflows 32-bit)"),
            ({"Width": 65535, "Height": 65535, "ColorSpace": "DeviceRGB",
              "BitsPerComponent": 8, "Filter": "DCTDecode"}, _patched_huge_jpeg(),
             "JPEG whose SOF claims 65535 x 65535"),
            ({"Width": 300_000, "Height": 300_000, "ImageMask": True, "BitsPerComponent": 1,
              "Filter": "FlateDecode"}, small, "300000 x 300000 1-bit ImageMask"),
        ]
        for d, data, label in images:
            img = pdf.stream(data, {"Type": "XObject", "Subtype": "Image", **d}, compress=False)
            add_page(pdf, pages_ref, kids,
                     b"q 400 0 0 400 106 250 cm /Im0 Do Q\n" + text_ops(label, size=12, top=120),
                     {"Font": {"F1": font}, "XObject": {"Im0": img}}, LETTER)
        add_page(pdf, pages_ref, kids, text_ops("Control page (valid)"), {"Font": {"F1": font}},
                 LETTER)
        simple_document(pdf, kids, pages_ref, title="FastPDF malformed fixture: huge images",
                        id_seed="m-huge-image")
    return {"pages": len(images) + 1}


def m_bomb(out: Path, ctx: Ctx, nested: bool = False) -> dict:
    total = 512 * MIB
    with open(out, "wb") as fp:
        pdf = RawPdf(fp)
        font = pdf.obj(std_font("Helvetica"))
        res = {"Font": {"F1": font}}
        pages_ref = pdf.alloc()
        kids: list[Ref] = []
        add_page(pdf, pages_ref, kids, text_ops("Decompression bomb fixture", "Page 1: valid"),
                 res, LETTER)
        bomb = zero_bomb(total)
        if nested:
            outer = zlib.compress(bomb, 9)
            c = pdf.stream(outer, {"Filter": ["FlateDecode", "FlateDecode"]}, compress=False)
            kids.append(pdf.obj({"Type": "Page", "Parent": pages_ref, "MediaBox": list(LETTER),
                                 "Resources": res, "Contents": c}))
        else:
            c = pdf.stream(bomb, {"Filter": "FlateDecode"}, compress=False)
            kids.append(pdf.obj({"Type": "Page", "Parent": pages_ref, "MediaBox": list(LETTER),
                                 "Resources": res, "Contents": c}))
            img = pdf.stream(bomb, {"Type": "XObject", "Subtype": "Image", "Width": 1000,
                                    "Height": 1000, "ColorSpace": "DeviceGray",
                                    "BitsPerComponent": 8, "Filter": "FlateDecode"},
                             compress=False)
            add_page(pdf, pages_ref, kids,
                     b"q 400 0 0 400 106 250 cm /Im0 Do Q\n" +
                     text_ops("1000x1000 gray image whose stream inflates to 512 MiB", size=12,
                              top=120), {"Font": {"F1": font}, "XObject": {"Im0": img}}, LETTER)
        add_page(pdf, pages_ref, kids, text_ops("Control page (valid)"), res, LETTER)
        simple_document(pdf, kids, pages_ref, title="FastPDF malformed fixture: decompression bomb",
                        id_seed="m-bomb-nested" if nested else "m-bomb")
    return {"pages": len(kids), "inflated_bytes": total}


# --------------------------------------------------------------------------
# fonts and content syntax
# --------------------------------------------------------------------------
def m_broken_fonts(out: Path, ctx: Ctx) -> dict:
    import reportlab

    r = rng("m-broken-fonts")
    vera = (Path(reportlab.__file__).parent / "fonts" / "Vera.ttf").read_bytes()
    with open(out, "wb") as fp:
        pdf = RawPdf(fp)
        helv = pdf.obj(std_font("Helvetica"))
        pages_ref = pdf.alloc()
        kids: list[Ref] = []

        def descriptor(name: str, key: str, data: bytes, extra: dict | None = None) -> Ref:
            ff = pdf.stream(data, {"Length1": len(data), **(extra or {})})
            return pdf.obj({"Type": "FontDescriptor", "FontName": name, "Flags": 32,
                            "FontBBox": [-200, -200, 1200, 1000], "ItalicAngle": 0,
                            "Ascent": 800, "Descent": -200, "CapHeight": 700, "StemV": 80,
                            key: ff})

        garbage = bytes(r.randrange(256) for _ in range(4096))
        fonts = [
            ("TrueType FontFile2 = random bytes",
             {"Type": "Font", "Subtype": "TrueType", "BaseFont": "BrokenTT", "FirstChar": 32,
              "LastChar": 126, "Widths": [600] * 95, "Encoding": "WinAnsiEncoding",
              "FontDescriptor": descriptor("BrokenTT", "FontFile2", garbage)}),
            ("Type1 FontFile = random bytes",
             {"Type": "Font", "Subtype": "Type1", "BaseFont": "BrokenT1", "FirstChar": 32,
              "LastChar": 126, "Widths": [600] * 95,
              "FontDescriptor": descriptor("BrokenT1", "FontFile", garbage,
                                           {"Length2": 2048, "Length3": 0})}),
            ("TrueType truncated to 30% (Vera.ttf)",
             {"Type": "Font", "Subtype": "TrueType", "BaseFont": "Vera", "FirstChar": 32,
              "LastChar": 126, "Widths": [600] * 95, "Encoding": "WinAnsiEncoding",
              "FontDescriptor": descriptor("Vera", "FontFile2", vera[: len(vera) * 3 // 10])}),
        ]
        cid = pdf.obj({"Type": "Font", "Subtype": "CIDFontType2", "BaseFont": "BrokenCID",
                       "CIDSystemInfo": {"Registry": b"Adobe", "Ordering": b"Identity",
                                         "Supplement": 0},
                       "FontDescriptor": descriptor("BrokenCID", "FontFile2", garbage[::-1]),
                       "DW": 1000, "W": Raw(b"[0 [500 500] 3 5 600 7 [ ]"),
                       "CIDToGIDMap": "Identity"})
        tounicode = pdf.stream(b"begincmap garbage <00> <zz> beginbfchar <0001> endcmap", {})
        fonts.append(("Type0 + broken W array + broken ToUnicode",
                      {"Type": "Font", "Subtype": "Type0", "BaseFont": "BrokenCID",
                       "Encoding": "Identity-H", "DescendantFonts": [cid],
                       "ToUnicode": tounicode}))
        for label, fdict in fonts:
            f = pdf.obj(fdict)
            body = (text_ops(label, size=12, top=740).replace(b"/F1", b"/FH")
                    + b"BT /FX 28 Tf 72 600 Td (The quick brown fox 0123456789) Tj ET\n")
            add_page(pdf, pages_ref, kids, body, {"Font": {"FX": f, "FH": helv}}, LETTER)
        add_page(pdf, pages_ref, kids, text_ops("Control page (valid)").replace(b"/F1", b"/FH"),
                 {"Font": {"FH": helv}}, LETTER)
        simple_document(pdf, kids, pages_ref, title="FastPDF malformed fixture: broken fonts",
                        id_seed="m-fonts")
    return {"pages": len(kids)}


def m_syntax_errors(out: Path, ctx: Ctx) -> dict:
    with open(out, "wb") as fp:
        pdf = RawPdf(fp)
        font = pdf.obj(std_font("Helvetica"))
        res = {"Font": {"F1": font}}
        pages_ref = pdf.alloc()
        kids: list[Ref] = []
        bodies = [
            text_ops("Unknown operators / operand underflow") +
            b"foo bar 1 2 baz\nTf\n10 20 re f\nET\nQ Q Q\n1 0 0 rg 100 100 50 50 re f\n",
            text_ops("Unbalanced dictionaries and binary garbage") +
            b"<< /A 1 >> >> ] ) \x00\x01\x02\xff\xfe garbage 0 0 1 rg 200 100 60 60 re f\n"
            b"(unterminated string ...",
            b"BT /F1 18 Tf 72 720 Td (BT without ET; nested BT) Tj BT 0 -30 Td (inner) Tj\n"
            b"1..2 --5 +-3 0.0.0 Td (invalid numbers) Tj\n",
            text_ops("Inline image with too little data") +
            b"q 200 0 0 200 100 300 cm BI /W 100 /H 100 /BPC 8 /CS /G ID " + bytes(10) +
            b"\nEI Q\n0 0 1 rg 400 100 50 50 re f\n",
            text_ops("Control page (valid)"),
        ]
        for body in bodies:
            add_page(pdf, pages_ref, kids, body, res, LETTER)
        simple_document(pdf, kids, pages_ref, title="FastPDF malformed fixture: content syntax",
                        id_seed="m-syntax")
    return {"pages": len(bodies)}


G24 = "§24"
G25 = "§25"


def _m(name: str, func, expected: str, desc: str, guardrails: tuple[str, ...],
       cost: float = 0.1, **kwargs) -> Job:
    return Job(f"malformed/{name}.pdf", func, expected, desc, guardrails=guardrails,
               features=("malformed",), kwargs=kwargs, cost=cost)


def jobs() -> list[Job]:
    return [
        _m("truncated-at-60pct", m_truncated_mid, "render_partial_ok",
           "檔案在 60% 處被截斷（catalog／page tree 在前，後半頁面物件、xref、trailer 遺失）。",
           (f"{G24} malformed xref", f"{G24} missing object")),
        _m("truncated-before-xref", m_truncated_before_xref, "recover_expected",
           "所有物件完整，但在 xref 之前截斷（無 xref／trailer／startxref／%%EOF），需重建 xref。",
           (f"{G24} malformed xref",)),
        _m("bad-xref-offsets", m_bad_xref_offsets, "recover_expected",
           "xref 表每個 offset 都偏移 +7 bytes，需驗證 offset 並 fallback 重建。",
           (f"{G24} malformed xref",)),
        _m("startxref-beyond-eof", m_startxref_beyond_eof, "recover_expected",
           "startxref 指向檔案結尾之後。", (f"{G24} malformed xref",)),
        _m("xref-table-garbled", m_xref_garbled, "recover_expected",
           "xref subsection 宣告 99 筆但內容不足、型別字元錯誤、欄寬錯誤、部分項目遺失。",
           (f"{G24} malformed xref",)),
        _m("xref-prev-cycle", m_xref_prev_cycle, "recover_expected",
           "incremental update 的 xref 鏈 /Prev 形成循環（A→B→A），必須偵測迴圈避免無窮迴圈。",
           (f"{G25} infinite recursion", f"{G24} malformed xref")),
        _m("no-header", m_no_header, "recover_expected",
           "缺少 %PDF- header（offset 仍正確）。", (f"{G24} malformed xref",)),
        _m("leading-junk-before-header", m_leading_junk, "recover_expected",
           "header 前有約 700 bytes 的 HTTP header 與二進位垃圾（xref offset 以 %PDF 為基準）。",
           (f"{G24} malformed xref",)),
        _m("garbage-after-eof-small", m_garbage_after_eof, "open_ok",
           "%%EOF 之後附加 200 bytes 隨機資料（仍在最後 1024 bytes 內可找到 startxref）。",
           (f"{G24} malformed xref",), size=200),
        _m("garbage-after-eof-64k", m_garbage_after_eof, "recover_expected",
           "%%EOF 之後附加 64 KiB 隨機資料，startxref 不在檔尾 1024 bytes 內。",
           (f"{G24} malformed xref",), size=65536),
        _m("no-catalog", m_no_catalog, "open_error_expected",
           "trailer 沒有 /Root，檔案中也沒有 Catalog 物件。", (f"{G24} invalid object",)),
        _m("not-a-pdf-random-bytes", m_random_bytes, "open_error_expected",
           "16 KiB 隨機位元組，副檔名為 .pdf。", (f"{G24} invalid object",)),
        _m("empty-file", m_empty, "open_error_expected", "0 byte 檔案。", (f"{G24} invalid object",)),
        _m("missing-object", m_missing_object, "render_partial_ok",
           "引用不存在的物件：/Annots → 9999 0 R、/Contents 指向 free entry、字型資源遺失；第 4 頁正常。",
           (f"{G24} missing object", f"{G24} invalid object")),
        _m("corrupt-flate-stream", m_corrupt_flate, "render_partial_ok",
           "content stream 損壞：中段位元翻轉、deflate 截斷、非 deflate 資料；第 1、5 頁正常。",
           (f"{G24} corrupted stream",)),
        _m("wrong-stream-length", m_wrong_length, "recover_expected",
           "stream /Length 錯誤：太小、太大、指向不存在物件、超過 64-bit、負值；需以 endstream 掃描修正。",
           (f"{G24} corrupted stream", f"{G25} max object length", f"{G25} integer overflow")),
        _m("page-tree-kids-cycle", m_kids_cycle, "render_partial_ok",
           "page tree 的 /Kids 形成循環（中間節點的 kid 指回根節點），/Count 不一致。",
           (f"{G25} infinite recursion", f"{G25} malformed object tree")),
        _m("page-parent-cycle", m_parent_cycle, "render_partial_ok",
           "頁面沒有 MediaBox／Resources，且 /Parent 鏈形成循環（屬性繼承查找可能無窮迴圈）。",
           (f"{G25} infinite recursion", f"{G25} malformed object tree")),
        _m("page-count-lie", m_count_lie, "recover_expected",
           "/Count 宣稱 2147483647 頁但只有 2 個 kids，不可依 /Count 預先配置記憶體。",
           (f"{G25} integer overflow", f"{G25} memory exhaustion")),
        _m("indirect-reference-cycle", m_indirect_cycle, "guardrail_expected",
           "間接參照循環：/Length 7 0 R → 8 0 R → 7 0 R，/Resources 指向自己。",
           (f"{G25} infinite recursion", f"{G25} max recursion")),
        _m("deep-nesting-array-5000", m_deep_nesting, "guardrail_expected",
           "5000 層巢狀 array（一個在獨立物件、一個 inline 在 page dict），第 3 頁正常。",
           (f"{G25} max nesting", f"{G25} infinite recursion"), kind="array", depth=5000),
        _m("deep-nesting-dict-5000", m_deep_nesting, "guardrail_expected",
           "5000 層巢狀 dictionary（同上配置）。",
           (f"{G25} max nesting", f"{G25} infinite recursion"), kind="dict", depth=5000),
        _m("deep-nesting-content-100000", m_deep_content, "guardrail_expected",
           "content stream 內 100000 層巢狀 array operand、100000 層 q…Q、100000 個未配對 q；第 4 頁正常。",
           (f"{G25} max nesting", f"{G25} max recursion")),
        _m("huge-mediabox", m_huge_mediabox, "guardrail_expected",
           "MediaBox 1e7x1e7、零面積、反向、整數溢位、/UserUnit 75000、負座標＋旋轉；最後一頁正常。",
           (f"{G25} giant page dimension", f"{G25} integer overflow")),
        _m("huge-image-declared", m_huge_image, "guardrail_expected",
           "宣告巨大影像但資料很小：100000x100000 RGB、寬 2^31-1、SOF 宣稱 65535x65535 的 JPEG、300000x300000 ImageMask；最後一頁正常。",
           (f"{G25} max decoded image dimension", f"{G25} max bitmap allocation",
            f"{G25} giant bitmap", f"{G25} integer overflow")),
        _m("decompression-bomb-512mib", m_bomb, "guardrail_expected",
           "Flate bomb：約 0.5 MB 的 stream inflate 後為 512 MiB 的 0（一個 content stream、一個 1000x1000 影像 stream），首末頁正常。",
           (f"{G25} decompression bomb", f"{G25} memory exhaustion"), cost=2),
        _m("decompression-bomb-nested-flate", m_bomb, "guardrail_expected",
           "雙層 /FlateDecode /FlateDecode bomb：數 KB 的 stream 展開為 512 MiB 的 0。",
           (f"{G25} decompression bomb", f"{G25} memory exhaustion"), nested=True, cost=2),
        _m("broken-embedded-fonts", m_broken_fonts, "render_partial_ok",
           "壞掉的嵌入字型：FontFile2／FontFile 為隨機資料、截斷 30% 的 TrueType、Type0 的 W 陣列與 ToUnicode 損壞；最後一頁正常。",
           (f"{G24} broken fonts", f"{G25} malicious font")),
        _m("content-syntax-errors", m_syntax_errors, "render_partial_ok",
           "content stream 語法錯誤：未知運算子、operand 不足、未配對 BT/ET 與 q/Q、未結束字串、錯誤數字、資料不足的 inline image；最後一頁正常。",
           (f"{G24} invalid object", f"{G24} weird encoding")),
    ]
