"""Page labels (/PageLabels number tree, ISO 32000-2 §12.4.2).

Every page prints its physical number and the label a reader should show,
so screenshots and text layers prove where a label jump landed. Written
with rawpdf so the number trees are exact, including the hostile one.
"""

from __future__ import annotations

from pathlib import Path
from typing import Any

from .common import Ctx, Job
from .rawpdf import Content, Hex, Raw, RawPdf, Ref, add_page, pdf_lit, simple_document, std_font

LETTER = (0, 0, 612, 792)


def _utf16(text: str) -> Hex:
    return Hex(b"\xfe\xff" + text.encode("utf-16-be"))


def _page(number: int, count: int, label: str, note: str = "") -> bytes:
    cs = Content()
    cs("BT /F1 40 Tf 72 640 Td")
    cs(f"{pdf_lit(f'Label: {label}'.encode('latin-1'))} Tj ET")
    cs("BT /F1 22 Tf 72 580 Td")
    cs(f"{pdf_lit(f'Physical page {number} of {count}'.encode('latin-1'))} Tj ET")
    if note:
        cs("BT /F1 14 Tf 72 540 Td")
        cs(f"{pdf_lit(note.encode('latin-1'))} Tj ET")
    return cs.data()


def _pages(pdf: RawPdf, labels: list[str],
           notes: dict[int, str] | None = None) -> tuple[Ref, list[Ref]]:
    font = pdf.obj(std_font("Helvetica"))
    pages_ref = pdf.alloc()
    kids: list[Ref] = []
    for i, label in enumerate(labels):
        add_page(pdf, pages_ref, kids, _page(i + 1, len(labels), label, (notes or {}).get(i, "")),
                 {"Font": {"F1": font}}, LETTER)
    return pages_ref, kids


def labels_mixed(out: Path, ctx: Ctx) -> dict:
    """i, ii, iii, 1, 2, 3, App-A, App-B from one /Nums array."""
    labels = ["i", "ii", "iii", "1", "2", "3", "App-A", "App-B"]
    with open(out, "wb") as fp:
        pdf = RawPdf(fp)
        pages_ref, kids = _pages(pdf, labels)
        nums: list[Any] = [0, {"S": "r"}, 3, {"S": "D"}, 6, {"S": "A", "P": b"App-"}]
        simple_document(pdf, kids, pages_ref, title="FastPDF fixture: page labels (mixed)",
                        id_seed="page-labels-mixed",
                        catalog_extra={"PageLabels": {"Nums": nums}})
    return {"pages": len(labels)}


def labels_tree_cjk(out: Path, ctx: Ctx) -> dict:
    """A /Kids tree with /Limits, every style, a CJK prefix and a duplicate label."""
    appendix = "附錄-"  # 附錄-
    labels = ["Cover", "I", "II", "III", "a", "b", "1", "2",
              appendix + "5", appendix + "6", "1", "2"]
    notes = {
        0: "Prefix-only range: every page of it would read Cover.",
        8: "Label prefix is CJK (U+9644 U+9304 '-') with /St 5.",
        10: "Second part restarts at 1: label 1 also names physical page 7.",
    }
    shown = [label if label.isascii() else label.replace(appendix, "<CJK appendix>-")
             for label in labels]
    with open(out, "wb") as fp:
        pdf = RawPdf(fp)
        pages_ref, kids = _pages(pdf, shown, notes)
        restart = pdf.obj({"Type": "PageLabel", "S": "D"})
        leaf_a = pdf.obj({"Limits": [0, 5],
                          "Nums": [0, {"P": b"Cover"}, 1, {"S": "R"}, 4, {"S": "a"}]})
        leaf_c = pdf.obj({"Limits": [6, 9],
                          "Nums": [6, {"S": "D"}, 8, {"S": "D", "P": _utf16(appendix), "St": 5}]})
        leaf_d = pdf.obj({"Limits": [10, 11], "Nums": [10, restart]})
        mid_b = pdf.obj({"Limits": [6, 11], "Kids": [leaf_c, leaf_d]})
        root = pdf.obj({"Kids": [leaf_a, mid_b]})
        simple_document(pdf, kids, pages_ref, title="FastPDF fixture: page labels (tree, CJK)",
                        id_seed="page-labels-tree-cjk", catalog_extra={"PageLabels": root})
    return {"pages": len(labels)}


def labels_hostile(out: Path, ctx: Ctx) -> dict:
    """A number tree with a cycle, a deep chain, too many pairs and bad entries."""
    page_count = 3
    with open(out, "wb") as fp:
        pdf = RawPdf(fp)
        pages_ref, kids = _pages(pdf, ["(bounded)"] * page_count,
                                 {0: "Hostile /PageLabels: cycle, 200-deep chain, "
                                     "140,000 pairs, bad keys, 20,000-char prefix."})
        root = pdf.alloc()
        cycle = pdf.alloc()
        # 20,000 characters with line feeds, right-to-left overrides and
        # zero-width spaces.
        noisy = "".join("\n" if i % 37 == 0 else "‮" if i % 41 == 0 else
                        "​" if i % 43 == 0 else "x" for i in range(20_000))
        pdf.obj({"Kids": [cycle, root], "Nums": [0, {"S": "r", "P": _utf16(noisy)}]}, cycle)
        # 200 levels; only the deepest node has entries.
        chain = [pdf.alloc() for _ in range(200)]
        for depth, ref in enumerate(chain):
            node: dict[str, Any] = ({"Kids": [chain[depth + 1]]} if depth + 1 < len(chain)
                                    else {"Nums": [0, {"S": "A"}]})
            pdf.obj(node, ref)
        shared = pdf.obj({"S": "D"})
        pairs = b" ".join(b"%d %d 0 R" % (i % 5, shared.num) for i in range(140_000))
        big = pdf.raw_obj(b"<</Nums [" + pairs + b"]>>")
        bad: list[Any] = [-1, {"S": "D"}, Raw(b"2.5"), {"S": "D"}, 4294967296, {"S": "D"},
                          b"1", {"S": "D"}, 1, None, 2, Ref(999_999),
                          1, {"S": "R", "St": 99999999999},
                          2, {"S": "Q", "P": b"Q-", "St": -7}]
        pdf.obj({"Kids": [cycle, chain[0], big], "Nums": bad}, root)
        simple_document(pdf, kids, pages_ref, title="FastPDF malformed fixture: page labels",
                        id_seed="page-labels-hostile", catalog_extra={"PageLabels": root})
    return {"pages": page_count}


def jobs() -> list[Job]:
    guardrails = ("§25 infinite recursion", "§25 malformed object tree", "§25 memory exhaustion")
    return [
        Job("small-text/page-labels-mixed.pdf", labels_mixed, "open_ok",
            "8 頁，頁面標籤 i、ii、iii、1、2、3、App-A、App-B（/Nums 陣列：/S /r、/S /D、/S /A＋/P）；"
            "每頁印出實體頁碼與應顯示的標籤。",
            pages=8, features=("page-labels",), cost=0.1),
        Job("small-text/page-labels-tree-cjk.pdf", labels_tree_cjk, "open_ok",
            "12 頁，/Kids number tree（含 /Limits 與間接 value）：只有前綴的 Cover、I–III、a–b、1–2、"
            "UTF-16BE 前綴「附錄-」＋/St 5，最後一段重新從 1 起算（重複標籤）。",
            pages=12, features=("page-labels", "cjk-text-string"), cost=0.1),
        Job("malformed/page-labels-hostile.pdf", labels_hostile, "open_ok",
            "惡意 /PageLabels：/Kids 循環、200 層鏈、14 萬組 key/value、錯誤的 key（負數、小數、"
            "超大、字串）、null 與不存在的 value、/St 負值與超大值、未知 /S、2 萬字含控制字元與 bidi 的 /P。"
            "必須正常開檔，標籤有界。",
            pages=3, features=("malformed", "page-labels"), guardrails=guardrails, cost=0.2),
    ]
