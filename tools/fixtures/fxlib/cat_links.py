"""Link annotations: every kind of target a reader meets, and a hostile flood.

``small-text/links-actions.pdf`` puts each link in a labelled box at a
documented page-space position (points from the top-left corner of the
unrotated page, as dev scripts address them), so GUI checks can hover and
click them: internal jumps (explicit, named through /Names and /Dests),
http/https/mailto links, links FastPDF must never follow (file:,
javascript:, data:, Launch, GoToR, JavaScript) and the URI policy's corner
cases (IDN host, credentials, quotes, bidi characters, an over-long
address). See fixtures/README.md for the table of boxes.

``malformed/links-flood.pdf`` is a page with 12,000 link annotations over a
deep, cyclic, flat name tree, 10 KiB URIs and duplicated references; reading
its links must stay bounded and fast.
"""

from __future__ import annotations

from pathlib import Path
from typing import Any

from .common import Job
from .rawpdf import Content, Raw, RawPdf, Ref, pdf_lit, simple_document, std_font

LETTER = (0, 0, 612, 792)
PAGE_HEIGHT = 792
# Box geometry in page space (top-left origin, y down).
LEFT_X, RIGHT_X, BOX_W, BOX_H = 72, 322, 218, 36

# (column x, centre y, label, target) for page 1; targets are built below.
PAGE1_BOXES: list[tuple[int, int, str, str]] = [
    (LEFT_X, 100, "GoTo page 3 (XYZ)", "goto-p3"),
    (LEFT_X, 160, "https query+fragment", "https"),
    (LEFT_X, 220, "IDN host (buecher)", "idn"),
    (LEFT_X, 280, "file: URI", "file"),
    (LEFT_X, 340, "javascript: URI", "javascript-uri"),
    (LEFT_X, 400, "Launch action", "launch"),
    (LEFT_X, 460, "GoToR action", "gotor"),
    (LEFT_X, 520, "JavaScript action", "js-action"),
    (LEFT_X, 580, "mailto", "mailto"),
    (LEFT_X, 640, "Named via /Names (p2)", "named-tree"),
    (LEFT_X, 700, "Named via /Dests (p3)", "named-dests"),
    (RIGHT_X, 100, "Hidden link (/F 2)", "hidden"),
    (RIGHT_X, 160, "Overlap: top -> p2", "overlap"),
    (RIGHT_X, 220, "Inverted /Rect -> p2", "inverted"),
    (RIGHT_X, 280, "Credentials in URI", "credentials"),
    (RIGHT_X, 340, "Quote in URI", "quote"),
    (RIGHT_X, 400, "Missing named dest", "missing-named"),
    (RIGHT_X, 460, "www. without scheme", "schemeless"),
    (RIGHT_X, 520, "Bidi override in URI", "bidi"),
    (RIGHT_X, 580, "3000-byte URI", "long"),
    (RIGHT_X, 640, "NoView link (/F 32)", "noview"),
    (RIGHT_X, 700, "data: URI", "data"),
]

URIS = {
    "https": b"https://example.invalid/path?q=1#frag",
    "idn": "https://b\u00fccher.example/".encode("utf-8"),
    "file": b"file:///C:/Windows/System32/calc.exe",
    "javascript-uri": b"javascript:alert(1)",
    "mailto": b"mailto:someone@example.invalid?subject=FastPDF%20test",
    "credentials": b"https://user:secret@example.invalid/",
    "quote": b'https://example.invalid/a"b',
    "schemeless": b"www.example.invalid",
    "bidi": "https://example.invalid/\u202egnp.exe".encode("utf-8"),
    "long": b"https://example.invalid/" + b"a" * (3000 - 24),
    "data": b"data:text/html,<script>alert(1)</script>",
}


def _box(x: int, y: int) -> tuple[float, float, float, float]:
    """A box centred on page-space row y as a user-space /Rect."""
    top, bottom = y - BOX_H / 2, y + BOX_H / 2
    return (x, PAGE_HEIGHT - bottom, x + BOX_W, PAGE_HEIGHT - top)


def _draw_box(cs: Content, x: int, y: int, label: str) -> None:
    x0, y0, x1, y1 = _box(x, y)
    cs(f"0.2 0.4 0.8 RG 1 w {x0} {y0} {x1 - x0} {y1 - y0} re S")
    cs(f"BT /F1 12 Tf 0 0 0 rg {x0 + 6} {y0 + 13} Td {pdf_lit(label.encode('latin-1'))} Tj ET")


def _page_text(title: str, lines: list[str]) -> Content:
    cs = Content()
    cs(f"BT /F1 18 Tf 72 {PAGE_HEIGHT - 50} Td {pdf_lit(title.encode('latin-1'))} Tj ET")
    for i, line in enumerate(lines):
        cs(f"BT /F1 11 Tf 72 {PAGE_HEIGHT - 70 - 14 * i} Td {pdf_lit(line.encode('latin-1'))} Tj ET")
    return cs


def _link(rect: tuple[float, ...], target: dict[str, Any]) -> dict[str, Any]:
    d: dict[str, Any] = {"Type": "Annot", "Subtype": "Link", "Rect": list(rect),
                         "Border": [0, 0, 0]}
    d.update(target)
    return d


def links_actions(out: Path, ctx) -> dict:
    """Three Letter pages; page 1 holds the labelled link boxes."""
    with open(out, "wb") as fp:
        pdf = RawPdf(fp)
        font = pdf.obj(std_font("Helvetica"))
        resources = {"Font": {"F1": font}}
        pages_ref = pdf.alloc()
        page_refs = [pdf.alloc() for _ in range(3)]
        p1, p2, p3 = page_refs

        targets: dict[str, dict[str, Any]] = {
            "goto-p3": {"Dest": [p3, "XYZ", 0, PAGE_HEIGHT, 0]},
            "launch": {"A": {"S": "Launch", "F": b"calc.exe"}},
            "gotor": {"A": {"S": "GoToR", "F": b"other.pdf", "D": [0, "Fit"]}},
            "js-action": {"A": {"S": "JavaScript", "JS": b"app.alert(1)"}},
            "named-tree": {"Dest": b"chapter-2"},
            "named-dests": {"Dest": "appendix"},
            "hidden": {"F": 2, "Dest": [p2, "Fit"]},
            "inverted": {"Dest": [p2, "Fit"]},
            "missing-named": {"Dest": b"nowhere"},
            "noview": {"F": 32, "Dest": [p2, "Fit"]},
        }
        for key, uri in URIS.items():
            targets[key] = {"A": {"S": "URI", "URI": uri}}

        cs = _page_text("FastPDF fixture: link annotations",
                        ["Each box is one link; positions are in fixtures/README.md.",
                         "Blue boxes are clickable areas; hidden ones must do nothing."])
        annots: list[Ref] = []
        for x, y, label, key in PAGE1_BOXES:
            _draw_box(cs, x, y, label)
            rect = _box(x, y)
            if key == "inverted":
                rect = (rect[2], rect[3], rect[0], rect[1])
            if key == "overlap":
                # Under: page 3; on top (later in /Annots): page 2, on the right half.
                annots.append(pdf.obj(_link(rect, {"Dest": [p3, "Fit"]})))
                half = (rect[0] + BOX_W / 2, rect[1], rect[2], rect[3])
                annots.append(pdf.obj(_link(half, {"Dest": [p2, "Fit"]})))
                continue
            annots.append(pdf.obj(_link(rect, targets[key])))

        page1 = {"Type": "Page", "Parent": pages_ref, "MediaBox": list(LETTER),
                 "Resources": resources, "Contents": pdf.stream(cs.data()), "Annots": annots}
        pdf.obj(page1, p1)

        cs2 = _page_text("Page 2: target of chapter-2", ["The box below goes back to page 1 (Fit)."])
        _draw_box(cs2, LEFT_X, 100, "Back to page 1 (Fit)")
        back2 = pdf.obj(_link(_box(LEFT_X, 100), {"Dest": [p1, "Fit"]}))
        pdf.obj({"Type": "Page", "Parent": pages_ref, "MediaBox": list(LETTER),
                 "Resources": resources, "Contents": pdf.stream(cs2.data()),
                 "Annots": [back2]}, p2)

        cs3 = _page_text("Page 3 (/Rotate 90): appendix",
                         ["Page space is unrotated: the back link is at (100, 100)."])
        _draw_box(cs3, LEFT_X, 100, "Back to page 1 (rotated)")
        back3 = pdf.obj(_link(_box(LEFT_X, 100), {"A": {"S": "GoTo", "D": [p1, "Fit"]}}))
        pdf.obj({"Type": "Page", "Parent": pages_ref, "MediaBox": list(LETTER), "Rotate": 90,
                 "Resources": resources, "Contents": pdf.stream(cs3.data()),
                 "Annots": [back3]}, p3)

        tree = pdf.obj({"Names": [b"chapter-2", [p2, "XYZ", 72, PAGE_HEIGHT - 40, 0]]})
        dests = pdf.obj({"appendix": [p3, "FitH", PAGE_HEIGHT]})
        simple_document(pdf, page_refs, pages_ref, title="FastPDF fixture: link annotations",
                        id_seed="links-actions",
                        catalog_extra={"Names": {"Dests": tree}, "Dests": dests})
    return {"pages": 3}


def links_flood(out: Path, ctx) -> dict:
    """12,000 links over a hostile name tree, long URIs, duplicates."""
    count = 12_000
    with open(out, "wb") as fp:
        pdf = RawPdf(fp)
        font = pdf.obj(std_font("Helvetica"))
        pages_ref = pdf.alloc()
        page_ref = pdf.alloc()

        # Name tree: a cycle back to the root, a 200-level chain, and a flat
        # leaf of 50,000 names without /Limits (none of the names looked up).
        root = pdf.alloc()
        cycle = pdf.alloc()
        pdf.obj({"Kids": [root, cycle]}, cycle)
        chain = [pdf.alloc() for _ in range(200)]
        for depth, ref in enumerate(chain):
            node: dict[str, Any] = ({"Kids": [chain[depth + 1]]} if depth + 1 < len(chain)
                                    else {"Names": [b"deep", [page_ref, "Fit"]]})
            pdf.obj(node, ref)
        flat = b" ".join(b"(n%05d) [%d 0 R /Fit]" % (i, page_ref.num) for i in range(50_000))
        leaf = pdf.raw_obj(b"<</Names [" + flat + b"]>>")
        pdf.obj({"Kids": [cycle, chain[0], leaf]}, root)

        long_uri = pdf.obj(b"https://example.invalid/" + b"x" * 10_240)
        # 1e30 written the PDF way (no exponents in PDF numbers).
        huge, tiny = Raw(b"1" + b"0" * 30 + b".0"), Raw(b"-1" + b"0" * 30 + b".0")
        shared_action = pdf.obj({"S": "Launch", "F": b"calc.exe"})
        annots: list[Any] = []
        for i in range(count):
            x, y = 20 + (i % 100) * 5.7, 40 + (i // 100) * 6
            rect: list[Any] = [x, y, x + 5, y + 5]
            kind = i % 6
            if kind == 0:
                target: dict[str, Any] = {"Dest": b"missing-%05d" % i}
            elif kind == 1:
                target = {"A": {"S": "URI", "URI": long_uri}}
            elif kind == 2:
                target = {"A": shared_action}
            elif kind == 3:
                target = {"Dest": b"deep", "F": 2 if i % 12 == 3 else 4}
            elif kind == 4:
                target = {"Dest": [page_ref, "XYZ", huge, tiny, 0]}
                rect = [x + 5, y + 5, x, y]
            else:
                target = {"A": {"S": "URI", "URI": b"https://example.invalid/%d" % i}}
                rect = [tiny, tiny, huge, huge] if i % 600 == 5 else rect
            annots.append(pdf.obj(_link(tuple(rect), target)))
        # The first 2,000 annotations are listed twice; the second listing
        # changes nothing.
        annots = annots + annots[:2_000]
        cs = _page_text("FastPDF malformed fixture: link flood",
                        ["12,000 tiny links; reading them must stay bounded."])
        pdf.obj({"Type": "Page", "Parent": pages_ref, "MediaBox": list(LETTER),
                 "Resources": {"Font": {"F1": font}}, "Contents": pdf.stream(cs.data()),
                 "Annots": annots}, page_ref)
        simple_document(pdf, [page_ref], pages_ref, title="FastPDF malformed fixture: links",
                        id_seed="links-flood", catalog_extra={"Names": {"Dests": root}})
    return {"pages": 1}


def jobs() -> list[Job]:
    guardrails = ("§25 infinite recursion", "§25 malformed object tree", "§25 memory exhaustion")
    return [
        Job("small-text/links-actions.pdf", links_actions, "open_ok",
            "3 頁，第 1 頁有 22 個標示框的連結（座標見 fixtures/README.md）：GoTo XYZ、/Names 與 /Dests "
            "的具名目的地、https（query＋fragment）、mailto、IDN 主機、file:、javascript:、data:、Launch、"
            "GoToR、JavaScript action、隱藏（/F 2、/F 32）、重疊、反向 /Rect、帳密、引號、bidi、3000 bytes 網址、"
            "不存在的具名目的地；第 3 頁 /Rotate 90 有回第 1 頁的連結。",
            pages=3, features=("link-annotations", "uri-links", "named-destinations"), cost=0.1),
        Job("malformed/links-flood.pdf", links_flood, "open_ok",
            "惡意連結：一頁 12,000 個 link annotation（另有 2,000 個重複參照）、名稱樹有循環、200 層鏈與 "
            "5 萬個名稱的扁平 leaf（無 /Limits）、10 KiB 網址、共用的 Launch action、1e30 的座標。讀取連結必須有界且快速。",
            pages=1, features=("malformed", "link-annotations"), guardrails=guardrails, cost=0.3),
    ]
