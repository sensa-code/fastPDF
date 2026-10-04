# ADR 0003 — Tile Rendering, Scale Buckets and the Render Scheduler

- 狀態：Accepted（資料結構與 scheduler 已實作於 `crates/fastpdf-render`；tile size 與 worker 數待 benchmark 定案）
- 日期：2026-10-04
- 相關 spec：§1、§12、§13、§14、§17、§18

## Context

- §12 禁止以「每頁完整 bitmap」作為唯一 rendering model，zoom 到 600% 時只能 render 與 viewport 相交的 tile。
- §13 zoom 時 UI 必須一直有畫面：先用既有低解析 texture 暫時放大，再逐步換成高解析 tile。
- §14 需要獨立 scheduler，P0–P5 優先序，過期 request 要能 cancel／discard／reprioritize。
- §17 tile cache key 要含 document、page、tile x/y、render scale、rotation、color mode，而且 zoom 不能產生無限種類的 cache。
- §18 rendering 不可 block UI thread，worker 數不能等於核心數。

## Decision

1. **Tile grid**：頁面在某個 render scale 下旋轉後的 pixel 空間切成正方形 tile（`TileGrid`），邊緣 tile 裁切到頁面大小。預設 512 px（`DEFAULT_TILE_SIZE`），最小 64 px；256 與 512 的取捨由 benchmark 決定（benchmark plan B-3）。
2. **Scale bucket**：display scale = zoom × window scale factor（1.0 = 96 dpi 下的 100%）。tile 只會以 26 個固定 bucket 之一 render（0.125 … 48）。這些 bucket 剛好涵蓋 Windows 常見的 scale factor（1.25、1.5、1.75、2.0），所以最常見的情況不需要任何縮放。任意 zoom（例如 137%）會選「略低於需求但放大 ≤ 6%」的 bucket，否則選上一個 bucket（縮小顯示，文字較銳利）。cache 種類因此有上界（每頁 × 每 rotation × 26）。
3. **TileKey** = `PageId`（document + page）+ `ScaleBucket` + user `Rotation` + `ColorMode` + tile size + `TileCoord`。page 的 intrinsic `/Rotate` 每頁固定，不需要放進 key。
4. **Layout 與 viewport**：`DocumentLayout` 以 point 為單位垂直排列所有頁面；未知頁面先用估計尺寸（通常是第 1 頁的尺寸），之後再修正（spec §11：開檔不需要解析全部頁面）。修正尺寸時以 `ScrollAnchor`（頁 + 頁內比例）維持畫面不跳動。`Viewport` 的 scroll offset 以 point 表示，所以 zoom 時不會漂移；`zoom_around` 讓滑鼠位置下的內容保持不動（Ctrl + 滾輪）。
5. **Tile planning**（`plan_tiles`）：
   - P0 `Visible`：與 viewport 相交的 tile。
   - P1 `VisiblePage`：可見頁面中，在 near margin 內但不在畫面上的 tile。
   - P2 `Near`：near margin（預設上下各半個 viewport 高）內其他頁面的 tile。
   - P3 `Prefetch`：near 區域之後下一頁的第一個畫面。
   - P4 `Thumbnail`、P5 `Background`：保留給 sidebar 與搜尋。
   - 同一優先序內依與 viewport 中心的距離排序。page info 尚未解析的頁面不規劃。near margin 以外的東西永遠不規劃（§1 "Never render what the user cannot see"）。
6. **RenderScheduler**：
   - 固定大小的 worker pool（預設 `available_parallelism / 4`，限制在 2–4，待 2/4/6/8 benchmark 定案）。不支援 `parallel_render` 的 engine 由 core 配置單一 worker。
   - `submit_plan()` 以新 plan 取代整個 queue：不再需要的 queued job 直接丟棄（discarded），仍在 render 但已不需要的 job 透過 `CancelToken` 取消（cancelled），仍需要且正在 render 的 job 保留，不重複排入。
   - 結果透過 sink callback 在 worker thread 交出（UI 端只做 channel push + 喚醒），被取消的 job 不產生結果。統計數據（queued／in-flight／completed／failed／cancelled／discarded）供 development overlay 使用。
7. **Progressive rendering（§13）**：UI 在新 bucket 的 tile 抵達之前，先用 cache 中同頁其他 bucket 的 tile 縮放顯示（`ScaleBucket::screen_factor`）；新 tile 抵達後逐塊替換。這部分邏輯在 M5 由 core／UI 實作。

## Consequences

- 600% zoom 的 Letter 頁面（4896 × 6336 px，約 120 MB RGBA）在 1920×1080 viewport 下最多只需要約 20 個 512 px tile（約 20 MB）。
- 快速捲動時，舊 plan 的工作在下一次 `submit_plan` 就被丟棄或取消；engine 若支援 cooperative cancel，in-flight 工作也會提早結束。
- 不支援 region render 的 engine（`region_render: false`）每個 tile 都要完整 rasterize 整頁，成本 = tile 數 × 整頁。這是選擇 engine 的關鍵指標之一，已列入 M4 比較。
- 每個 tile 的邊界都是整數 pixel，相鄰 tile 由同一個 render scale 產生，不會有接縫；但 anti-aliasing 在 tile 邊界可能有 1 px 差異，需要以截圖比對確認（M5 驗收項目）。

## Alternatives considered

- **整頁 bitmap + mipmap**：實作簡單，但高 zoom 時記憶體爆炸，違反 §12。
- **任意 scale 直接當 key**：cache 無限碎片化，違反 §17。
- **每次 zoom 都重 render 全部可見 tile 且不顯示舊內容**：畫面閃爍／空白，違反 §13。

## Validation

- `fastpdf-render` 單元測試：600% zoom 只規劃相交 tile、可見 tile 優先、未知頁面不規劃、旋轉後 tile 幾何正確、快速捲動時舊工作被丟棄或取消、失敗頁面不影響其他頁、scheduler drop 時 worker 會結束。
- Benchmark：tile size（256 vs 512）、worker 數（2/4/6/8）對「viewport 填滿時間」與 RAM 的影響（benchmark plan B-3、B-4）。
