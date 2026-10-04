# ADR 0002 — PDF Engine Abstraction

- 狀態：Accepted（M2 前提；實作於 `crates/fastpdf-engine-api`）
- 日期：2026-10-04
- 相關 spec：§4、§6、§7、§24、§25、§42

## Context

Spec §4 要求 UI 與 PDF engine 之間必須隔一層 Reader Core 與 `PdfEngine API`，engine 要能替換（Hayro、zpdf、未來可能是 PDFium）。§6 給了一個以 associated type 表達的 trait 草稿，並明說「實際 API 可以改良」。§7 要求建立自己的 domain types，不讓第三方型別滲透。§24／§25 要求單頁失敗不得讓整個 app crash，且所有輸入都要當成 hostile。

Spec 草稿有幾個實務問題：

1. `type Document; type Error;` 讓 trait 不是 object-safe，無法在 runtime 依 CLI 參數或 fallback 策略切換 engine（§43 要求 runtime／compile feature 都能選）。
2. `render(&self, request)` 沒有 document 參數，request 必須自己帶 document，否則無法實作。
3. 每個 engine 各自的 `Error` 型別會被迫往上傳到 core／UI。
4. 草稿沒有表達取消（§14）、render target 由誰配置（buffer pooling）、以及 engine 能力差異。

## Decision

1. **兩層 object-safe trait**：
   - `PdfEngine`（factory）：`info()`、`open(DocumentSource, &OpenOptions) -> Box<dyn EngineDocument>`。
   - `EngineDocument`（已開啟的文件，`Send + Sync`）：`page_count`、`page_info`、`metadata`、`render`、`text_layer`、`outline`、`links`、`trim_memory`。後四者有預設實作（`Unsupported`／no-op），讓 adapter 可以分階段完成（M3 只需 open／page count／page size／render）。
2. **Domain model 全部在 `fastpdf-engine-api`，且此 crate 零依賴**：`DocumentId`、`PageIndex`、`PageId`、`PageSize`、`Rotation`、`RenderScale`、`PixelRect`、`PageRect`、`RenderRequest`、`RenderOutcome`、`Pixmap`／`PixmapMut`、`TextLayer`／`TextSpan`、`OutlineItem`、`Destination`、`Link`、`DocumentMetadata`、`EngineError`、`ResourceLimits`、`CancelToken`。`TileKey`／`ScaleBucket` 等 render 專屬型別放在 `fastpdf-render`，cache 泛型放在 `fastpdf-cache`。
3. **統一座標系**：
   - Page space（`PageRect`）：point、原點在 CropBox 左上、y 向下、未旋轉。文字框、連結、destination 一律用這個空間，與 zoom／rotation 無關。
   - Pixel space（`PixelRect`）：在某個 `RenderScale` 下、旋轉後的 device pixel，原點左上。tile 就是這個空間裡的一個矩形。
   - PDF user space（左下原點、y 向上、CropBox offset）的轉換由 adapter 負責，engine 型別不外流。
4. **Render 寫入呼叫端提供的 buffer**（`PixmapMut`），pixel format 由呼叫端指定（RGBA 或 BGRA premultiplied），讓 scheduler 能做 buffer pooling，並讓 adapter 直接輸出 GPU 想要的 channel order。
5. **`EngineCapabilities`**：`region_render`、`parallel_render`、`cooperative_cancel`、`text_extraction`、`outline`、`links`、`encryption`、`gpu`。scheduler／UI 依能力調整（例如不支援 parallel 的 engine 只配一個 worker）。
6. **`GuardedDocument` 是唯一被 core 使用的入口**（`open_guarded()`）：
   - 用 `catch_unwind` 把 engine panic 轉成 `EngineError::Panicked`；累計 3 次後 `is_degraded()`，core 應重新開檔而非繼續使用可能不一致的狀態。
   - 在呼叫 engine 之前驗證：page 範圍、region 必須在頁面 pixel bounds 內、target 尺寸與 region 一致、bitmap 尺寸／位元組上限、page 尺寸 sanity、page count 上限。
   - 取消：呼叫前檢查 token；呼叫後再檢查一次，取消後才回來的結果一律丟棄（stale result 不上畫面）。
   - `page_info` 結果快取（有 `max_page_count` 上限保護）。
7. **`panic = "unwind"` 是硬性要求**：release profile 明確設定，因為 `catch_unwind` 在 `panic = "abort"` 下無效。

## Consequences

- 好處：engine 可在 runtime 切換、可做逐頁 fallback（例如 zpdf 失敗時用 Hayro 重 render 該頁，見 ADR 0005）；UI 與 core 完全不知道第三方型別；所有 engine 共用同一套 guardrail 與 panic isolation。
- 成本：每次呼叫多一次 dynamic dispatch 與一次 page-info 查表，相對於 tile render（毫秒級）可忽略。
- `catch_unwind` 無法攔截 stack overflow、abort、或 engine 內部的 `process::exit`；對這類情況需要 process isolation。benchmark harness 已採「每個檔案一個子 process」（見 benchmark plan），GUI 端的 render process isolation 列為未來選項（Risks）。
- Adapter 必須自行把 PDF user space 轉成 page space，這是常見 bug 來源，需要以 fixture（旋轉頁、CropBox offset）做測試。

## Alternatives considered

- **照草稿使用 associated types + generic**：零成本抽象，但 engine 只能 compile-time 選擇，fallback 需要 enum 包裝所有 engine，每加一個 engine 都要改 core。否決。
- **Enum dispatch（`enum AnyEngine { Hayro(..), Zpdf(..) }`）**：可行但讓 core 依賴所有 adapter crate，違反 dependency isolation（§5）。否決。
- **每個 engine 在獨立 process（IPC）**：安全性最好，但開檔延遲與記憶體成本高，V0.1 不採用；保留為未來 hardening 選項。

## Validation

- `crates/fastpdf-engine-api/src/guard.rs` 的單元測試：open／render panic 被攔截並計數、超出範圍的 region 與尺寸錯誤的 target 在進入 engine 前就被拒絕、取消的 request 不會進入 engine。
- M2 完成條件（§42）：`rg "hayro" crates/fastpdf-ui crates/fastpdf-core crates/fastpdf-render` 必須沒有結果；只有 `fastpdf-engine-hayro` 可以 `use hayro::...`。
