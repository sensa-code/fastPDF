"""Synthetic Taiwanese documents: a government letter (公文「函」) and a lab report.

Every organisation, person, address, phone number, document number and lab
value below is fictional and generated for testing only. Each page carries a
visible "test sample / fictional" note.

The layouts are painter-agnostic (see painter.py), so the same document can be
emitted with non-embedded CID fonts, reportlab TrueType subsets, Type0
Identity-H embedding, or rasterized to simulate a scan.
"""

from __future__ import annotations

from typing import Any, Callable

from .common import wrap

GRAY = (0.45, 0.45, 0.45)
RED = (0.80, 0.05, 0.05)
LIGHT = (0.90, 0.90, 0.90)

FICTION_NOTE = "【測試樣本】本文件內容全部為虛構，僅供 FastPDF 測試使用，非真實文件。"


class Pager:
    """Tracks the vertical cursor and starts new pages when needed."""

    def __init__(self, p: Any, top: float, bottom: float, decorate: Callable[[int], None]):
        self.p, self.top, self.bottom, self.decorate = p, top, bottom, decorate
        self.page_no = 1
        decorate(1)

    def ensure(self, y: float, need: float) -> float:
        if y - need < self.bottom:
            self.p.new_page()
            self.page_no += 1
            self.decorate(self.page_no)
            return self.top
        return y


def hanging_para(p: Any, pg: Pager, x: float, y: float, label: str, text: str, size: float,
                 max_x: float, leading: float, role: str = "body") -> float:
    lw = p.width(label, size, role) if label else 0.0
    lines = wrap(text, lambda t: p.width(t, size, role), max_x - x - lw)
    for i, line in enumerate(lines):
        y = pg.ensure(y, leading)
        if i == 0 and label:
            p.text(x, y - size, label, size, role)
        p.text(x + lw, y - size, line, size, role)
        y -= leading
    return y


def table(p: Any, pg: Pager, x: float, y: float, col_w: list[float], rows: list[list[str]],
          size: float, leading: float, *, role: str = "body", header: bool = True,
          align: list[str] | None = None,
          color_fn: Callable[[int, int, str], tuple[float, float, float]] | None = None,
          pad: float = 3.5) -> float:
    align = align or ["l"] * len(col_w)
    total_w = sum(col_w)
    for r_i, row in enumerate(rows):
        cells = [wrap(str(c), lambda t: p.width(t, size, role), col_w[i] - 2 * pad) or [""]
                 for i, c in enumerate(row)]
        h = max(len(c) for c in cells) * leading + 2 * pad
        new_y = pg.ensure(y, h)
        if new_y != y:
            y = new_y
            if header and r_i > 0:  # repeat the header row on the new page
                y = table(p, pg, x, y, col_w, [rows[0]], size, leading, role=role,
                          header=True, align=align, pad=pad)
        if header and r_i == 0:
            p.rect(x, y - h, total_w, h, stroke=None, fill=LIGHT)
        p.rect(x, y - h, total_w, h, lw=0.6)
        cx = x
        for c_i, lines in enumerate(cells):
            if c_i:
                p.line(cx, y, cx, y - h, lw=0.6)
            color = color_fn(r_i, c_i, row[c_i]) if (color_fn and r_i > 0) else (0, 0, 0)
            for l_i, line in enumerate(lines):
                tw = p.width(line, size, role)
                if align[c_i] == "r":
                    tx = cx + col_w[c_i] - pad - tw
                elif align[c_i] == "c":
                    tx = cx + (col_w[c_i] - tw) / 2
                else:
                    tx = cx + pad
                p.text(tx, y - pad - l_i * leading - size * 0.95, line, size, role, color)
            cx += col_w[c_i]
        y -= h
    return y


def seal(p: Any, x: float, y: float, side: float, chars: str, role: str = "body") -> None:
    """Square red seal with four characters laid out right-to-left, top-to-bottom."""
    p.rect(x, y, side, side, lw=2.2, stroke=RED)
    p.rect(x + 4, y + 4, side - 8, side - 8, lw=0.8, stroke=RED)
    s = side * 0.36
    cols = [chars[0:2], chars[2:4]]
    for col_i, col in enumerate(cols):
        cx = x + side - (col_i + 1) * side / 2 + (side / 2 - s) / 2
        for row_i, ch in enumerate(col):
            cy = y + side - (row_i + 1) * side / 2 + (side / 2 - s) / 2 + s * 0.12
            p.text(cx, cy, ch, s, role, RED)


# --------------------------------------------------------------------------
# 公文：函
# --------------------------------------------------------------------------
GOV_AGENCY = "虛構市政府環境保護局"
GOV_RIGHT = [
    "地址：99999虛構市測試區範例路一段1號",
    "承辦人：測試員甲",
    "電話：(00)0000-0000 分機123",
    "傳真：(00)0000-0001",
    "電子信箱：fixture@example.invalid",
]
GOV_META = [
    "發文日期：中華民國115年10月4日",
    "發文字號：虛環稽字第1150000001號",
    "速別：普通件",
    "密等及解密條件或保密期限：",
    "附件：如說明三",
]
GOV_SUBJECT = ("有關貴公司所屬「範例路廠區」空氣污染防制設施定期檢測結果未符合規定一案，"
               "請於文到30日內完成改善並函復本局，請查照。")
GOV_ITEMS = [
    "依據「虛構市空氣污染防制管理自治條例（測試版）」第5條及第12條規定辦理。",
    "本局於115年9月15日派員至貴公司廠區執行稽查，採樣檢測結果詳如附表，其中粒狀污染物"
    "排放濃度為每立方公尺58.2毫克，超過本市管制標準。",
    "請貴公司於115年11月3日前提出改善計畫書（格式如附件），內容應包含污染源說明、改善措施、"
    "預定完成日期及聯絡窗口，並以公文或電子郵件送達本局。",
    "逾期未提出改善計畫或改善後複查仍未符合規定者，本局將依相關規定續處，並得按次處分。",
    "本案如有疑義，請洽本局承辦人員（聯絡方式如右上）。",
    "本文件為FastPDF測試用虛構範例，所載機關、人名、地址、電話及字號均非真實，"
    "請勿作為任何行政用途。",
]
GOV_TABLE = [
    ["項次", "檢測項目", "檢測值", "管制標準", "判定"],
    ["1", "粒狀污染物", "58.2 mg/Nm3", "50 mg/Nm3", "不合格"],
    ["2", "硫氧化物", "120 ppm", "300 ppm", "合格"],
    ["3", "氮氧化物", "210 ppm", "250 ppm", "合格"],
    ["4", "不透光率", "15 %", "20 %", "合格"],
    ["5", "揮發性有機物", "85 ppm", "100 ppm", "合格"],
    ["6", "一氧化碳", "35 ppm", "2000 ppm", "合格"],
]
GOV_FORM = [
    ["欄位", "內容（由受文者填寫）"],
    ["公司名稱", ""],
    ["管制編號", ""],
    ["污染源說明", ""],
    ["改善措施", ""],
    ["預定完成日期", "中華民國　　年　　月　　日"],
    ["聯絡人／電話", ""],
]
_NUMS = "一二三四五六七八九十"


def gov_letter(p: Any, *, with_seal: bool = False) -> None:
    W, H = p.page_w, p.page_h
    L, R = 72.0, 62.0
    max_x = W - R

    def decorate(n: int) -> None:
        p.text(L, H - 34, FICTION_NOTE, 8, "body", GRAY)
        foot = f"第 {n} 頁"
        p.text((W - p.width(foot, 9)) / 2, 30, foot, 9, "body", GRAY)

    pg = Pager(p, H - 60, 56, decorate)
    y = H - 58
    p.text(L, y, "檔　　號：", 10)
    p.text(L, y - 14, "保存年限：", 10)
    title = f"{GOV_AGENCY}　函"
    p.text((W - p.width(title, 20, "title")) / 2, H - 112, title, 20, "title")
    y = H - 140
    rx = max_x - 210
    for line in GOV_RIGHT:
        p.text(rx, y, line, 10)
        y -= 14
    y -= 8
    p.text(L, y, "受文者：虛構科技股份有限公司", 14)
    y -= 26
    for line in GOV_META:
        p.text(L, y, line, 11)
        y -= 17
    y -= 10
    y = hanging_para(p, pg, L, y, "主旨：", GOV_SUBJECT, 14, max_x, 22)
    y -= 6
    y = pg.ensure(y, 22)
    p.text(L, y - 14, "說明：", 14)
    y -= 22
    for i, item in enumerate(GOV_ITEMS):
        y = hanging_para(p, pg, L + 14, y, _NUMS[i] + "、", item, 14, max_x, 22)

    # Attachment table, copies and signature
    y -= 14
    y = pg.ensure(y, 60)
    p.text(L, y - 12, "附表：稽查採樣檢測結果一覽表", 12, "title")
    y -= 22
    y = table(p, pg, L, y, [40, 120, 105, 105, max_x - L - 370], GOV_TABLE, 11, 15,
              align=["c", "l", "r", "r", "c"],
              color_fn=lambda r, c, t: (0.8, 0.05, 0.05) if t == "不合格" else (0, 0, 0))
    y -= 18
    for label, text in (("正本：", "虛構科技股份有限公司"),
                        ("副本：", "虛構市政府環境保護局稽查科、虛構市測試區公所（均含附件）")):
        y = hanging_para(p, pg, L, y, label, text, 11, max_x, 16)
    y -= 26
    y = pg.ensure(y, 70)
    sign = "局長　範　例　人"
    p.text(max_x - p.width(sign, 18, "title") - 10, y - 20, sign, 18, "title")
    if with_seal:
        seal(p, max_x - 70, y - 82, 58, "虛構測試")
    y -= 100

    y = pg.ensure(y, 140)
    p.text(L, y - 12, "附件：改善計畫書格式（範例）", 12, "title")
    y -= 22
    table(p, pg, L, y, [110, max_x - L - 110], GOV_FORM, 11, 16)


# --------------------------------------------------------------------------
# 醫療檢驗報告
# --------------------------------------------------------------------------
LAB_PATIENT = [
    ["病歷號碼", "T000000001", "姓名", "測試病人甲"],
    ["性別", "女", "出生日期", "民國70年1月1日"],
    ["年齡", "45歲", "科別", "家庭醫學科"],
    ["開單醫師", "虛構醫師乙", "就診別", "門診"],
    ["檢體別", "血液／尿液", "檢驗單號", "L1151004-0001"],
    ["採檢時間", "2026/10/01 08:30", "報告時間", "2026/10/01 14:05"],
]
LAB_SECTIONS: list[tuple[str, list[list[str]]]] = [
    ("血液常規 CBC", [
        ["WBC 白血球", "6.8", "", "10^3/uL", "4.0-10.0"],
        ["RBC 紅血球", "4.52", "", "10^6/uL", "4.20-5.40"],
        ["Hb 血色素", "11.6", "L", "g/dL", "12.0-16.0"],
        ["Hct 血球容積比", "35.8", "L", "%", "37.0-47.0"],
        ["MCV 平均紅血球容積", "79.2", "L", "fL", "80.0-100.0"],
        ["PLT 血小板", "255", "", "10^3/uL", "150-400"],
    ]),
    ("生化檢驗 Chemistry", [
        ["Glucose AC 空腹血糖", "108", "H", "mg/dL", "70-99"],
        ["HbA1c 糖化血色素", "6.1", "H", "%", "4.0-5.6"],
        ["BUN 尿素氮", "14", "", "mg/dL", "7-20"],
        ["Creatinine 肌酸酐", "0.82", "", "mg/dL", "0.50-0.90"],
        ["eGFR 腎絲球過濾率", "88", "", "mL/min/1.73m2", ">=60"],
        ["UA 尿酸", "7.4", "H", "mg/dL", "2.4-5.7"],
        ["AST (GOT) 天門冬胺酸轉胺酶", "25", "", "U/L", "0-40"],
        ["ALT (GPT) 丙胺酸轉胺酶", "48", "H", "U/L", "0-41"],
        ["Na 鈉", "140", "", "mmol/L", "136-145"],
        ["K 鉀", "4.1", "", "mmol/L", "3.5-5.1"],
    ]),
    ("血脂檢驗 Lipid profile", [
        ["T-CHO 總膽固醇", "212", "H", "mg/dL", "<200"],
        ["TG 三酸甘油酯", "145", "", "mg/dL", "<150"],
        ["HDL-C 高密度脂蛋白膽固醇", "52", "", "mg/dL", ">=50"],
        ["LDL-C 低密度脂蛋白膽固醇", "131", "H", "mg/dL", "<130"],
    ]),
    ("尿液檢查 Urinalysis", [
        ["Color 顏色", "Yellow", "", "", "Yellow"],
        ["Turbidity 混濁度", "Clear", "", "", "Clear"],
        ["SG 比重", "1.015", "", "", "1.005-1.030"],
        ["pH 酸鹼值", "6.0", "", "", "5.0-8.0"],
        ["Protein 尿蛋白", "Negative", "", "mg/dL", "Negative"],
        ["Glucose 尿糖", "Negative", "", "mg/dL", "Negative"],
        ["OB 潛血", "Trace", "A", "", "Negative"],
        ["RBC 紅血球", "0-2", "", "/HPF", "0-2"],
        ["WBC 白血球", "6-10", "H", "/HPF", "0-5"],
    ]),
    ("甲狀腺功能 Thyroid", [
        ["TSH 甲狀腺刺激素", "2.15", "", "uIU/mL", "0.27-4.20"],
        ["Free T4 游離甲狀腺素", "1.21", "", "ng/dL", "0.93-1.70"],
    ]),
]
LAB_NOTES = [
    "註記說明：H 表示高於參考範圍，L 表示低於參考範圍，A 表示異常。",
    "參考範圍依本院（虛構）檢驗醫學部建立之成人參考值，僅供臨床參考，請由醫師綜合判讀。",
    "本報告為FastPDF測試用虛構資料，病人姓名、病歷號碼及數值皆為合成，非真實個人資料。",
]


def lab_report(p: Any) -> None:
    W, H = p.page_w, p.page_h
    L, R = 52.0, 52.0
    max_x = W - R
    content_w = max_x - L

    def decorate(n: int) -> None:
        p.text(L, H - 30, FICTION_NOTE, 8, "body", GRAY)
        t1 = "測試綜合醫院（虛構）檢驗醫學部"
        p.text((W - p.width(t1, 16, "title")) / 2, H - 58, t1, 16, "title")
        t2 = "臨床檢驗報告單" + ("" if n == 1 else "（續）")
        p.text((W - p.width(t2, 13, "title")) / 2, H - 78, t2, 13, "title")
        p.line(L, H - 88, max_x, H - 88, lw=1.2)
        foot = f"病歷號碼 T000000001　第 {n} 頁"
        p.text(max_x - p.width(foot, 8), 28, foot, 8, "body", GRAY)

    pg = Pager(p, H - 100, 60, decorate)
    y = H - 100
    y = table(p, pg, L, y, [70, content_w / 2 - 70, 70, content_w / 2 - 70], LAB_PATIENT,
              10, 14, header=False)
    y -= 14
    cols = [content_w - 300, 64, 36, 96, 104]
    head = ["檢驗項目", "結果", "註記", "單位", "參考範圍"]

    for title, rows in LAB_SECTIONS:
        y = pg.ensure(y, 60)
        p.text(L, y - 12, "■ " + title, 11.5, "title")
        y -= 18
        flagged = [r[:] for r in rows]
        y = table(p, pg, L, y, cols, [head] + flagged, 9.5, 13,
                  align=["l", "r", "c", "l", "l"],
                  color_fn=lambda r, c, t, rows=flagged: (
                      RED if (c in (1, 2) and rows[r - 1][2] in ("H", "L", "A")) else (0, 0, 0)))
        y -= 12
    y -= 6
    for note in LAB_NOTES:
        y = hanging_para(p, pg, L, y, "", note, 9, max_x, 13)
    y -= 18
    y = pg.ensure(y, 30)
    p.text(L, y - 10, "報告醫檢師：測試醫檢師丙　　審核醫師：虛構醫師丁　　列印時間：2026/10/01 14:10", 10)
