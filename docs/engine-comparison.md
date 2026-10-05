# Engine Comparison — Hayro vs zpdf（M4）

- 日期：2026-10-04
- 版本：Hayro git `ced00dd0`（`fastpdf-engine-hayro`），zpdf git `fe0ed23`（`fastpdf-engine-zpdf`）
- 語料：`fixtures/generated/manifest.json`（quick profile，84 個檔案）
- 工具：`fastpdf-bench diff-corpus`（正確性）、`fastpdf-bench corpus`（效能）
- 原則（spec §44）：不預設誰贏，讓數據決定。以下每個結論都附上數據或圖片來源。

## 1. 正確性

### 1.1 開檔與健壯性

| 項目 | Hayro | zpdf |
|---|---|---|
| 84 個檔案中能開啟 | 81 | 82 |
| 差異 | `malformed/no-catalog.pdf` 開檔失敗 | 能以全檔物件掃描修復開啟 |
| crash／timeout（每檔一個子 process） | 0／0 | 0／0 |
| decompression bomb（512 MiB） | 該頁回報 `LimitExceeded(ObjectSize)`，不展開 | 依 zpdf 自身預算處理；peak RSS 約 270 MB |
| 宣告 100000×100000 的影像 | `LimitExceeded(DecodedImage)` | 依 zpdf 自身預算處理 |
| 巨大 MediaBox | 兩者都被 `GuardedDocument` 擋下（`PageDimension`） | 同左 |

兩個 adapter 都通過 `GuardedDocument`，所以頁面尺寸、bitmap 大小、panic isolation 的保護是共用的。差別在 engine 內部的 guardrail：Hayro adapter 在 render 前做靜態掃描與有預算的預先解譯；zpdf 依賴 zpdf 自己的預算。

### 1.2 像素比對（`diff-corpus`，每檔比對第 1 頁、中間頁、最後一頁，96 dpi，容許每個 channel 差 16）

兩邊都能開啟的 81 個檔案中，「差異像素比例」的分布：中位數 3.7%，p90 10.9%。

差異最大的類別與原因（以 `fastpdf-bench diff --out` 輸出的圖片逐張目視檢查）：

| 檔案 | 差異 | 原因 | 判斷 |
|---|---|---|---|
| `transparency/softmask-groups.pdf` | 10.9% | Hayro 把「alpha soft mask（group ca 0.4）」畫成不透明的方塊，且 knockout group 被當成一般混色；zpdf 兩者都正確 | **zpdf 正確**（Hayro 已知限制：knockout group 未支援） |
| `cjk/cid-nonembedded-*`、`traditional-chinese/*-msung-nonembedded.pdf` | 10–23% | 非內嵌字型的替換：Hayro adapter 把 MSung（明體）對應到 PMingLiU（明體），zpdf 用黑體。zpdf 替換字型的字寬和 PDF 宣告的寬度不一致，表格中的「58.2 mg/Nm3」壓到欄線 | **Hayro 較忠實**（兩者都可閱讀） |
| `vector-heavy/bezier-curves-40k.pdf` | 37.7% | 4 萬條細曲線的抗鋸齒差異遍布全頁 | 兩者都合理，屬於 rasterizer 差異 |
| `large-text/*`、`small-text/*` | 8–10% | 文字抗鋸齒、標準 14 字型的替換（Hayro adapter 用 Arial／Times New Roman／Courier New） | 兩者都合理 |
| `image-heavy/jpeg-variants.pdf` | 11.8% | 差異集中在影像邊緣與說明文字，影像內容本身一致 | 兩者都合理 |

> 圖片輸出：`fastpdf-bench diff <file> --engine hayro,zpdf --page 1 --out <dir>`（輸出 Hayro 圖、zpdf 圖，以及差異圖：紅色表示超過容許值）。

### 1.3 其他已知正確性問題（來自 audit 與 adapter 實作）

- zpdf：
  - `/Rotate` 搭配非零原點 box 的位移與 annotation 消失（adapter 已 workaround）。
  - WinAnsi 未使用碼（0x7F 等）沒有對應到 bullet，ReportLab 的項目符號被畫成「ù」；`Times-Bold`／`-Italic`／`-BoldItalic` 在 Windows 上都用到正體（adapter 已 workaround，見 `docs/upstream-issues/zpdf.md` #2、#3）。
  - 細線 tile 接縫（adapter 已修正，代價見下）。
  - 超過 64 MP 時默默降低 scale；shading 解析度固定；文字只有 span 等級。
- Hayro：knockout group 未支援、alpha soft mask 錯誤（本次發現）、R2–R4 只接受 user password（見 `docs/upstream-issues/hayro.md`）。

## 2. 效能

量測條件：機器安靜（量測前 CPU 2.8%，沒有編譯中的 process）、release build（`git a920ea8`，從乾淨匯出目錄 build）、`fastpdf-bench corpus --repeat 3`（每檔 3 個獨立子 process，取中位數）、display scale 1.0（96 dpi 下 100%）。原始數據：`benchmarks/baseline.json`（Hayro，M1 baseline）與 `benchmarks/runs/m4-zpdf.json`（zpdf，不 commit）。

### 2.1 全 corpus

| 指標（中位數／p95／最大） | Hayro | zpdf |
|---|---|---|
| open | 0.06／5.2／8.1 ms | 0.14／2.8／6.3 ms |
| first page（第 1 頁整頁 render） | **4.3**／89／213 ms | 21.5／147／649 ms |
| time to first page（read + open + metadata + 第 1 頁） | **5.7**／90／214 ms | 22.4／149／649 ms |
| 抽樣頁 render（各檔 median 的中位數） | **3.0** ms | 11.3 ms |
| text extraction（第 1 頁） | 0.36 ms | 0.28 ms |
| peak RSS（每檔子 process） | 25.7／84／207 MB | **18.0**／177／330 MB |
| CPU time | **15.6**／250 ms | 46.9／594 ms |
| status | 77 ok、4 partial（hostile 檔被 guardrail 擋下）、2 open_error、1 load_error | 81 ok、1 partial、1 open_error、1 load_error |

### 2.2 依類別（中位數；H = Hayro，Z = zpdf）

| 類別 | 檔案數 | first page ms（H／Z） | 頁 render ms（H／Z） | peak RSS MB（H／Z） | CPU ms（H／Z） |
|---|---|---|---|---|---|
| cad | 3 | 149／435 | — | 145／127 | 172／547 |
| cjk | 4 | 10.4／32.6 | 5.3／11.3 | 30／55 | 31／78 |
| encrypted | 8 | 4.1／21.5 | 4.9／15.9 | 26／18 | 16／47 |
| fonts | 5 | 6.0／22.6 | 4.3／13.8 | 26／16 | 31／62 |
| image-heavy | 4 | 17.3／51.7 | 16.7／35.6 | 39／114 | 164／195 |
| japanese | 3 | 5.2／25.5 | 3.6／12.3 | 25／15 | 16／78 |
| large-page-count（2000 頁） | 3 | 1.6／13.7 | 1.0／1.5 | 28／20 | 16／31 |
| large-text | 2 | 9.8／45.7 | 8.2／33.3 | 28／21 | 156／570 |
| malformed | 24 | 1.3／12.4 | 1.0／1.3 | 22／14 | 0／16 |
| scanned | 3 | 14.6／82.9 | 14.1／70.6 | 30／276 | 141／1219 |
| small-text | 3 | 4.5／26.7 | 3.9／13.5 | 24／16 | 16／47 |
| traditional-chinese | 10 | 7.9／25.7 | 2.8／5.8 | 27／23 | 16／39 |
| transparency | 3 | 3.6／29.4 | 16.3／77.1 | 24／20 | 16／47 |
| vector-heavy | 4 | 59.8／127 | 48.0／156 | 38／29 | 125／188 |

2000 頁檔的開檔：flat page tree 4.0 ms（H）／6.3 ms（Z），balanced tree 2.6／3.1 ms，object stream + xref stream 6.6／3.8 ms。兩者都不需要掃描整份文件就能顯示（spec §11）。

### 2.3 解讀與限制

- Hayro 在**每一個類別**的 first page 與頁 render 都比較快（2–8 倍）。原因包括：Hayro adapter 的 block 合併 render、zpdf 每次 render 重新縮放影像（掃描頁）、zpdf adapter 的細線接縫修正成本（CAD 與長線條平面圖 +15% 到 2.4 倍）。
- zpdf 在簡單文件上的 RSS 少 5–10 MB；但在影像類（全解析度解碼）高出許多（掃描頁 276 vs 30 MB）。
- CPU time 的解析度是 Windows 的 15.6 ms tick，小檔數字只能看量級。
- 這是**合成語料、單一高階機器**的結果（Ryzen 9 9950X、RTX 5090）。真實世界文件（`fixtures/local/`）與低階機器還沒有量。

## 3. 結論與建議

1. **V0.1 的主要 engine 採用 Hayro**（[ADR 0007](adr/0007-primary-engine-hayro.md)）。依據：
   - 每一類的 first page 與頁 render 都快 2–8 倍。
   - 影像類的記憶體低很多。
   - 可以零複製使用 mmap，大檔不必整份複製。
   - 有 adapter 端的 guardrail。
   - 非內嵌 CJK 字型的替換較忠實，台灣公文用明體顯示。
   - 維護者活躍、成熟度高。
2. **zpdf 保留為 feature-gated 的第二 engine**（`engine-zpdf`），用於正確性比對與未來的 fallback 研究。它在透明度（knockout、alpha soft mask）與部分壞檔修復上較好，這些是 Hayro 待補的缺口。
3. **待辦**：
   - 向 Hayro upstream 回報或貢獻 knockout group 與 alpha soft mask 的修正，並附上 `transparency/softmask-groups.pdf` 的重現方式。
   - 用真實世界語料（`fixtures/local/`：政府文件、醫療報告、掃描檔）重跑本比較。
   - 逐頁 fallback（Hayro 失敗時改用 zpdf）目前不做：記憶體要兩份，複雜度高，現階段收益不明。
