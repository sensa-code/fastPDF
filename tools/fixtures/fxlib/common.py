"""Shared definitions: jobs, context, deterministic RNG and synthetic text.

All text produced here is synthetic (generated from small hand-written word
and phrase pools). It contains no real personal data and no third-party
copyrighted prose.
"""

from __future__ import annotations

import hashlib
import random
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Callable

SEED = 20261004
GENERATOR_VERSION = "1.0.0"

# Expected-behaviour vocabulary used in manifest.json (documented in README).
EXPECTED_VALUES = {
    "open_ok": "Valid file; must open and render every page.",
    "password_required": "Encrypted with a non-empty user password (see encryption.user_password).",
    "recover_expected": "Damaged but recoverable (xref rebuild, /Length repair, ...); "
                        "a robust reader opens it, otherwise it must fail cleanly.",
    "render_partial_ok": "Opens; some pages/objects are broken and may be skipped, "
                         "the rest must render; never crash.",
    "guardrail_expected": "Hostile input; a §25 resource limit should trigger while the "
                          "process stays within its memory/time budget.",
    "open_error_expected": "Should be rejected with a clean error (no panic, no hang).",
}


class SkipFixture(Exception):
    """Raised by a generator when a prerequisite (e.g. a system font) is missing."""


@dataclass
class Ctx:
    profile: str
    large_file_mb: int
    fonts: dict[str, dict[str, Any]]  # role -> font info (path, index, ...)

    def font(self, role: str) -> dict[str, Any]:
        info = self.fonts.get(role)
        if not info:
            raise SkipFixture(f"system font for role '{role}' not found")
        return info


@dataclass
class Job:
    path: str                       # posix path relative to the output directory
    func: Callable[..., dict | None]
    expected: str
    description: str
    pages: int | None = None
    guardrails: tuple[str, ...] = ()
    features: tuple[str, ...] = ()
    kwargs: dict[str, Any] = field(default_factory=dict)
    cost: float = 1.0               # rough relative cost, used for scheduling only
    full_only: bool = False
    producer: str = "rawpdf"

    @property
    def category(self) -> str:
        return self.path.split("/", 1)[0]


def rng(key: str) -> random.Random:
    """Independent, reproducible RNG per fixture (str seeds hash via SHA-512)."""
    return random.Random(f"{SEED}:{key}")


def sha256_file(path: Path) -> tuple[int, str]:
    h = hashlib.sha256()
    size = 0
    with open(path, "rb") as f:
        while True:
            chunk = f.read(1 << 22)
            if not chunk:
                break
            size += len(chunk)
            h.update(chunk)
    return size, h.hexdigest()


# --------------------------------------------------------------------------
# Synthetic Latin text
# --------------------------------------------------------------------------
_WORDS = (
    "page render glyph cache tile viewport latency memory budget stream object "
    "parser xref trailer font image vector path curve stroke fill shading pattern "
    "clip text layout search index thumbnail scroll zoom document reader engine "
    "frame buffer pixel raster scanline worker thread queue priority request "
    "cancel prefetch visible window surface device context matrix transform "
    "width height offset length filter decode inflate encode table record field "
    "value number string array dictionary reference resource content operator "
    "graphics state color space profile alpha blend mask group form annotation "
    "outline bookmark link action metadata catalog version header body section "
    "chapter paragraph line word character baseline ascent descent kerning "
    "spacing margin column gutter figure caption footnote appendix reference "
    "benchmark baseline regression budget target measure sample median percentile "
    "the a of and to in for with on by from at as is are was be this that which "
    "fast small large quick slow simple dense sparse early late first next last"
).split()


def latin_sentence(r: random.Random) -> str:
    n = r.randint(7, 18)
    words = [r.choice(_WORDS) for _ in range(n)]
    if r.random() < 0.35:
        words.insert(r.randint(1, n - 1), str(r.randint(2, 9999)))
    if r.random() < 0.15:
        words.insert(r.randint(1, n - 1), f"({r.choice(_WORDS)})")
    s = " ".join(words)
    s = s[0].upper() + s[1:]
    return s + r.choice((".", ".", ".", ".", ";", "?", "!", ":"))


def latin_paragraph(r: random.Random, lo: int = 3, hi: int = 7) -> str:
    return " ".join(latin_sentence(r) for _ in range(r.randint(lo, hi)))


# --------------------------------------------------------------------------
# Synthetic CJK phrase pools (hand-written, fictional)
# --------------------------------------------------------------------------
TC_PHRASES = (
    "這是一份用於FastPDF效能測試的繁體中文範例文件，所有內容皆為虛構。",
    "臺北市今日天氣晴朗，最高氣溫攝氏二十八度，午後山區有局部短暫陣雨。",
    "本公司為提升服務品質，自即日起延長線上客服時間至晚間十點。",
    "請於期限內完成線上申請，逾期者將不予受理，敬請見諒。",
    "數位轉型讓政府服務更加便利，民眾可透過行動裝置查詢辦理進度。",
    "健康檢查報告顯示各項數值大致正常，建議維持規律運動與均衡飲食。",
    "會議紀錄：一、確認上次會議決議事項；二、討論年度預算編列；三、臨時動議。",
    "注意事項：本文件僅供測試使用，不具任何法律效力。",
    "為落實節能減碳政策，各單位應於下班前關閉電源並回報執行情形。",
    "圖書館將於下週一起進行系統維護，暫停借還書服務三天。",
    "高雄港貨櫃吞吐量較去年同期成長百分之五點三，創下歷史新高。",
    "颱風警報解除後，請民眾注意道路坍方與落石，行車務必小心。",
    "依據「虛構市資訊安全管理要點」，密碼應每九十日更新一次。",
    "新竹科學園區廠商說明會訂於十月十五日上午九時舉行。",
    "全民健康保險卡請妥善保管，遺失時應儘速向保險人申請補發。",
    "臺灣鐵路連假期間加開區間車，旅客可多加利用。",
    "請各位同仁於本週五前繳交工作週報，並副知主管。",
    "測試字元：（全形括號）「引號」『雙引號』、頓號；分號：冒號！驚嘆號？問號。",
    "數字與單位：１２３４５、100公尺、3.5公斤、攝氏25度、NT$1,280元。",
    "罕用字測試：犇、淼、鑫、垚、焱、龘、靐、齉、爨。",
)

SC_PHRASES = (
    "这是用于FastPDF渲染测试的简体中文示例文档，所有内容均为虚构。",
    "北京今天天气晴朗，最高气温二十三度，夜间有轻微雾霾。",
    "会议将于下午三点在第二会议室举行，请提前阅读相关资料。",
    "数字化转型使政府服务可以在线办理，群众少跑腿。",
    "请在截止日期前提交申请材料，逾期将不予受理。",
    "测试字符：（括号）“引号”、顿号；分号：冒号！感叹号？问号。",
)

JA_PHRASES = (
    "これはFastPDFのための日本語テスト文書です。内容はすべて架空のものです。",
    "東京都千代田区の天気は晴れ、最高気温は二十三度の予想です。",
    "ひらがな、カタカナ、漢字、そして全角ＡＢＣと半角ｶﾀｶﾅを含みます。",
    "請求書番号：ＴＥＳＴ－２０２６－００１　金額：１２，３４５円（税込）",
    "会議は午後三時から第二会議室で行われます。資料を事前に確認してください。",
    "デジタル化の推進により、行政手続きのオンライン申請が可能になりました。",
    "医療機関の受付時間は平日の午前九時から午後五時までです。",
    "縦書きの表示テスト：一二三四五六七八九十、「かぎ括弧」と句読点。",
    "記号の例：〒１００－０００１、※注意、①②③、㈱、〜、・、ー。",
)

KO_PHRASES = (
    "이 문서는 FastPDF 렌더링 테스트를 위한 한국어 샘플입니다.",
    "서울특별시의 오늘 날씨는 맑고 최고 기온은 이십삼 도입니다.",
    "회의는 오후 세 시에 제이 회의실에서 열립니다.",
    "한글과 漢字가 섞인 문장도 포함합니다.",
    "신청서는 마감일 전까지 온라인으로 제출해 주십시오.",
)


def cjk_paragraph(r: random.Random, pool: tuple[str, ...], lo: int = 2, hi: int = 5) -> str:
    return "".join(r.choice(pool) for _ in range(r.randint(lo, hi)))


# --------------------------------------------------------------------------
# Line breaking helpers (Latin words and CJK characters)
# --------------------------------------------------------------------------
_NO_LINE_START = set("，。、；：？！）」』】》〉〕,.;:?!)]}%％・ー〜")
_NO_LINE_END = set("（「『【《〈〔([{")


def tokenize(text: str) -> list[str]:
    """ASCII words/numbers stay together, every other character is a token."""
    tokens: list[str] = []
    buf = ""
    for ch in text:
        if ch.isascii() and not ch.isspace():
            buf += ch
            continue
        if buf:
            tokens.append(buf)
            buf = ""
        tokens.append(ch)
    if buf:
        tokens.append(buf)
    return tokens


def wrap(text: str, width: Callable[[str], float], max_w: float) -> list[str]:
    """Greedy line breaking with minimal CJK kinsoku rules."""
    lines: list[str] = []
    cur, cur_w = "", 0.0
    for tok in tokenize(text):
        w = width(tok)
        if cur and cur_w + w > max_w and tok not in _NO_LINE_START and not tok.isspace():
            carry = ""
            if cur[-1] in _NO_LINE_END:
                carry, cur = cur[-1], cur[:-1]
            lines.append(cur.rstrip())
            cur = carry + tok
            cur_w = width(cur)
        elif not cur and tok.isspace():
            continue
        else:
            cur += tok
            cur_w += w
    if cur.strip():
        lines.append(cur.rstrip())
    return lines
