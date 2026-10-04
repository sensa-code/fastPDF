# ADR 0006 — Document Loading and Lazy Page Geometry

- 狀態：Accepted（實作於 `crates/fastpdf-core/src/loader.rs`、`crates/fastpdf-render/src/layout.rs`）
- 日期：2026-10-04
- 相關 spec：§10、§11、§25、§29

## Context

- §11：2000 頁、800 MB 的 PDF 開檔時不能 parse 全部、抽全部文字、render 全部縮圖、建全部索引；只做最少量 parse → page count → 第 1 頁 metadata → 第 1 頁 render → UI 立即可用。
- §10：開檔流程不能先初始化所有子系統。
- Engine 都需要隨機存取整個檔案的 bytes。Hayro 與 zpdf 都沒有 streaming／按需讀取 API（見 `docs/audit/hayro.md`、`docs/audit/zpdf.md`）。
- 把 800 MB 整個讀進記憶體違反「大型 PDF 不爆 RAM」，也讓開檔時間與檔案大小成正比。

## Decision

1. **Engine 不碰檔案系統**：`fastpdf-core::loader` 取得 bytes，以 `SharedBytes`（`Arc<dyn AsRef<[u8]> + Send + Sync>`）交給 engine，app 與 benchmark 走同一條路徑。
2. **依大小選策略**（門檻 `MMAP_THRESHOLD = 64 MiB`）：
   - **≤ 64 MiB：一次讀進記憶體**。循序讀取最快；不鎖檔，其他程式可以覆寫或替換檔案（未來可做 auto-reload）。
   - **> 64 MiB：唯讀 memory map**。開檔成本與檔案大小無關，只有 engine 實際碰到的頁面才會被讀入。mapped page 是 file-backed，不算入 commit charge，OS 在記憶體壓力下可以直接丟棄。
3. **Mapping 的 soundness**：Windows 上以不含 `FILE_SHARE_WRITE` 的 share mode 開檔，並在 mapping 存活期間持有這個 handle，所以其他 process 無法以寫入模式開啟該檔（OS 也禁止截斷有 view 的檔案）。如果檔案已被其他 process 以寫入模式開著（`ERROR_SHARING_VIOLATION`），就退回「讀進記憶體」，不冒險 map 一個可能變動的檔案。
4. **Page geometry lazy 化**：開檔只解析第 1 頁的尺寸。`DocumentLayout` 先以第 1 頁尺寸估計所有頁，畫面附近的頁面在顯示前才解析真實尺寸。修正時以 `ScrollAnchor`（頁 + 頁內比例）維持畫面不跳動。fit-width 的 zoom 不會因為捲動經過較寬的頁面而跳動，只在 resize 或明確指令時重算。
5. **Engine adapter 的責任**：能以共享所有權接收 bytes 的 engine 不得複製。必須複製的 engine（zpdf 的 `Arc<[u8]>` 介面）要在 adapter 註解並在 benchmark 中呈現代價（peak RSS），列入 M4 比較。

## Consequences

- 大檔開檔成本 ≈ engine 的最小 parse（xref、trailer、page tree）＋實際碰到的 page fault，不再與檔案大小成正比。
- **大檔開著時無法被其他程式覆寫**（與 Adobe Reader 行為相同；小檔不受影響）。這是為了 mapping 的正確性而接受的 UX 取捨。
- 網路磁碟（SMB）上的 mapped file 若在連線中斷時被存取，Windows 會丟出 `EXCEPTION_IN_PAGE_ERROR`，導致 process crash。**待辦**：以 `GetDriveTypeW == DRIVE_REMOTE` 偵測網路路徑，一律改用讀取（列入 Risks）。
- zpdf adapter 對大檔會產生一份完整複本（800 MB 檔案 → 約 800 MB RSS），除非 upstream 或我們的 fork 改為接受共享 bytes。

## Alternatives considered

- **一律讀進記憶體**（SumatraPDF 的做法）：實作最簡單，但 800 MB 檔案開檔要讀完整個檔案，RSS 也等於檔案大小。否決，但小檔沿用這個策略。
- **Streaming／按需讀取的 engine API**：理想做法，但目前兩個候選 engine 都不支援，需要大幅修改 engine。保留為長期選項。
- **一律 mmap**：小檔多一次 mapping 設定成本，且所有開著的檔案都會被鎖住。否決。

## Validation

- `loader` 單元測試：小檔走讀取、大檔走 mapping，且 mapping 期間無法以寫入模式開檔（Windows）、空檔與不存在的檔案回傳錯誤。
- `DocumentSession` 測試：開檔後遠處頁面仍是估計值；跳頁後附近頁面才解析，畫面停在同一頁。
- Benchmark：`fastpdf-bench` 報告中的 `load_strategy`、`read_ms`、`open_ms` 與 `rss_peak_mb`；large-file fixture（`--profile full`，預設 800 MB）用來驗證開檔時間與 RSS 不隨檔案大小成長。
