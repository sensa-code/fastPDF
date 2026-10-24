"""A document for the find options: full-width forms, match case, whole words.

Page 1 holds the traps (full-width ＰＤＦ and digits, words inside words, a
full-width code broken over two lines); pages 2-40 repeat one line thirty
times, so the result list has thousands of rows to scroll and anchor. The
counts a check compares against are in COUNTS and in the manifest
description. Painter-agnostic (see painter.py); the corpus writes it with a
Type0 / Identity-H font whose ToUnicode maps full-width forms as they are.
"""

from __future__ import annotations

from typing import Any

TITLE = "搜尋選項測試（全形、大小寫、全字）"
NOTE = "【測試樣本】本文件內容全部為虛構，僅供測試使用。"
PAGE1 = [
    "本系統支援ＰＤＦ檔案，也支援 PDF 與 pdf 兩種寫法。",
    "依第１２條及第112條規定辦理。",
    "FastPDF reads PDFs; PDF-A is a PDF profile.",
    "認證編號：ＩＳＯ",
    "９００１（跨行測試）",
    "ＡＢＣ　ＤＥＦ / ABC DEF",
    "research, searching, search-engine, (search)",
]
REPEAT = "search Search SEARCH 搜尋結果列表測試"
REPEAT_LINES = 30
PAGES = 40

# query -> (default, match case, whole word) hit counts over the document.
COUNTS = {
    "pdf": (7, 1, 5),
    "PDF": (7, 6, 5),
    "ＰＤＦ": (7, 6, 5),
    "12": (2, 2, 1),
    "ISO": (1, 1, 1),
    "ISO9001": (1, 1, 1),
    "ABCDEF": (1, 1, 1),
    "ABC": (2, 2, 2),
    "ABC DEF": (1, 1, 1),
    "search": (3514, 1174, 3512),
    "搜尋結果": (1170, 1170, 1170),
}

GRAY = (0.45, 0.45, 0.45)


def text() -> str:
    """Every character the document draws (to check the font has them)."""
    return TITLE + NOTE + "".join(PAGE1) + REPEAT


def search_options(p: Any) -> None:
    H = p.page_h
    left = 60.0
    p.text(left, H - 70, TITLE, 18, "title")
    p.text(left, H - 95, NOTE, 9, "body", GRAY)
    y = H - 140
    for line in PAGE1:
        p.text(left, y, line, 14)
        y -= 26
    for _ in range(2, PAGES + 1):
        p.new_page()
        y = H - 70
        for _ in range(REPEAT_LINES):
            p.text(left, y, REPEAT, 12)
            y -= 22


def description() -> str:
    def row(query: str) -> str:
        default, case, word = COUNTS[query]
        return f"「{query}」{default}／{case}／{word}"

    return ("搜尋選項：Type0／Identity-H＋ToUnicode（全形字元照原碼對應）。第 1 頁：全形ＰＤＦ與半形 "
            "PDF、pdf，第１２條與第112條，FastPDF、PDFs、PDF-A，跨行的全形ＩＳＯ／９００１，"
            "ＡＢＣ　ＤＥＦ與 ABC DEF，research／searching／search-engine／(search)；第 2–40 頁每頁 "
            f"30 行「{REPEAT}」。預期筆數（預設／大小寫須相符／全字）："
            + "、".join(row(q) for q in COUNTS) + "。")
