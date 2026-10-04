# ADR 0007 — Primary Engine for V0.1: Hayro

- 狀態：Accepted
- 日期：2026-10-04
- 取代：ADR 0005 中「主要 engine 的選擇等 M4 之後決定」的部分
- 相關 spec：§3、§44；證據：`docs/engine-comparison.md`、`benchmarks/baseline.json`

## Context

Spec §3 把 zpdf 列為長期的 primary candidate，Hayro 列為 secondary／reference，並要求由 FastPDF 自己的 benchmark 驗證，§44 要求「讓數據決定」。M3 完成兩個 adapter 後，M4 以同一個 84 檔語料、同一台安靜的機器、release build、每檔 3 個獨立子 process 比較兩者（`docs/engine-comparison.md`）。

## Decision

**V0.1 的預設 engine 是 Hayro**（`fastpdf-engine-hayro`，cargo feature `engine-hayro` 為預設）。zpdf 保留為 feature-gated 的第二 engine（`engine-zpdf`），不進預設 build。

依據：

| 面向 | 結果 |
|---|---|
| 速度 | Hayro 在每個類別的 first page 與頁 render 都快 2–8 倍（corpus 中位數 4.3 vs 21.5 ms；頁 render 3.0 vs 11.3 ms） |
| 記憶體 | 影像類 Hayro 低很多（掃描 30 vs 276 MB）；簡單文件 zpdf 少 5–10 MB |
| 大檔 | Hayro 零複製 mmap；zpdf 必須把整份檔案複製進 `Arc<[u8]>` |
| Hostile 輸入 | Hayro adapter 有靜態掃描、有預算的預先解譯、bomb 預解壓上限；zpdf 依賴自身預算 |
| 台灣使用場景 | 非內嵌 CJK 字型：Hayro adapter 以 PMingLiU 顯示 MSung（明體，字寬正確）；zpdf 用黑體且字寬不符 |
| 正確性缺口 | Hayro：knockout group、alpha soft mask；zpdf：旋轉頁（已 workaround）、64 MP 默默降解析度、shading 固定解析度 |
| 維護 | Hayro：成熟、活躍、被 typst 生態使用；zpdf：4 個月、bus factor 約 1 |

## Consequences

- 預設 build 只包含 Hayro，exe 較小（dist 15.4 MB）。
- 必須補上 Hayro 的透明度缺口：向 upstream 回報或貢獻 knockout group 與 alpha soft mask 的修正（重現檔 `fixtures/generated/transparency/softmask-groups.pdf`）。
- zpdf adapter 繼續由 CI 編譯與測試（`--features engine-zpdf`），用於正確性比對；升級 Hayro 時可以用 `fastpdf-bench diff-corpus` 對照。
- 這個決定基於合成語料與單一高階機器。真實世界語料（`fixtures/local/`）或低階機器的結果若顯著不同，就以新的 ADR 重新評估。

## Validation

- `benchmarks/baseline.json`（M1 baseline，Hayro）。
- `fastpdf-bench diff-corpus --engine hayro,zpdf` 的正確性比對（`docs/engine-comparison.md` §1）。
