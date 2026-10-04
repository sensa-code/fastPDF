# ADR 0004 — Memory Budget

- 狀態：Accepted（實作於 `crates/fastpdf-cache`；各 cache 的實際預算待 benchmark 調整）
- 日期：2026-10-04
- 相關 spec：§15、§16、§46

## Context

- §15：每個 cache 都必須有 budget，可以用 LRU、weighted LRU 或 cost-aware eviction。起始數字：Font 32 MB、Decoded Image 64 MB、Render Tile 128 MB、Thumbnail 32 MB、Text 32 MB。
- §16：需要統一的 `MemoryBudgetManager`，分 Normal／Soft／Hard；soft 開始積極 eviction，hard 立即移除低優先 cache；優先保留 visible tiles、current page、critical font state；優先丟 far pages、thumbnails、old zoom level、old tiles、background text。
- §46：所有重要 cache 都要可觀察：current bytes、entry count、hit rate、miss rate、eviction count。

## Decision

1. **`ByteLru<K, V>`**：以 byte 計重的 O(1) LRU（slab + 雙向鏈結串列），每個 entry 插入時聲明重量。被淘汰的 entry 會回傳給呼叫端，讓 GPU texture、pooled buffer 等資源能確定性地釋放。內建統計：bytes、budget、entries、hits、misses、inserts、evictions、evicted bytes。
2. **`SharedCache<K, V>`**：thread-safe 包裝，有 eviction hook（例如把要釋放的 GPU image 排進 UI thread 的釋放佇列），以及 **protected bytes floor**：UI 每個 frame 會 touch 畫面上的 tile（使其成為 MRU），並宣告它們的總位元組數；memory pressure 釋放時不會把 cache 縮到這個 floor 以下，所以目前畫面不會被清空（§16「優先保留 visible tiles」）。
3. **`MemoryBudgetManager`**：
   - 以 `Weak` 持有註冊的 cache（cache 被 drop 就自動註銷）。
   - 輸入為「cache 總量 + 呼叫端提供的 external bytes」。external bytes 由 core 量測（Windows：process private bytes 減掉 cache 總量），涵蓋 engine 內部 cache（字型、decoded image）等我們無法直接控制的部分。manager 本身保持平台中立、可測試。
   - Normal：各 cache 在 insert 時自行遵守自己的 budget。
   - Soft（預設 320 MB）：依 retention 由低到高縮小 cache，直到總量回到 soft limit 的 85%。
   - Hard（預設 512 MB）：先把 retention 低於 `CRITICAL` 的 cache（thumbnails、text、prefetch tiles）全部清空，再依 Soft 規則縮小其餘 cache；同時透過 `EngineDocument::trim_memory(Hard)` 要求 engine 丟掉內部 cache。
4. **Retention 等級**（`fastpdf_cache::retention`）：THUMBNAILS 10 < TEXT 20 < PREFETCH 30 < CRITICAL 50 < TILES 60。對應 §16 的「優先丟」順序；「old zoom level / old tiles / far pages」靠 tile cache 自身的 LRU 順序處理（畫面上的 tile 每個 frame 都會被 touch）。
5. **起始 budget**（可由設定覆寫，benchmark 後調整）：

   | Cache | Budget | Owner | Retention |
   |---|---|---|---|
   | Render tiles | 128 MB | `fastpdf-render`／UI | TILES |
   | Prefetch tiles | 包含在 tile cache 內 | — | — |
   | Thumbnails | 32 MB | UI sidebar | THUMBNAILS |
   | Text layers | 32 MB | `fastpdf-search` | TEXT |
   | Fonts | 32 MB（engine 內部） | engine adapter | 透過 `trim_memory` |
   | Decoded images | 64 MB（engine 內部） | engine adapter | 透過 `trim_memory` |

## Consequences

- 每個 cache 都有上限，且 manager 可以在不了解 cache 內容的情況下釋放記憶體。
- Engine 內部 cache 只能透過 `trim_memory` 間接控制；如果 engine 不支援，hard limit 時唯一的手段是重新開檔（丟掉整個 engine document）。各 engine 的支援程度列入 M4 比較。
- external bytes 的量測頻率需要節制（例如每秒一次或每次 tile 抵達時節流），以免 idle 時產生 CPU 喚醒（§1 idle CPU ≈ 0）。只在有工作時量測。

## Alternatives considered

- **以 entry 數量為上限的 LRU**：tile 尺寸不一（邊緣 tile、不同 bucket），entry 數無法反映實際記憶體。否決。
- **全域 allocator 層級的限制**：無法決定要丟什麼，只能讓配置失敗。否決。

## Validation

- `fastpdf-cache` 單元測試：依 byte 淘汰 LRU、超大 entry 不破壞 budget、replace 更新重量、shrink／retain／iteration 順序、slot 重用、soft 先丟低 retention、hard 清空非 critical cache、protected bytes 在 relief 後仍存在、cache drop 後自動註銷。
- M6：development overlay 顯示各 cache 的 bytes／entries／hit rate／evictions；用 large-page-count 與 large-file fixture 捲動 2000 頁，確認 RSS 曲線有上限（benchmark plan B-5）。
