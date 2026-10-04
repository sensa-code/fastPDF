"""Categories: cjk, japanese, traditional-chinese."""

from __future__ import annotations

from pathlib import Path
from typing import Any

from .cat_raster import _g4_encoder, render_raster_pages, write_scanned_pdf
from .common import (JA_PHRASES, KO_PHRASES, SC_PHRASES, TC_PHRASES, Ctx, Job, SkipFixture,
                     cjk_paragraph, rng, wrap)
from .docs_tw import Pager, gov_letter, lab_report
from .fonts import Type0Font
from .imaging import jpeg, scan_effects
from .painter import A4, RawPainter, ReportLabPainter
from .rawpdf import RawPdf
from .rl import rl_canvas, stringWidth

TC_KAI_FIRST = ("tc_kai", "tc_ming", "tc_sans")
TC_SANS_FIRST = ("tc_sans", "tc_ming", "tc_kai")
TC_MING_FIRST = ("tc_ming", "tc_kai", "tc_sans")
JA_GOTHIC_FIRST = ("ja_gothic", "ja_msgothic")
JA_MSGOTHIC_FIRST = ("ja_msgothic", "ja_gothic")


def pick_font(ctx: Ctx, roles: tuple[str, ...]) -> dict[str, Any]:
    for role in roles:
        if ctx.fonts.get(role):
            return ctx.fonts[role]
    raise SkipFixture(f"no system font found for roles {list(roles)}")


def font_inputs(info: dict[str, Any]) -> dict[str, Any]:
    return {"system_font": {k: info[k] for k in ("role", "file", "index", "family",
                                                 "postscript_name", "sha256")}}


def cid_font(face: str, vertical: bool = False, *, reportlab_legacy_cmap: bool = False) -> str:
    """Register a non-embedded Adobe CID font (reportlab UnicodeCIDFont).

    reportlab maps MSung-Light (Adobe-CNS1) to the UniGB-UCS2 CMap, i.e. a CMap
    of the wrong character collection. We patch it to UniCNS-UCS2 unless the
    legacy (mismatched) output is explicitly requested.
    """
    from reportlab.pdfbase import cidfonts, pdfmetrics

    table = cidfonts.defaultUnicodeEncodings
    original = table[face]
    if face == "MSung-Light" and not reportlab_legacy_cmap:
        table[face] = ("cht", "UniCNS-UCS2-H")
    try:
        f = cidfonts.UnicodeCIDFont(face, isVertical=vertical)
    finally:
        table[face] = original
    f.name = f.fontName = (face + ("-V" if vertical else "")
                           + ("-LegacyUniGB" if reportlab_legacy_cmap else ""))
    pdfmetrics.registerFont(f)
    return f.fontName


def ttf_font(info: dict[str, Any], name: str) -> str:
    from reportlab.pdfbase import pdfmetrics
    from reportlab.pdfbase.ttfonts import TTFont

    pdfmetrics.registerFont(TTFont(name, info["path"], subfontIndex=info["index"]))
    return name


def paragraphs_layout(p: Any, title: str, paragraphs: list[str], *, size: float = 11,
                      leading: float = 18, note: str = "") -> None:
    W, H = p.page_w, p.page_h
    L = 60.0

    def decorate(n: int) -> None:
        if note:
            p.text(L, H - 36, note, 8, "body", (0.45, 0.45, 0.45))
        foot = f"- {n} -"
        p.text((W - p.width(foot, 9)) / 2, 30, foot, 9)

    pg = Pager(p, H - 60, 56, decorate)
    y = H - 60
    p.text(L, y - 18, title, 18, "title")
    y -= 40
    for para in paragraphs:
        for line in wrap(para, lambda t: p.width(t, size), W - 2 * L):
            y = pg.ensure(y, leading)
            p.text(L, y - size, line, size)
            y -= leading
        y -= leading * 0.5


# --------------------------------------------------------------------------
# cjk: non-embedded CID fonts (reportlab's built-in Adobe CJK font names)
# --------------------------------------------------------------------------
CID_LANGS = [
    ("繁體中文 Traditional Chinese", ("MSung-Light",), TC_PHRASES),
    ("简体中文 Simplified Chinese", ("STSong-Light",), SC_PHRASES),
    ("日本語 Japanese", ("HeiseiMin-W3", "HeiseiKakuGo-W5"), JA_PHRASES),
    ("한국어 Korean", ("HYSMyeongJo-Medium", "HYGothic-Medium"), KO_PHRASES),
]


def cjk_four_languages(out: Path, ctx: Ctx) -> dict:
    r = rng("cjk-4lang")
    c = rl_canvas(out, "FastPDF fixture: non-embedded CJK CID fonts")
    W, H = A4
    pages = 0
    for label, faces, pool in CID_LANGS:
        for face in faces:
            name = cid_font(face)
            c.setFont("Helvetica-Bold", 12)
            c.drawString(56, H - 50, f"{face} (non-embedded CIDFontType0, UCS2 CMap)")
            c.setFont(name, 18)
            c.drawString(56, H - 80, label)
            y = H - 110
            c.setFont(name, 11)
            while y > 70:
                for line in wrap(cjk_paragraph(r, pool), lambda t: stringWidth(t, name, 11),
                                 W - 112):
                    if y < 70:
                        break
                    c.drawString(56, y, line)
                    y -= 17
                y -= 8
            c.showPage()
            pages += 1
    c.save()
    return {"pages": pages}


def cjk_vertical(out: Path, ctx: Ctx) -> dict:
    r = rng("cjk-vertical")
    c = rl_canvas(out, "FastPDF fixture: vertical CJK text (-V CMaps)")
    W, H = A4
    pages = 0
    for face, pool in (("MSung-Light", TC_PHRASES), ("HeiseiMin-W3", JA_PHRASES)):
        name = cid_font(face, vertical=True)
        c.setFont("Helvetica-Bold", 12)
        c.drawString(40, H - 36, f"{face} vertical writing (-UCS2-V CMap), right to left")
        text = "".join(cjk_paragraph(r, pool, 6, 9).split())
        size, per_col = 16, 42
        x = W - 50
        c.setFont(name, size)
        for i in range(0, len(text), per_col):
            if x < 40:
                break
            c.drawString(x, H - 60, text[i:i + per_col])
            x -= size * 1.6
        c.showPage()
        pages += 1
    c.save()
    return {"pages": pages}


def cjk_dense(out: Path, ctx: Ctx, pages: int = 50) -> dict:
    r = rng("cjk-dense")
    name = cid_font("MSung-Light")
    c = rl_canvas(out, f"FastPDF fixture: {pages} pages dense Traditional Chinese (MSung-Light)")
    W, H = A4
    for p in range(1, pages + 1):
        c.setFont(name, 14)
        c.drawString(50, H - 50, f"第{p}頁　繁體中文密集文字（未嵌入字型）")
        c.setFont(name, 10)
        y = H - 76
        while y > 50:
            for line in wrap(cjk_paragraph(r, TC_PHRASES, 3, 6), lambda t: stringWidth(t, name, 10),
                             W - 100):
                if y < 50:
                    break
                c.drawString(50, y, line)
                y -= 15
        c.showPage()
    c.save()
    return {"pages": pages}


def cjk_legacy_cmap(out: Path, ctx: Ctx) -> dict:
    name = cid_font("MSung-Light", reportlab_legacy_cmap=True)
    c = rl_canvas(out, "FastPDF fixture: reportlab legacy MSung-Light CMap mismatch")
    W, H = A4
    c.setFont("Helvetica-Bold", 11)
    c.drawString(56, H - 50, "MSung-Light (Adobe-CNS1) with /Encoding /UniGB-UCS2-H (reportlab default)")
    c.setFont(name, 12)
    y = H - 80
    for phrase in TC_PHRASES:
        for line in wrap(phrase, lambda t: stringWidth(t, name, 12), W - 112):
            c.drawString(56, y, line)
            y -= 19
    c.showPage()
    c.save()
    return {"pages": 1}


# --------------------------------------------------------------------------
# japanese
# --------------------------------------------------------------------------
def ja_cid(out: Path, ctx: Ctx) -> dict:
    r = rng("ja-cid")
    c = rl_canvas(out, "FastPDF fixture: Japanese, non-embedded CID fonts")
    W, H = A4
    for face in ("HeiseiMin-W3", "HeiseiKakuGo-W5"):
        name = cid_font(face)
        c.setFont("Helvetica-Bold", 12)
        c.drawString(56, H - 50, f"{face} (Adobe-Japan1, UniJIS-UCS2-H, not embedded)")
        y = H - 80
        c.setFont(name, 11)
        for phrase in JA_PHRASES:
            c.drawString(56, y, phrase)
            y -= 18
        y -= 10
        while y > 70:
            for line in wrap(cjk_paragraph(r, JA_PHRASES), lambda t: stringWidth(t, name, 11),
                             W - 112):
                if y < 70:
                    break
                c.drawString(56, y, line)
                y -= 18
        c.showPage()
    name = cid_font("HeiseiMin-W3", vertical=True)
    c.setFont("Helvetica-Bold", 12)
    c.drawString(40, H - 36, "HeiseiMin-W3 vertical (UniJIS-UCS2-V)")
    text = "".join(cjk_paragraph(r, JA_PHRASES, 6, 8).split())
    c.setFont(name, 15)
    x = W - 50
    for i in range(0, len(text), 44):
        if x < 40:
            break
        c.drawString(x, H - 60, text[i:i + 44])
        x -= 24
    c.showPage()
    c.save()
    return {"pages": 3}


def _ja_paragraphs(key: str) -> list[str]:
    r = rng(key)
    return list(JA_PHRASES) + [cjk_paragraph(r, JA_PHRASES, 3, 6) for _ in range(40)]


def ja_embedded_ttf(out: Path, ctx: Ctx) -> dict:
    info = pick_font(ctx, JA_GOTHIC_FIRST)
    name = ttf_font(info, "JaEmbedded")
    p = ReportLabPainter(str(out), {"body": name, "title": name},
                         title="FastPDF fixture: Japanese embedded TrueType subsets")
    paragraphs_layout(p, f"日本語テスト（{info['family']} 埋め込み）", _ja_paragraphs("ja-ttf"),
                      note="架空のテスト文書です。")
    return {"pages": p.close(), **font_inputs(info)}


def ja_embedded_type0(out: Path, ctx: Ctx) -> dict:
    info = pick_font(ctx, JA_MSGOTHIC_FIRST)
    with open(out, "wb") as fp:
        pdf = RawPdf(fp)
        f = Type0Font(pdf, info, "F1")
        p = RawPainter(pdf, {"body": f, "title": f}, title="FastPDF fixture: Japanese Type0",
                       id_seed="ja-type0")
        paragraphs_layout(p, f"日本語テスト（{info['family']} Type0／Identity-H）",
                          _ja_paragraphs("ja-type0"), note="架空のテスト文書です。")
        pages = p.close()
    return {"pages": pages, **font_inputs(info)}


# --------------------------------------------------------------------------
# traditional-chinese
# --------------------------------------------------------------------------
def tc_doc_ttf(out: Path, ctx: Ctx, doc: str) -> dict:
    info = pick_font(ctx, TC_KAI_FIRST if doc == "gov" else TC_SANS_FIRST)
    name = ttf_font(info, "TCEmbedded")
    p = ReportLabPainter(str(out), {"body": name, "title": name},
                         title="FastPDF fixture: " + ("公文（函）" if doc == "gov" else "檢驗報告"),
                         subject="虛構測試文件")
    gov_letter(p) if doc == "gov" else lab_report(p)
    return {"pages": p.close(), **font_inputs(info)}


def tc_doc_type0(out: Path, ctx: Ctx, doc: str) -> dict:
    info = pick_font(ctx, TC_KAI_FIRST if doc == "gov" else TC_MING_FIRST)
    with open(out, "wb") as fp:
        pdf = RawPdf(fp)
        f = Type0Font(pdf, info, "F1")
        p = RawPainter(pdf, {"body": f, "title": f},
                       title="FastPDF fixture: " + ("公文（函）" if doc == "gov" else "檢驗報告"),
                       id_seed=f"tc-{doc}-type0")
        gov_letter(p) if doc == "gov" else lab_report(p)
        pages = p.close()
    return {"pages": pages, **font_inputs(info)}


def tc_doc_cid(out: Path, ctx: Ctx, doc: str) -> dict:
    body = cid_font("MSung-Light")
    p = ReportLabPainter(str(out), {"body": body, "title": body},
                         title="FastPDF fixture: " + ("公文（函）" if doc == "gov" else "檢驗報告"),
                         subject="虛構測試文件")
    gov_letter(p) if doc == "gov" else lab_report(p)
    return {"pages": p.close()}


def tc_gov_scanned_jpeg(out: Path, ctx: Ctx) -> dict:
    info = pick_font(ctx, TC_KAI_FIRST)
    fx = rng("tc-scan-jpeg-fx")

    def enc(img, idx):
        img = scan_effects(img, fx, noise_amp=12, blur=0.5, speckles=250)
        return {"data": jpeg(img, 70), "w": img.size[0], "h": img.size[1],
                "filter": "DCTDecode", "cs": "DeviceRGB"}

    font = (info["path"], info["index"])
    pages, _ = render_raster_pages(lambda p: gov_letter(p, with_seal=True),
                                   {"body": font, "title": font}, enc, dpi=200, mode="RGB",
                                   paper=(255, 255, 255))
    n = write_scanned_pdf(out, pages, title="FastPDF fixture: scanned 公文 (RGB JPEG)",
                          id_seed="tc-scan-jpeg")
    return {"pages": n, **font_inputs(info)}


def tc_gov_scanned_ccitt(out: Path, ctx: Ctx) -> dict:
    info = pick_font(ctx, TC_KAI_FIRST)
    font = (info["path"], info["index"])
    pages, _ = render_raster_pages(lambda p: gov_letter(p, with_seal=True),
                                   {"body": font, "title": font}, _g4_encoder(rng("tc-scan-g4")),
                                   dpi=300)
    n = write_scanned_pdf(out, pages, title="FastPDF fixture: scanned 公文 (CCITT G4)",
                          id_seed="tc-scan-g4")
    return {"pages": n, **font_inputs(info)}


def big5_level1_chars() -> str:
    chars = []
    for lead in range(0xA4, 0xC7):
        for trail in list(range(0x40, 0x7F)) + list(range(0xA1, 0xFF)):
            if (lead, trail) > (0xC6, 0x7E):
                break
            try:
                chars.append(bytes([lead, trail]).decode("big5"))
            except UnicodeDecodeError:
                pass
    return "".join(chars)


def _big5_layout(p: Any, chars: str) -> None:
    H = p.page_h
    per_row, rows_per_page, size = 30, 40, 15.0
    rows = [chars[i:i + per_row] for i in range(0, len(chars), per_row)]
    for start in range(0, len(rows), rows_per_page):
        if start:
            p.new_page()
        p.text(50, H - 40, f"Big5 常用字（第一字面）{len(chars)} 字：第 {start * per_row + 1} 字起",
               12, "title")
        y = H - 70
        for row in rows[start:start + rows_per_page]:
            p.text(50, y, f"{row[0].encode('big5').hex().upper()}", 7, "body", (0.4, 0.4, 0.4))
            p.text(80, y, row, size)
            y -= 18.5


def tc_big5_ttf(out: Path, ctx: Ctx) -> dict:
    info = pick_font(ctx, TC_MING_FIRST)
    name = ttf_font(info, "TCBig5")
    p = ReportLabPainter(str(out), {"body": name, "title": name},
                         title="FastPDF fixture: Big5 level-1 characters (TrueType subsets)")
    _big5_layout(p, big5_level1_chars())
    return {"pages": p.close(), **font_inputs(info)}


def tc_big5_type0(out: Path, ctx: Ctx) -> dict:
    info = pick_font(ctx, TC_MING_FIRST)
    with open(out, "wb") as fp:
        pdf = RawPdf(fp)
        f = Type0Font(pdf, info, "F1")
        p = RawPainter(pdf, {"body": f, "title": f},
                       title="FastPDF fixture: Big5 level-1 characters (Type0)", id_seed="tc-big5")
        _big5_layout(p, big5_level1_chars())
        pages = p.close()
    return {"pages": pages, **font_inputs(info)}


_TRICKY = ("新細明體／標楷體在 FreeType 中屬於 tricky font（字形可能依賴 TrueType hinting 指令組合筆畫），"
           "不執行 hinting 的 rasterizer 可能畫出錯位筆畫。")


def jobs() -> list[Job]:
    return [
        Job("cjk/cid-nonembedded-4lang.pdf", cjk_four_languages, "open_ok",
            "非嵌入 CJK 字型（Adobe CID 字型名稱）：MSung-Light（繁中，UniCNS-UCS2-H）、STSong-Light（簡中）、HeiseiMin-W3／HeiseiKakuGo-W5（日）、HYSMyeongJo-Medium／HYGothic-Medium（韓）。reader 必須自行找系統替代字型。",
            pages=6, features=("CIDFontType0", "non-embedded", "UCS2-CMap", "font-fallback"),
            producer="reportlab", cost=0.8),
        Job("cjk/cid-nonembedded-vertical.pdf", cjk_vertical, "open_ok",
            "非嵌入 CID 字型直排（UniCNS-UCS2-V、UniJIS-UCS2-V CMap，WMode 1），由右至左多欄。",
            pages=2, features=("vertical-writing", "non-embedded", "V-CMap"), producer="reportlab",
            cost=0.3),
        Job("cjk/reportlab-msung-unigb-cmap-mismatch.pdf", cjk_legacy_cmap, "open_ok",
            "真實世界的 reportlab 舊行為：MSung-Light（CIDSystemInfo Adobe-CNS1）卻使用 /UniGB-UCS2-H CMap（GB1 字集）。字碼本身是 UCS-2，以 Unicode 做字型替代的 reader 可正確顯示；嚴格依 CID 對應的 reader 會出現錯字。不可 crash。",
            pages=1, features=("CIDFontType0", "non-embedded", "cmap-ordering-mismatch"),
            producer="reportlab", cost=0.2),
        Job("cjk/cid-nonembedded-dense-50p.pdf", cjk_dense, "open_ok",
            "50 頁密集繁中文字（MSung-Light，未嵌入），測替代字型下的文字渲染與抽取效能。",
            pages=50, features=("non-embedded", "dense-text"), producer="reportlab", cost=2),
        Job("japanese/cid-nonembedded-heisei.pdf", ja_cid, "open_ok",
            "日文非嵌入 CID 字型：HeiseiMin-W3、HeiseiKakuGo-W5 橫排（含平假名、片假名、半形片假名、全形英數、記號）與 HeiseiMin-W3 直排。",
            pages=3, features=("CIDFontType0", "non-embedded", "vertical-writing"),
            producer="reportlab", cost=0.4),
        Job("japanese/embedded-ttfsubset.pdf", ja_embedded_ttf, "open_ok",
            "日文嵌入 TrueType subset（reportlab，simple font、每個 subset ≤256 字；優先 Yu Gothic Medium，其次 MS Gothic）。系統字型只在產生時讀取。",
            features=("TrueType", "subset", "embedded", "system-font"), producer="reportlab",
            cost=2),
        Job("japanese/embedded-type0.pdf", ja_embedded_type0, "open_ok",
            "日文嵌入 Type0／CIDFontType2／Identity-H＋ToUnicode（fontTools subset、保留 GID；優先 MS Gothic）。",
            features=("Type0", "CIDFontType2", "Identity-H", "ToUnicode", "system-font"), cost=2),
        Job("traditional-chinese/gov-letter-embedded-ttfsubset.pdf", tc_doc_ttf, "open_ok",
            "模擬政府公文「函」（發文機關、受文者、發文日期／字號、主旨、說明、附表、正副本、附件表格），嵌入 TrueType subset（reportlab，優先標楷體）。內容全為虛構。" + _TRICKY,
            features=("TrueType", "subset", "embedded", "system-font", "gov-document", "table"),
            kwargs={"doc": "gov"}, producer="reportlab", cost=2),
        Job("traditional-chinese/gov-letter-embedded-type0.pdf", tc_doc_type0, "open_ok",
            "同一份虛構公文，以 Type0／CIDFontType2／Identity-H＋ToUnicode 嵌入（Word／瀏覽器輸出 PDF 的典型結構，優先標楷體）。" + _TRICKY,
            features=("Type0", "CIDFontType2", "Identity-H", "ToUnicode", "gov-document"),
            kwargs={"doc": "gov"}, cost=2),
        Job("traditional-chinese/gov-letter-cid-msung-nonembedded.pdf", tc_doc_cid, "open_ok",
            "同一份虛構公文，使用非嵌入 MSung-Light（UniCNS-UCS2-H），需要字型替代。",
            features=("CIDFontType0", "non-embedded", "gov-document"), kwargs={"doc": "gov"},
            producer="reportlab", cost=0.4),
        Job("traditional-chinese/lab-report-embedded-ttfsubset.pdf", tc_doc_ttf, "open_ok",
            "模擬醫院檢驗報告（病人資料表、CBC／生化／血脂／尿液／甲狀腺，含數值、單位、參考範圍、H／L 註記），嵌入 TrueType subset（優先微軟正黑體）。病人與數值全為虛構。",
            features=("TrueType", "subset", "embedded", "system-font", "medical-report", "table"),
            kwargs={"doc": "lab"}, producer="reportlab", cost=2.5),
        Job("traditional-chinese/lab-report-embedded-type0.pdf", tc_doc_type0, "open_ok",
            "同一份虛構檢驗報告，以 Type0／Identity-H 嵌入（優先新細明體）。" + _TRICKY,
            features=("Type0", "CIDFontType2", "Identity-H", "ToUnicode", "medical-report"),
            kwargs={"doc": "lab"}, cost=2.5),
        Job("traditional-chinese/lab-report-cid-msung-nonembedded.pdf", tc_doc_cid, "open_ok",
            "同一份虛構檢驗報告，使用非嵌入 MSung-Light（UniCNS-UCS2-H）。",
            features=("CIDFontType0", "non-embedded", "medical-report"), kwargs={"doc": "lab"},
            producer="reportlab", cost=0.4),
        Job("traditional-chinese/gov-letter-scanned-rgb-jpeg.pdf", tc_gov_scanned_jpeg, "open_ok",
            "掃描版公文：以系統繁中字型點陣化（200 dpi 彩色、紅色印章、紙色、雜訊、傾斜），每頁一張 RGB JPEG，無文字層。",
            features=("DCTDecode", "DeviceRGB", "200dpi", "gov-document", "scan"), cost=3),
        Job("traditional-chinese/gov-letter-scanned-ccitt-g4.pdf", tc_gov_scanned_ccitt, "open_ok",
            "掃描版公文黑白版：300 dpi 1-bit CCITT G4（公文掃描歸檔最常見的格式）。",
            features=("CCITTFaxDecode", "G4", "gov-document", "scan"), cost=3),
        Job("traditional-chinese/big5-level1-chars-ttfsubset.pdf", tc_big5_ttf, "open_ok",
            "Big5 第一字面全部 5401 個常用字，reportlab TrueType subset（約 22 個 subset 字型），測大量字型物件與 glyph cache。",
            features=("TrueType", "many-subsets", "glyph-cache"), producer="reportlab", cost=4),
        Job("traditional-chinese/big5-level1-chars-type0.pdf", tc_big5_type0, "open_ok",
            "Big5 第一字面 5401 字，單一 Type0／Identity-H 字型（大型 subset），測 glyph cache 與 ToUnicode 對照。",
            features=("Type0", "Identity-H", "ToUnicode", "glyph-cache"), cost=4),
    ]
