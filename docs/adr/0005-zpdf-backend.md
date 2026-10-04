# ADR 0005 — Engine Strategy: Hayro Baseline, zpdf Candidate

- 狀態：Accepted for M1–M4。主要 engine 的選擇已由 [ADR 0007](0007-primary-engine-hayro.md) 依 M4 數據決定（Hayro）
- 日期：2026-10-04
- 相關 spec：§3、§6、§19、§42–§44、§49 Q4–Q6；證據：`docs/audit/hayro.md`、`docs/audit/zpdf.md`

## Context

Spec §3 把 zpdf 列為長期的 primary candidate，Hayro 列為 secondary／reference，並要求「不要相信 README，所有效能都要由 FastPDF benchmark 驗證」。§44 要求在 M4 以同一批 PDF 比較，「不要先決定誰贏」。

兩份 engine audit 的重點：

| | Hayro（HEAD ≥ `ced00dd0`） | zpdf（`fe0ed23`） |
|---|---|---|
| 成熟度 | 1,800+ commits；被 typst 生態使用（crates.io 下載約 285 萬）；作者活躍；1.0 前仍有 breaking change | 4 個月、247 commits、bus factor 約 1、34 筆 commit 帶 Claude co-author、3.5 個月發了 12 版 |
| 開檔 | `Pdf::new(impl Into<PdfData>)`，**可以零複製使用 mmap**（800 MB 檔 8.9 ms） | 一定要 `Arc<[u8]>`，**大檔會被完整複製**（530 MB 檔 peak 1015 MB） |
| Threading | `Pdf`／`Page` 是 Sync；每個 worker 一份 `RenderCache` | `PdfDocument` 不是 Sync；`DisplayList` 可共用 |
| Tile | `render_into` 可接受任意 transform，但每次呼叫都重新 interpret 整頁 | `page_rect` 子矩形加上 adapter 端 bbox culling，效果很好 |
| 文字 | glyph 層級 Unicode 與位置（自訂 Device） | span 層級 |
| 健壯性 | 364 份 0 panic；**但有無法攔截的 stack overflow**，沒有解壓縮上限與運算預算 | 861 份 0 panic、0 timeout；`catch_unwind` 只包住 JPX；fuzz 只涵蓋 parser |
| 正確性疑慮 | 非內嵌 CJK 字型會變成 Helvetica（需要 resolver） | `/Rotate` 搭配非零原點 box 的位移 bug（有 workaround）；超過 64 MP 會默默降 scale；CPU 與 GPU 像素差異 20.3% |
| GPU | 無 | wgpu backend **不適合** tile viewport（Q5） |
| 授權 | Apache-2.0 OR MIT；74 個依賴全部 permissive | MIT；`timestamp` feature 會引入網路依賴 |

## Decision

1. **Hayro 是 M1 的 baseline engine，也是長期的 fallback 與 reference**（Q6：保留）。adapter：`fastpdf-engine-hayro`，以 git 依賴 pin 在 ≥ `ced00dd0` 的 rev。
2. **zpdf 以 M3 PoC 進入**：`fastpdf-engine-zpdf`，以 git 依賴 pin 在已 audit 的 `fe0ed23`（包含 JBIG2 underflow 安全修正），只開 CPU render，**不開 GPU、不開 `timestamp`**。只透過 cargo feature `engine-zpdf` 編入，預設的 app 與 bench 都不包含。
3. **M4 以數據決定主要 engine**：同一 corpus 比較 correctness（像素比對加人工檢視，重點是 CJK、透明度、旋轉頁）、open、first page、render latency、CPU、peak memory。結果寫成 `docs/engine-comparison.md`，並以新 ADR 記錄選擇。
4. **兩個 engine 都只經過 `GuardedDocument`**；fallback（例如 zpdf 某頁失敗時改用 Hayro render）同樣受 guardrail 保護。
5. **兩者都有 `catch_unwind` 攔不住的失敗模式**（stack overflow、無上限的配置）。中期目標是把 render 放到獨立 process；benchmark 已經是每個檔案一個子 process。另外向 upstream 提出深度上限、取消與預算 callback 的需求。

## Consequences

- M1 的 baseline 數字來自 Hayro。zpdf 的 PoC 結果在 M4 以同一套 harness 比較，避免「先入為主」。
- 維護兩個 adapter 會增加成本，但 spec §48 只禁止「一開始完整實作兩套 renderer」。zpdf PoC 只涵蓋 open、page count、page size、render。
- 兩個 engine 都 pin git rev，升級要刻意進行，並跑 corpus 與 compare。

## Validation

- `fastpdf-bench corpus --engine hayro` 與 `--engine zpdf` 在 quick corpus 上都 0 crash、0 timeout。
- `docs/engine-comparison.md`（M4）。
