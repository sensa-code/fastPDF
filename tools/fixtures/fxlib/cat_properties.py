"""Document properties (the Ctrl+D panel): the information dictionary in
every text-string encoding, dates with offsets, tagging, linearization,
form and signature fields, owner-password permissions, and hostile
information dictionaries and field trees.

Written with rawpdf so every byte of the dictionaries is exact; the
encrypted fixture uses pypdf like the other encrypted ones.
"""

from __future__ import annotations

import io
from pathlib import Path
from typing import Any

from .common import Ctx, Job
from .rawpdf import (PRODUCER, Content, Hex, Raw, RawPdf, Ref, add_page, page_tree, pdf_lit,
                     std_font)

A4 = (0, 0, 595.2756, 841.8898)
LETTER_LANDSCAPE = (0, 0, 792, 612)


def _utf16(text: str) -> Hex:
    return Hex(b"\xfe\xff" + text.encode("utf-16-be"))


def _utf8(text: str) -> bytes:
    return b"\xef\xbb\xbf" + text.encode("utf-8")


def _page(lines: list[str]) -> bytes:
    cs = Content()
    y = 760
    for i, line in enumerate(lines):
        size = 22 if i == 0 else 12
        cs(f"BT /F1 {size} Tf 56 {y} Td {pdf_lit(line.encode('latin-1'))} Tj ET")
        y -= 34 if i == 0 else 18
    return cs.data()


def _pages(pdf: RawPdf, specs: list[tuple[tuple, list[str]]]) -> tuple[Ref, list[Ref]]:
    font = pdf.obj(std_font("Helvetica"))
    pages_ref = pdf.alloc()
    kids: list[Ref] = []
    for box, lines in specs:
        add_page(pdf, pages_ref, kids, _page(lines), {"Font": {"F1": font}}, box)
    return pages_ref, kids


def _finish(pdf: RawPdf, pages_ref: Ref, kids: list[Ref], *, id_seed: str,
            info: Any = None, catalog_extra: dict | None = None) -> None:
    page_tree(pdf, pages_ref, kids)
    catalog: dict[str, Any] = {"Type": "Catalog", "Pages": pages_ref}
    if catalog_extra:
        catalog.update(catalog_extra)
    root = pdf.obj(catalog)
    if isinstance(info, dict):
        info = pdf.obj(info)
    pdf.finish(root, info, id_seed=id_seed)


def _widget(pdf: RawPdf, page: Ref, entries: dict) -> Ref:
    """A field merged with its (invisible) widget annotation."""
    d: dict[str, Any] = {"Type": "Annot", "Subtype": "Widget", "Rect": [0, 0, 0, 0], "F": 2,
                         "P": page}
    d.update(entries)
    return pdf.obj(d)


def _signature_value(pdf: RawPdf) -> Ref:
    """A signature dictionary whose /Contents is not a real signature."""
    return pdf.obj({"Type": "Sig", "Filter": "Adobe.PPKLite", "SubFilter": "adbe.pkcs7.detached",
                    "ByteRange": [0, 0, 0, 0], "Contents": Hex(b"\x00" * 64),
                    "M": b"D:20261024153000+08'00'", "Name": b"FastPDF fixture"})


def properties_rich(out: Path, ctx: Ctx) -> dict:
    """Info in three encodings, dates with offsets, tagged, a form with signature fields."""
    with open(out, "wb") as fp:
        pdf = RawPdf(fp)
        pages_ref, kids = _pages(pdf, [
            (A4, ["Properties fixture (A4 portrait)",
                  "Ctrl+D: title in UTF-16BE, author in PDFDocEncoding, keywords in UTF-8,",
                  "dates +08'00' and -00'30', tagged, 6 form fields: 4 signature fields,",
                  "2 of them signed (dummy /V)."]),
            (LETTER_LANDSCAPE, ["Page 2: Letter landscape (the page-size row follows)."]),
        ])
        page = kids[0]
        text = _widget(pdf, page, {"FT": "Tx", "T": b"name", "V": b"FastPDF"})
        unsigned = _widget(pdf, page, {"FT": "Sig", "T": b"approval"})
        signed = _widget(pdf, page, {"FT": "Sig", "T": b"author", "V": _signature_value(pdf)})
        # /FT /Sig inherited by two named kids, one of them signed.
        parent = pdf.alloc()
        kid_a = _widget(pdf, page, {"T": b"a", "Parent": parent, "V": _signature_value(pdf)})
        kid_b = _widget(pdf, page, {"T": b"b", "Parent": parent})
        pdf.obj({"FT": "Sig", "T": b"witnesses", "Kids": [kid_a, kid_b]}, parent)
        # A text field with two widgets (kids without /T): one field.
        shared = pdf.alloc()
        w1 = _widget(pdf, page, {"Parent": shared})
        w2 = _widget(pdf, kids[1], {"Parent": shared})
        pdf.obj({"FT": "Tx", "T": b"date", "Kids": [w1, w2]}, shared)
        struct = pdf.obj({"Type": "StructTreeRoot"})
        info = {
            "Title": _utf16("文件內容測試 Properties fixture"),
            # PDFDocEncoding: 0x8D/0x8E are curly quotes, 0x84 an em dash.
            "Author": b"\x8dFastPDF\x8e fixture generator",
            "Subject": b"PDFDocEncoding \x84 text strings \x80 dates",
            "Keywords": _utf8("屬性, properties, 測試"),
            "Creator": b"tools/fixtures/generate.py",
            "Producer": PRODUCER,
            "CreationDate": b"D:20261024153000+08'00'",
            "ModDate": b"D:20261025091500-00'30'",
        }
        _finish(pdf, pages_ref, kids, id_seed="properties-rich", info=info, catalog_extra={
            "MarkInfo": {"Marked": True}, "StructTreeRoot": struct, "Lang": b"zh-TW",
            "AcroForm": {"Fields": [text, unsigned, signed, parent, shared], "SigFlags": 3},
        })
    return {"pages": 2}


def linearized_flag(out: Path, ctx: Ctx) -> dict:
    """A linearization dictionary first, whose /L is the file length (two passes)."""
    placeholder = b"7777777777"
    buf = io.BytesIO()
    pdf = RawPdf(buf)
    pdf.obj({"Linearized": 1, "L": Raw(placeholder), "H": [0, 0], "O": 5, "E": 0, "N": 1,
             "T": 0})
    pages_ref, kids = _pages(pdf, [(A4, [
        "Linearization flag fixture",
        "The first object is a linearization dictionary whose /L is this file's length;",
        "the hint tables are fake. Created has no D: prefix, Modified ends in Z."])])
    info = {"Title": b"Linearized flag", "Producer": PRODUCER,
            "CreationDate": b"20200101", "ModDate": b"D:20261024153000Z"}
    _finish(pdf, pages_ref, kids, id_seed="linearized-flag", info=info)
    data = buf.getvalue()
    length = b"%010d" % len(data)
    assert data.count(placeholder) == 1 and len(length) == len(placeholder)
    out.write_bytes(data.replace(placeholder, length))
    return {"pages": 1}


def encrypted_permissions(out: Path, ctx: Ctx, owner: str) -> dict:
    """RC4-128 with an owner password: low-resolution print, forms, accessibility, assembly."""
    from pypdf import PdfReader, PdfWriter
    from pypdf.constants import UserAccessPermissions as P

    from .cat_encrypted import _base_document, deterministic_secrets

    perms = P.PRINT | P.FILL_FORM_FIELDS | P.EXTRACT_TEXT_AND_GRAPHICS | P.ASSEMBLE_DOC
    with deterministic_secrets("properties-permissions"):
        writer = PdfWriter(clone_from=PdfReader(io.BytesIO(_base_document())))
        writer.encrypt(user_password="", owner_password=owner, algorithm="RC4-128",
                       permissions_flag=perms)
        with open(out, "wb") as f:
            writer.write(f)
    return {"pages": 3, "encryption": {
        "handler": "Standard", "algorithm": "RC4-128", "V": 2, "R": 3, "key_bits": 128,
        "user_password": "", "owner_password": owner,
        "permissions": "print-low-res,fill-forms,accessibility,assemble"}}


def info_hostile_strings(out: Path, ctx: Ctx) -> dict:
    """Huge, control-laden, bidi and malformed text strings and dates."""
    with open(out, "wb") as fp:
        pdf = RawPdf(fp)
        pages_ref, kids = _pages(pdf, [(A4, [
            "Hostile information dictionary",
            "1 MiB title, controls and a right-to-left override in the author,",
            "odd-length and lone-surrogate UTF-16, invalid UTF-8, language escapes,",
            "an impossible date and a 10,000-character date. Nothing may break."])])
        title = (b"Huge title " + b"\x80-" * 8) + b"A" * (1 << 20)
        author = _utf16("evil‮exe.txt\n\x00\tname​⁦x⁩")
        info = {
            "Title": title,
            "Author": author,
            "Subject": Hex(b"\xfe\xff\x4e\x2d\x65"),  # odd length
            "Keywords": Hex(b"\xfe\xff\xd8\x3d\x00\x41"),  # lone high surrogate
            "Creator": Hex(b"\xef\xbb\xbfok \xff\xfe bad \xe6\x97"),  # invalid UTF-8
            "Producer": Hex(b"\xfe\xff\x00\x1bzhTW\x00\x1b" + "產生器".encode("utf-16-be")),
            "CreationDate": b"D:2026-13-45",
            "ModDate": b"D:" + b"2" * 10_000,
        }
        _finish(pdf, pages_ref, kids, id_seed="info-hostile-strings", info=info)
    return {"pages": 1}


def info_odd(out: Path, ctx: Ctx, kind: str) -> dict:
    """/Info as a stream, as a number, or missing."""
    with open(out, "wb") as fp:
        pdf = RawPdf(fp)
        pages_ref, kids = _pages(pdf, [(A4, [f"/Info: {kind}",
                                             "The properties panel must open and say less."])])
        if kind == "stream":
            info: Any = pdf.stream(b"not an information dictionary",
                                   {"Title": b"Info as a stream"}, compress=False)
        elif kind == "number":
            info = pdf.raw_obj(b"42")
        else:
            info = None
        _finish(pdf, pages_ref, kids, id_seed=f"info-{kind}", info=info)
    return {"pages": 1}


def acroform_kids_cycle(out: Path, ctx: Ctx) -> dict:
    """A field tree with cycles, a 200-deep chain and items of every wrong kind."""
    with open(out, "wb") as fp:
        pdf = RawPdf(fp)
        pages_ref, kids = _pages(pdf, [(A4, [
            "Hostile form field tree",
            "Kids cycles, a field that is its own kid, a 200-deep chain, null, numbers,",
            "missing objects and an unreadable item. Counting must stop."])])
        a, b, c = pdf.alloc(), pdf.alloc(), pdf.alloc()
        pdf.obj({"FT": "Sig", "T": b"a", "Kids": [b, a]}, a)
        pdf.obj({"T": b"b", "Kids": [a, c]}, b)
        pdf.obj({"T": b"c", "Kids": [b], "V": Ref(999_999)}, c)
        chain = [pdf.alloc() for _ in range(200)]
        for depth, ref in enumerate(chain):
            node: dict[str, Any] = {"T": b"level %d" % depth}
            if depth + 1 < len(chain):
                node["Kids"] = [chain[depth + 1]]
            else:
                node["FT"] = "Tx"
            pdf.obj(node, ref)
        number = pdf.raw_obj(b"7")
        fields = [a, chain[0], None, 12, number, Ref(888_888), b,
                  Raw(b"<< /F 1 >> stream\nabc\nendstream"), c]
        _finish(pdf, pages_ref, kids, id_seed="acroform-kids-cycle",
                info={"Title": b"Hostile form field tree", "Producer": PRODUCER},
                catalog_extra={"AcroForm": {"Fields": fields}})
    return {"pages": 1}


def acroform_many_fields(out: Path, ctx: Ctx) -> dict:
    """25,000 terminal fields, more than a reader counts."""
    count = 25_000
    with open(out, "wb") as fp:
        pdf = RawPdf(fp)
        pages_ref, kids = _pages(pdf, [(A4, [
            "Many form fields", f"{count} text fields; a reader counts 20,000 at most."])])
        fields = [pdf.obj({"FT": "Tx", "T": b"f%d" % i}) for i in range(count)]
        _finish(pdf, pages_ref, kids, id_seed="acroform-many-fields",
                info={"Title": b"Many form fields", "Producer": PRODUCER},
                catalog_extra={"AcroForm": {"Fields": fields}})
    return {"pages": 1, "fields": count}


def jobs() -> list[Job]:
    guardrails = ("§25 infinite recursion", "§25 malformed object tree", "§25 memory exhaustion")
    malformed = ("malformed", "document-properties")
    return [
        Job("small-text/properties-rich.pdf", properties_rich, "open_ok",
            "2 頁（A4 直式、Letter 橫式）。/Info 標題 UTF-16BE、作者與主旨 PDFDocEncoding、關鍵字 UTF-8 BOM；"
            "建立日期 +08'00'、修改日期 -00'30'；/MarkInfo /Marked true；AcroForm 6 個欄位"
            "（4 個簽章欄位，2 個有假的 /V，其中 2 個繼承 /FT /Sig）。",
            pages=2, features=("document-properties", "tagged", "acroform", "cjk-text-string"),
            cost=0.1),
        Job("small-text/linearized-flag.pdf", linearized_flag, "open_ok",
            "第一個物件是線性化字典，/L 等於檔案長度（兩階段寫入，hint 表是假的）；"
            "建立日期沒有 D: 前綴，修改日期以 Z 結尾。",
            pages=1, features=("document-properties", "linearized"), cost=0.1),
        Job("encrypted/rc4-128-owner-perms.pdf", encrypted_permissions, "open_ok",
            "RC4 128-bit（V2／R3），空 user password、有 owner password；權限：低解析度列印、"
            "填寫表單、協助工具複製、頁面組合，不允許複製、變更與註解。",
            pages=3, features=("Encrypt", "RC4-128", "owner-only", "document-properties"),
            kwargs={"owner": "fastpdf-owner-perms"}, producer="pypdf", cost=0.4),
        Job("malformed/info-hostile-strings.pdf", info_hostile_strings, "open_ok",
            "惡意 /Info：1 MiB 標題、含控制字元與 RLO 的作者、奇數長度與孤立代理的 UTF-16、"
            "無效 UTF-8、語言跳脫碼、月份 13 的日期、1 萬字元的日期。必須正常開檔，內容有界。",
            pages=1, features=malformed, guardrails=guardrails, cost=0.2),
        Job("malformed/info-stream.pdf", info_odd, "open_ok",
            "/Info 指向串流物件（字典裡有 /Title）。必須正常開檔。",
            pages=1, features=malformed, kwargs={"kind": "stream"}, cost=0.1),
        Job("malformed/info-wrong-type.pdf", info_odd, "open_ok",
            "/Info 指向數字物件。必須正常開檔，文件內容沒有描述欄位。",
            pages=1, features=malformed, kwargs={"kind": "number"}, cost=0.1),
        Job("malformed/info-missing.pdf", info_odd, "open_ok",
            "trailer 沒有 /Info。必須正常開檔，文件內容沒有描述欄位。",
            pages=1, features=malformed, kwargs={"kind": "missing"}, cost=0.1),
        Job("malformed/acroform-kids-cycle.pdf", acroform_kids_cycle, "open_ok",
            "惡意 AcroForm 欄位樹：/Kids 循環、自己是自己的 kid、200 層鏈、null、數字、"
            "不存在的物件與讀不了的項目。計數必須停止且標示為下限。",
            pages=1, features=malformed, guardrails=guardrails, cost=0.1),
        Job("malformed/acroform-many-fields.pdf", acroform_many_fields, "open_ok",
            "25,000 個文字欄位，超過閱讀器計數的上限 20,000（顯示「≥ 20000」）。",
            pages=1, features=malformed, guardrails=("§25 memory exhaustion",), cost=0.3),
    ]
