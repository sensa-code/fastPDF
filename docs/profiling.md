# Profiling Guide

Spec §32：不要只靠猜測優化。每一次效能相關的修改都要附上 Before／After／Why／Tradeoff（§38），而數據來源就是本文件描述的工具。

## 1. Build profiles

| Profile | 用途 | 指令 |
|---|---|---|
| `dev` | 日常開發；dependency 以 `opt-level = 2` 編譯，讓 render 速度可用 | `cargo build` |
| `release` | Benchmark 基準（thin LTO、line tables） | `cargo build --release` |
| `profiling` | 繼承 release，加上完整 debug symbols、不 strip，給 profiler 用 | `cargo build --profile profiling` |
| `dist` | 發佈與 exe 大小量測（fat LTO、`codegen-units = 1`、strip） | `cargo build --profile dist` |

注意：benchmark 數據只和「同一個 profile、同一個 toolchain（`rust-toolchain.toml` pin 住）」的數據比較。

## 2. 第一步永遠是 fastpdf-bench

```bash
cargo run --release -p fastpdf-bench -- full fixtures/generated/large-text/large-text-300p.pdf
cargo run --release -p fastpdf-bench -- corpus fixtures/generated/manifest.json --out benchmarks/runs/my-change.json
cargo run --release -p fastpdf-bench -- compare benchmarks/baseline.json benchmarks/runs/my-change.json
```

先用 benchmark 確認「哪一個階段變慢」（open、first page、page render、text、thumbnail、peak RSS、CPU time），再用下面的 profiler 找原因。不要反過來。

## 3. Logging／tracing

- 開發時：`FASTPDF_LOG=debug`（或 `FASTPDF_LOG=fastpdf_render=trace,info` 這類 filter 語法）。
- Release 預設低噪音：只有 warn／error。
- 熱路徑（每個 tile、每個 frame）不要放 `info!` 等級以上的 log；用 `trace!` 並確認 release 下的成本。

## 4. Windows 工具

### 4.1 Windows Performance Recorder／Analyzer（首選，免費）

WPR／WPA 隨 Windows ADK 的 Windows Performance Toolkit 提供（`wpr.exe` 在 Windows 11 已內建）。

```powershell
# 系統管理員 PowerShell
wpr -start CPU -start GPU -start FileIO -filemode
# ... 執行要量測的情境（例如開一個大檔、快速捲動）...
wpr -stop fastpdf.etl
wpa fastpdf.etl
```

- Symbol：在 WPA 設定 symbol path 指到 `target\profiling\`（PDB 與 exe 同目錄），再加上 Microsoft symbol server。
- 看什麼：
  - **CPU Usage (Sampled)**：熱點函式（parse、font、image decode、rasterization）。
  - **CPU Usage (Precise)**：thread 喚醒與 context switch，用來驗證 **idle 時沒有任何喚醒**（spec KPI：idle CPU ≈ 0%）。
  - **GPU**：present、DXGI／D3D11 佇列；確認 idle 時沒有持續 present。
  - **File I/O**：開檔時讀了多少 byte。spec §11 要求開大檔只讀最少量資料；mmap 的 page fault 會顯示在 Hard Faults。
- Heap 追蹤（找出配置熱點）：`wpr -heaptracingconfig fastpdf.exe enable`，再用 `wpr -start Heap`。

### 4.2 samply（取樣式 profiler，ETW）

```powershell
cargo build --profile profiling -p fastpdf-bench
samply record target\profiling\fastpdf-bench.exe full path\to\file.pdf
```

需要系統管理員權限（ETW）。結果在本機瀏覽器以 Firefox Profiler 介面開啟；**不要上傳**含有檔案路徑或文件內容的 profile。

### 4.3 cargo flamegraph

Windows 上使用 ETW（blondie），同樣需要系統管理員權限：

```powershell
cargo flamegraph --profile profiling -p fastpdf-bench -- full path\to\file.pdf
```

### 4.4 Visual Studio Profiler

需要完整 Visual Studio（Build Tools 不含）。適合 CPU usage、memory usage（allocation 快照）與 GPU usage 的整合檢視。用 `target\profiling\` 的 exe 搭配同目錄 PDB。

### 4.5 GPU frame capture

- GPUI 在 Windows 使用 Direct3D 11（細節見 `docs/audit/gpui.md`）。D3D11 frame capture 用 RenderDoc；PIX 主要支援 D3D12。
- 用途：確認 tile texture 上傳次數、atlas 大小、每 frame draw call 數量。

## 5. 記憶體

- `fastpdf-bench` 在每個檔案的子 process 中回報 peak working set、peak private bytes（`GetProcessMemoryInfo`）與 CPU time（`GetProcessTimes`）。
- App 執行中：development overlay（M6）顯示各 cache 的 bytes／entries／hit rate／evictions 與 render queue。
- 外部觀察：Process Explorer 的 Private Bytes／Working Set 曲線；`typeperf "\Process(fastpdf)\Private Bytes" -si 1`。

## 6. 常見情境 recipe

| 問題 | 先量 | 再看 |
|---|---|---|
| 開檔慢 | `fastpdf-bench open` 的 `open_ms`／`metadata_ms` | WPA File I/O + CPU Sampled，看 xref／page tree 解析 |
| 第一頁慢 | `first_page_ms` | CPU Sampled：font load、image decode、rasterization 的比例 |
| 捲動卡頓 | App 的 frame time（overlay FPS）、render queue 長度 | CPU Precise：UI thread 是否被 block；GPU：上傳是否集中在同一個 frame |
| Zoom 延遲 | 新 bucket tile 的填滿時間 | scheduler 統計：discarded／cancelled 是否合理 |
| Idle CPU 不為 0 | Task Manager／Process Explorer | CPU Precise：找出誰在喚醒 thread（timer、render loop、log flush） |
| RAM 成長 | bench `rss_peak_mb`、overlay cache 統計 | WPA Heap、`MemoryBudgetManager` snapshot |

## 7. 紀錄格式

每次效能修改在 PR 說明（或 `benchmarks/runs/` 對應紀錄）寫：

```text
Before:   first_page_ms p50 42.6 (baseline.json, large-text-300p.pdf)
After:    first_page_ms p50 31.0
Why:      font program 改為第一次使用時才解析
Tradeoff: 第一次出現新字型的頁面多 2 ms；peak RSS -3 MB
```

達不到目標也要照實紀錄（spec §29：不要作弊）。
