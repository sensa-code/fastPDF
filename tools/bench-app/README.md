# tools/bench-app：App 層 KPI 量測工具（Benchmark Plan B-8）

用途：從 app **外部**量測 SPEC §28–§29 的 app 層 KPI，並在同一套方法下比較 FastPDF、pdf-reader-gpui upstream、SumatraPDF、Adobe Acrobat Reader 與瀏覽器。

可量測的指標：
- 啟動到視窗出現
- 第一個非空白畫面
- 視覺完成時間
- idle RAM／CPU
- peak RAM
- 捲動與縮放的反應時間

Engine 層的數字（open、first page render、text extraction）不在這裡，請用 `fastpdf-bench`（B-0、B-1）。

| 檔案 | 說明 |
|---|---|
| `bench-app.ps1` | 主程式（PowerShell 7 為主，Windows PowerShell 5.1 也可以執行） |
| `BenchApp.cs` | C# helper，由 `Add-Type` 在執行時編譯，**沒有任何外部依賴**（截圖用 GDI，PNG 用自寫的 encoder） |
| 輸出 | `benchmarks/runs/app-<label>.json`，截圖放在 `benchmarks/runs/app-shots/`。該目錄已被 `.gitignore` 排除，**不 commit** |

---

## 快速開始

```powershell
# 偵測本機已安裝的 competitor（只查 App Paths 登錄機碼與標準安裝路徑），結果寫到 benchmarks/runs/app-detect.json
pwsh -File tools/bench-app/bench-app.ps1 -Detect

# upstream 對照組：空視窗啟動 3 次
pwsh -File tools/bench-app/bench-app.ps1 -Preset upstream -Label upstream-pdf-reader-gpui -Scenario launch-empty -Runs 3

# upstream：用它自己的檔案對話框開檔，加上捲動與 PageDown
pwsh -File tools/bench-app/bench-app.ps1 -Preset upstream -Label upstream-pdf-reader-gpui `
    -Pdf fixtures/generated/large-text/dense-300p-times.pdf -Runs 3 -ScrollNotches 20 -PageDowns 5

# 瀏覽器：使用獨立的暫存 profile，1 次 warm-up 加 3 次量測
pwsh -File tools/bench-app/bench-app.ps1 -Preset edge -Pdf fixtures/generated/large-text/dense-300p-times.pdf `
    -WarmupRuns 1 -Runs 3 -ScrollNotches 20 -ZoomSteps 3

# 任意 exe（placeholders：{pdf} {pdf_uri} {profile}）
pwsh -File tools/bench-app/bench-app.ps1 -Exe C:\path\viewer.exe -Args '{pdf}' -Pdf some.pdf -Label myviewer

# 不重新啟動 app，只重算既有結果檔的 summary
pwsh -File tools/bench-app/bench-app.ps1 -Resummarize -OutFile benchmarks/runs/app-edge.json

# 1.1.0：idle 歸因（逐 thread 的 cycles 與喚醒次數、記憶體分類）
pwsh -File tools/bench-app/bench-app.ps1 -Preset fastpdf -Pdf fixtures/generated/small-text/three-pages-platypus-times.pdf `
    -Runs 3 -ThreadDetail -MemoryDetail

# 1.1.0：近似低核心機器（4 個實體核心，各取一個邏輯 CPU；本機 SMT 兄弟是相鄰編號）
pwsh -File tools/bench-app/bench-app.ps1 -Preset fastpdf -Pdf fixtures/generated/small-text/three-pages-platypus-times.pdf `
    -Runs 3 -Affinity 0x55 -Label fastpdf-4core

# 1.3.0：近似入門筆電（2 核 2 緒，每個核心約 1/6 速度），並傳環境變數給 app
pwsh -File tools/bench-app/bench-app.ps1 -Preset fastpdf -Pdf fixtures/generated/three-pages-platypus-times.pdf `
    -Runs 3 -Affinity 0x5 -Slowdown 6 -AppEnv FASTPDF_LOG=info -Label fastpdf-entry
```

同一個 `-OutFile` 可以放多個 scenario。scenario 名稱相同的資料會被取代，名稱不同就附加。

`-Affinity` 只限制 CPU 數；1.3.0 的 `-Slowdown` 另外讓每個核心變慢（duty cycle）。快取、記憶體頻寬、儲存裝置與 GPU 仍是本機的，所以兩者合起來也只能近似低階機器，不能代表它的絕對時間（見〈方法限制〉第 10 點）。選 mask 前先確認哪些邏輯 CPU 是同一個實體核心（SMT 兄弟）。低階機器的完整量測見 `docs/benchmarks/low-end.md`。

### Presets

| Preset | 執行檔 | 開檔方式 | profile 隔離 | 輸入方式 | 結束方式 |
|---|---|---|---|---|---|
| `fastpdf` | `target/release/fastpdf.exe`（或 `target/dist/`） | `fastpdf <file.pdf>` | 不需要 | 滾輪；Ctrl 狀態注入 | WM_CLOSE，逾時則 kill |
| `upstream` | `upstream/pdf-reader-gpui/target/release/pdf-reader-gpui.exe` | 不接受 CLI 參數，自動操作它的「Select a PDF file」按鈕與通用檔案對話框（`WM_SETTEXT` + `BM_CLICK`） | 不需要 | 滾輪 | kill |
| `edge`、`chrome` | App Paths 登錄機碼或標準路徑 | `--new-window file:///…`，帶一組 Chromium 旗標 | `--user-data-dir` 指到暫存目錄 | 安全滾輪；必要時改用鍵盤 | WM_CLOSE，之後結束殘留子 process |
| `firefox` | 同上 | `-profile <暫存> -no-remote -new-instance` | 暫存 profile，加上 `user.js` 關閉歡迎頁與資料回報 | 同 Chromium | 同上 |
| `sumatra` | 同上 | `-appdata <暫存> <pdf>` | 暫存 appdata，設定 `CheckForUpdates = false` | 鍵盤 | WM_CLOSE |
| `acrobat` | 同上 | `<pdf>` | **無法隔離**，必須加上 `-AllowSharedProfile` 才會執行 | 鍵盤 | WM_CLOSE |

`firefox`、`sumatra`、`acrobat` 三個 preset 因為本機沒有安裝，**尚未實測**。

### 主要參數

| 參數 | 預設 | 說明 |
|---|---|---|
| `-Runs` / `-WarmupRuns` | 3 / 0 | warm-up run 不做互動，也不列入 summary |
| `-IdleSeconds` | 10 | idle 取樣長度（開檔或啟動穩定後再等 1 秒才開始） |
| `-StableFrames` / `-QuietMs` | 5 / 1500 | 穩定判定：連續 N 張截圖沒有變化，**而且**至少 QuietMs 毫秒沒有任何變化 |
| `-TimeoutSec` | 30 | 等待視窗或穩定的上限 |
| `-ScrollNotches` / `-PageDowns` / `-ZoomSteps` | 0 | 互動量測，0 表示不做 |
| `-InputMode` | auto | `wheel`、`keys`，或依 preset 自動決定 |
| `-CaptureStdout` | fastpdf 預設開啟 | 設定 `FASTPDF_BENCH=1` 並解析 stdout 的 JSON 行 |
| `-ProfileRoot` / `-KeepProfiles` | `%TEMP%\fastpdf-bench-app` | 瀏覽器等 app 的暫存 profile 位置；預設跑完就刪除 |
| `-CacheState` | warm | 只用來標記（見〈Cold 與 warm〉） |
| `-LaunchBudgetFile` / `-MaxLaunches` | 無 / 25 | 跨多次呼叫累計 GUI 啟動次數，超過上限就拒絕執行 |
| `-NoScreenshots` | 關 | 預設會存第一個非空白畫面、穩定後畫面、每次互動後的截圖 |
| `-Affinity <mask>` | 無 | 1.1.0。限制 app 可用的邏輯 CPU（`0x55` 這類十六進位或十進位），用來近似核心數較少的機器。只能指定第一個 processor group（前 64 個邏輯 CPU）。**1.3.0 起**改用 job object：app 以暫停狀態建立、加入帶 affinity 限制的 job 之後才開始執行，所以限制從第一個指令就生效，之後建立的子 process 也在 job 裡。1.2.0 是在啟動後才設定，FastPDF 啟動時建立的第一個 render host 可能不受限制 |
| `-Slowdown <倍數>` / `-SlowdownPeriodMs` | 1（關）/ 6 | 1.3.0。duty cycle：每個週期內，job 裡的所有 process 只執行 1/倍數 的時間，其餘時間以 `NtSuspendProcess` 暫停（Chrome DevTools 的 CPU throttling 也是這種做法），用來近似每個核心較慢的 CPU。控制 thread 用 spin 計時，有 `-Affinity` 時固定在 app 用不到的邏輯 CPU 上。統計記錄在 `slowdown` |
| `-AppEnv NAME=value` | 無 | 1.3.0。額外傳給 app 的環境變數（`NAME=` 表示空值），記錄在 `config.app_env` |
| `-ThreadDetail` | 關 | 1.1.0。idle 期間逐條 thread 的 CPU cycles 與 context switch（見〈Idle 診斷〉） |
| `-MemoryDetail` | 關 | 1.1.0。idle 結束時以 `VirtualQueryEx` 分類 committed memory，並把 working set 依同樣的類別拆開（見〈Idle 診斷〉） |
| `-AppProbe` | preset `fastpdf` 開啟，其他關閉 | 1.2.0 起改為讀 FastPDF 的 frame 計數，在 idle 窗口、互動後 idle 窗口與每項互動的前後讀取（見〈-AppProbe：FastPDF 的 frame 計數〉）。會一併開啟 `-CaptureStdout` |
| `-TopThreads` | 20 | `-ThreadDetail` 列出的 thread 數 |

---

## 量測方法與指標

時間 0 是 `Process.Start` 被呼叫前一刻（Stopwatch）。所有記憶體與 CPU 數字都是**整個 process tree 的總和**：

- process tree = 啟動的 process，加上啟動之後才建立的所有後代；
- 用 Toolhelp snapshot 追蹤，並比對建立時間，避免 PID 被重複使用造成誤判；
- 對每個 process 都持有 handle，所以已結束的 process 仍能取得最終 CPU time，CPU 差值不會因為 process 結束而變成負值。

| 指標 | 定義 |
|---|---|
| `t_window_ms` | process tree 中第一個可見、未 cloak、client 區至少 200×150 的 top-level 視窗出現的時間（約每 2–5 ms 輪詢一次） |
| `launch.t_first_nonblank_ms` | 第一張「非空白」截圖：在 4 px 取樣網格上至少有 3 種顏色 |
| `launch.t_visual_complete_ms` | 最後一次畫面變化的截圖時間，也就是之後畫面不再改變。`*_lower_ms` 是前一張截圖的時間（真實時間點落在兩者之間） |
| `launch.t_stable_detected_ms` | 判定穩定的時刻，比視覺完成多出 QuietMs，只用於除錯 |
| `open.*`（upstream） | 以按下 Open 那一刻為 0：`t_first_change_ms` 是畫面第一次變化，`t_visual_complete_ms` 是視覺完成 |
| `idle.*` | 穩定後 1 秒開始的 `IdleSeconds` 秒：private bytes、working set、threads、handles、CPU 差值（`cpu_pct_of_one_core`），以及 GPU dedicated/shared（效能計數器 `GPU Process Memory`） |
| `idle.private_ws_mb` | 1.1.0。private working set，即工作管理員「記憶體」欄（`PROCESS_MEMORY_COUNTERS_EX2.PrivateWorkingSetSize`） |
| `idle.shared_commit_mb` | 1.1.0。`SharedCommitUsage`：算在這個 process 的共享 commit（pagefile-backed section） |
| `idle.commit_charge_mb` | 1.1.0。`private_mb + shared_commit_mb`。`private_mb` 本身就是 `PrivateUsage`，也就是 Microsoft 文件中的 commit charge、工作管理員的「認可大小」 |
| `idle.system_cpu_busy_pct` | 1.1.0。idle 期間整台機器所有邏輯 CPU 的忙碌比例（`GetSystemTimes`，含 app 自己），用來記錄背景負載 |
| `peak.private_ws_mb_sum` / `peak.commit_charge_mb_sum` | 1.1.0。與 `peak.*` 同樣取樣方式下，上面兩個值的 tree 總和最大值 |
| `post_interaction_idle.private_ws_mb` 等 | 1.1.0。互動後 idle 的同一組三個欄位 |
| `affinity`、`config.affinity_mask` | 1.1.0。`-Affinity` 的 mask、邏輯 CPU 數、設定完成的時間點（ms）。1.3.0 起時間點是 0，另記 root 加入 job 後的 mask（`root_mask_in_job`） |
| `launch_mode`、`config.launch_mode` | 1.3.0。`suspended-in-job`（有 `-Affinity` 或 `-Slowdown`）或 `process-start` |
| `slowdown` | 1.3.0。`-Slowdown` 的統計：週期數、實際執行比例 `run_share`、`effective_factor`（= 1 / run_share）、最大週期超時 `max_cycle_overrun_ms`、暫停失敗次數、同時存在的最大 process 數、idle 窗口不降速的時間 `paused_ms`。暫停呼叫本身的時間算在執行時間內，所以 `effective_factor` 是下限 |
| `idle.app_frames`、`post_interaction_idle.app_frames`、`app_probe` | 1.2.0。窗口內 FastPDF 的 render、prepaint、paint、wake 次數（見〈-AppProbe：FastPDF 的 frame 計數〉）。summary 中同名的 `idle.app_frames` 是判定標記 |
| `peak.*` | 錄影期間約每 250 ms 取樣一次，加上各階段邊界的取樣，取 tree 總和的最大值 |
| `capture_overhead` | 校正用：只截圖、不送輸入約 2.5 秒，期間目標 app 消耗的 CPU（ms/s）。互動的 `cpu_ms_net_of_capture` 已扣除這個量 |
| `scroll/pagedown/zoom.*` | `latency_first_change_ms` 是第一個輸入送出到畫面第一次變化；`settle_after_last_input_ms` 是最後一個輸入到最後一次變化；`changed_frames_per_s_during_input` 是輸入期間每秒有變化的截圖數（上限約等於截圖頻率）；`effect=false` 表示輸入沒有造成任何畫面變化，**這類 run 不列入 summary**，但保留在原始資料中 |
| `fastpdf_bench.events_t_ms` | 見下一節 |
| `process_created_offset_ms` | process 建立時間相對於工具時鐘 0 點的偏移，用來和 app 自報的時間對齊 |

截圖使用 `PrintWindow(PW_CLIENTONLY | PW_RENDERFULLCONTENT)` 擷取 client 區，所以即使視窗被其他視窗遮住，也能拿到 app 自己的畫面。最小間隔 15 ms，每張實際耗時約 15–18 ms，因此時間解析度約 ±20–35 ms。

### FASTPDF_BENCH 協定（`crates/fastpdf-app/src/bench.rs`）

app 以 `FASTPDF_BENCH=1` 啟動時，每個里程碑在 stdout 印一行 JSON。`t_ms` 從 **process 建立**起算（GetProcessTimes）：

```text
{"event":"process_start","t_ms":0.000}
{"event":"main","t_ms":14.210}
{"event":"frame_counters","address":"0x7ff6d2c41a08","layout":"fastpdf-frame-counters/1"}
{"event":"window_visible","t_ms":171.902}
{"event":"first_paint","t_ms":189.334}
{"event":"document_opened","t_ms":192.017}
{"event":"first_page_exact","t_ms":236.551}
```

`frame_counters` 沒有 `t_ms`，它告訴 `-AppProbe` 計數器在哪裡（見〈-AppProbe：FastPDF 的 frame 計數〉）。

工具的處理方式：
- 每一行都會記錄收到時的 host 時間（`events[].host_ms`）；
- 有 `event` 和 `t_ms` 的行會整理成 `fastpdf_bench.events_t_ms.<event>`，並進入 summary；
- 不是 JSON 的行保留為 `raw`；
- 換算方式：`host_ms ≈ process_created_offset_ms + t_ms`。

這樣可以拿 app 內部的 `first_page_exact` 對照外部截圖量到的 `launch.t_visual_complete_ms`，作為互相驗證。

### Idle 診斷（1.1.0，`-ThreadDetail`、`-MemoryDetail`）

兩者都只讀取目標 process（query／read 權限），不注入、不寫入；需要 64 位元的 PowerShell。

**`-ThreadDetail` → `idle.threads_detail`、`idle.thread_census`**

- idle 窗口開頭與結尾各取一次 snapshot：
  - CPU cycles：`QueryThreadCycleTime`（精確，不受 15.6 ms 計時粒度影響）；
  - CPU time 與 context switch 次數：`NtQuerySystemInformation(SystemProcessInformation)`。context switch／秒就是這條 thread 每秒被喚醒的次數；
  - 名稱：`GetThreadDescription`（Rust 的具名 thread、GPUI 的 `VSyncProvider`、.NET 等都會設定）；
  - 起點：`NtQueryInformationThread(ThreadQuerySetWin32StartAddress)`，再對照 module 清單寫成 `module.dll+0x偏移`。沒有名稱的 thread 以起點分組，例如 Windows thread pool 的 worker 都從同一個 `ntdll.dll` 位址開始。
- 主執行緒：root process 中建立時間最早的 thread，標成 `main`。
- 輸出：
  - `total_mcycles_per_s`、`total_switches_per_s`：整個 tree 的合計；
  - `main_*`、`vsync_*`：主執行緒與 `VSyncProvider`；
  - `by_source`：依來源（名稱或起點）合計；
  - `top`：cycles 最多的 `-TopThreads` 條 thread；
  - `thread_census`：idle 結束時依來源（名稱，否則起點 module）統計的 thread 數。
- `NtQuerySystemInformation` 的結構是以 64 位元版面讀取的。每次執行都會先用 PowerShell 自己的 thread 清單驗證版面，驗證失敗時只記錄錯誤，不輸出數字。

**`-MemoryDetail` → `idle.memory_detail`**

idle 結束時以 `VirtualQueryEx` 走過整個位址空間，把 committed 的 region 分類，再用 `QueryWorkingSet` 把 working set 的每一頁歸到同一個類別（PSAPI 的 Shared 位元為 0 即為 private）：

| 類別 | 判定方式 | `committed_mb` 的意義 |
|---|---|---|
| `heap` | `MEM_PRIVATE`，allocation base 是 PEB 列出的 process heap，或帶有 NT heap segment 簽章（`0xFFEEFFEE`） | private commit |
| `stack` | `MEM_PRIVATE`，allocation base 是某條 thread 的 stack（TEB 的 `NT_TIB.StackLimit`） | private commit（含 guard page） |
| `teb_peb` | `MEM_PRIVATE`，含 TEB 或 PEB | private commit |
| `other_private` | 其他所有 `MEM_PRIVATE`：GPU driver、D3D／DirectWrite runtime、segment heap 的 segment 與大型區塊等 | private commit |
| `image` | `MEM_IMAGE` | 只計可寫入／copy-on-write 的頁（image 的 commit charge）；整段大小另見 `image_va_mb` |
| `mapped_file` | `MEM_MAPPED` 且 `GetMappedFileName` 成功（檔案映射，例如字型、PDF） | 不佔 commit |
| `mapped_pagefile` | `MEM_MAPPED` 且不是檔案（pagefile-backed section） | 共享 commit，不算在 `private_mb` |

- `private_commit_mb` 是所有 `MEM_PRIVATE` 的合計；加上 `image` 的 commit 後，應接近 `idle.private_mb`。差額是 page table 等 kernel 端的 commit。
- `private_ws_mb` 是 `QueryWorkingSet` 中 private 頁的合計，應接近 `idle.private_ws_mb`。
- **限制：segment heap**。GPUI 的 manifest（gpui 的 `windows-manifest` feature）讓 process 使用 segment heap，FastPDF 也是。segment heap 的 segment 與大型區塊從外部無法和其他 `VirtualAlloc` 記憶體區分，所以會被歸到 `other_private`，`heap` 只剩 heap 本身的管理結構。要拆出 heap，需要在 app 內呼叫 `HeapSummary`；第六輪是用 scratch 的 instrumentation 做的（`docs/benchmarks/b8-app.md`〈Idle RAM 拆解〉），正式的 FastPDF 沒有這個功能。
- `largest_other_private`：`other_private` 中最大的 allocation（位址、大小、保護屬性），用來找來源。

### -AppProbe：FastPDF 的 frame 計數（1.2.0）

用途：自動檢查「idle 時 FastPDF 不畫 frame」（`benchmarks/README.md` 的 Idle CPU 定義中「FastPDF 端」的條件）。

- **FastPDF 端**（`crates/fastpdf-app/src/bench.rs`）：
  - 只在 `FASTPDF_BENCH=1` 時啟用。這時 UI 把每次 render（root view）、prepaint 與 paint（文件 canvas）、wake（背景工作喚醒 UI）交給 bench hook，hook 把它們累加到一塊固定版面的計數器；
  - 啟動時在 stdout 印出計數器的位址：`{"event":"frame_counters","address":"0x...","layout":"fastpdf-frame-counters/1"}`；
  - 計數器版面：8 bytes 的 `FPDFFRC1`，接著 render、prepaint、paint、wake 各一個 little-endian `u64`，共 40 bytes；
  - 沒有 thread、timer、event，也不會為了回報而輸出或醒來。沒有 `FASTPDF_BENCH` 時不建立 hook，什麼都不計、不印。
- **bench-app 端**：
  - 從 stdout 找到 `frame_counters` 那一行，在 idle 窗口、互動後 idle 窗口與每項互動（滾輪、PageDown、縮放）的開始與結束時，各用 `ReadProcessMemory` 讀一次計數器；
  - 讀取完全在 FastPDF 外部進行，不會在 FastPDF 裡執行任何程式，也就不影響量測。
- **輸出**：
  - `app_probe`：`layout` 與說明；找不到 `frame_counters` 時 `layout` 為 null（例如舊版 FastPDF 或其他 app）。`counters_total` 是 run 結束時從啟動起累計的次數；
  - `idle.app_frames`、`post_interaction_idle.app_frames`：窗口內的 render、prepaint、paint、wake 次數；
  - `scroll.app_frames`、`pagedown.app_frames`、`zoom.app_frames`：每項互動期間的次數；
  - 讀不到計數器時改記 `idle.app_frames_error`；
  - summary 另有標記：
    - `idle.app_frames`、`post_interaction_idle.app_frames`：`status` 為 `ok` 或 `frames_while_idle`，加上 run 數、畫了 frame 的 run 數，以及各計數的最大值；
    - `input.app_frames_seen`：互動期間計數器有沒有動。
- **判定與警告**（執行結束時印出，`-Resummarize` 也會）：
  - 任何一次 run 在窗口內的 render 或 paint 不為 0，`status` 就是 `frames_while_idle`，並印出警告；
  - wake 不為 0、但 render 與 paint 為 0 時，表示背景工作在窗口內完成但沒有改變畫面，不算畫 frame；
  - **自我檢查**：有互動的 run 中，互動期間的 render 一次都沒有增加時（`input.app_frames_seen = false`），表示計數器根本沒在計數，idle 的 0 沒有意義，也會印出警告。
- **解讀**：
  - 回歸的典型樣子是每次 run 都有、而且次數很多：持續的動畫每秒數十次（60 Hz 下 10 秒約 600 次；bench-app 不把視窗移到前景，GPUI 對非焦點視窗限制在約 30 fps，所以大約減半）；
  - 偶爾 1 次 render＋paint、wake 為 0，可能來自外部事件：其他視窗搶走焦點（activation 改變）、系統設定變更的廣播、使用者的滑鼠經過視窗。這類情況要看同一次 run 的時間點再判斷；
  - 窗口內不能有輸入。bench-app 自己只在互動階段送輸入，idle 窗口內不送。

---

## Cold 與 warm

- **warm launch**：exe、DLL、字型與 PDF 都已經在 OS 的 file cache（standby list）裡。連續執行時，除了每組的第一次以外都是 warm。**本工具預設量的就是 warm。**
- **cold launch**：要先清掉 file cache。做法有兩種：
  - 用 Sysinternals **RAMMap** 的「Empty → Empty Standby List」（需要系統管理員權限）；
  - 或重新開機，開機後等背景活動平息再量。
- 影響 cold 數字的因素：
  - Windows SysMain/Prefetch 會讓「重開機後第一次」與「清 cache 後」的結果不同；
  - Defender 會在新 exe 第一次執行時掃描（例如 build 完的第一次啟動）。
- 本工具**不會**自動清 cache，也不需要系統管理員權限。自己清過 cache 之後，請在每次執行時用 `-CacheState cold-rammap` 或 `-CacheState cold-reboot` 標記，並用 `-Runs 1` 逐次執行（每次之前都要先清）。

## 瀏覽器的 profile 處理

- 每次執行都在 `-ProfileRoot` 底下建立唯一的暫存 profile，**不會碰使用者既有的瀏覽器 profile、登入狀態或設定**。執行結束後刪除。
- 有 `-WarmupRuns` 時，warm-up run 會初始化一份「範本 profile」，而且不做任何互動。之後每次正式 run 都使用這份範本的**複本**（`profile_state: warm-copy`）。
  - 原因：實測發現 **Edge 的 PDF viewer 會把每份文件的縮放比例與檢視位置記在 profile 裡**，重用同一個 profile 會讓各次 run 的起始畫面不一樣。
  - 沒有 warm-up 時，每次 run 都用全新的空 profile（`fresh`），數字會包含建立 profile 的成本。
- Chromium 旗標：
  - 關閉 first-run 與預設瀏覽器檢查：`--no-first-run`、`--no-default-browser-check`；
  - 關閉 sync、背景網路、元件更新與 metrics 上傳；
  - 關閉被遮蔽或背景視窗的節流：`--disable-features=CalculateNativeWinOcclusion` 等；
  - 固定視窗大小：`--window-size=1536,864`。

## 安全設計（共用機器也能跑）

- **只對自己啟動的 process tree 送輸入**。全部使用 `PostMessage`，不使用 `SendInput`，也不移動真實游標。
- **Chromium 的滾輪保護**：
  - 依 Chromium 原始碼（`ui/base/win/mouse_wheel_util.cc` 的 `RerouteMouseWheel`），Chromium 收到 `WM_MOUSEWHEEL` 時會依螢幕座標把它轉送給「該位置最上層的 Chrome 視窗」，可能捲到**使用者自己的** Chrome。此行為本次沒有另外實測，工具以保守方式處理。
  - 因此每次送出前都先用 `WindowFromPoint` 確認該點最上層的視窗屬於我們的 process tree；不是的話就不送（`inputs_skipped_unsafe`），改用鍵盤 fallback。
- **Ctrl 修飾鍵**：GPUI 與 Chromium 都用 `GetKeyState` 判斷 Ctrl，所以只在 `PostMessage` 裡設定 `MK_CONTROL` 旗標沒有用。工具會用 `AttachThreadInput` + `SetKeyboardState` 讓 Ctrl 只對目標 GUI thread 呈現為按下狀態，結束後立即還原。
- **結束 process 的範圍**：
  - 只結束「我們啟動的 root，以及建立時間晚於啟動時間的後代」；
  - 對使用暫存 profile 的瀏覽器，另外結束 command line 含該唯一 profile 路徑的 process；
  - **絕不依 process 名稱 kill**。
- **遇到額外的 top-level 視窗**（同意條款、更新、登入提示等）時，工具會中止該 run 並結束 process，**不點擊任何東西**，同時把視窗資訊記錄在 `other_windows`。
  - 限制：畫在主視窗內部的提示（例如 bubble）無法自動偵測，請檢查 `app-shots/` 的截圖。
- 單一實例的 app（SumatraPDF、Acrobat）如果已經在執行，工具會拒絕執行，避免檔案被交給使用者自己開著的那個 instance。

## 方法限制（解讀數字前必讀）

1. **外部觀察，不是 frame time**：
   - 截圖頻率約 55–65 Hz，時間解析度約 ±20–35 ms；
   - `changed_frames_per_s` 受截圖頻率限制，只能看出「有沒有持續更新」，不能代表 FPS。
2. **觀察者效應**：截圖期間目標 app 會多用 6–54 ms CPU/s（實測值，見 `capture_overhead`）。idle 取樣期間不截圖，所以 idle 數字不受影響。
3. **多 process 的記憶體**：
   - working set 的總和會重複計算共用頁面，瀏覽器的 WS 總和偏高；
   - Idle RAM 的 KPI 是 private working set 的總和（定義與理由見 `benchmarks/README.md`〈App 層 KPI 定義〉）；private bytes（commit）與 WS 一律一起列出；
   - GPU 記憶體只取自效能計數器（driver 層級）。
4. **鍵盤輸入需要視窗在前景**：
   - Chromium 的視窗不在前景時，實測 PostMessage 的 PageDown 與 Ctrl+= 都**沒有效果**（推測是非前景時沒有 focused view）；
   - 工具不會強制搶前景，所以瀏覽器的捲動與縮放都只用安全滾輪；
   - 捲動點被其他視窗遮住時，該次互動會記為 `effect=false`。
5. **各 app 的開檔路徑不同**：
   - upstream 只能經由 UI 對話框開檔，`open.*` 從按下 Open 起算，不含啟動；
   - 其他 app 從命令列開檔，`launch.t_visual_complete_ms` 包含啟動加開檔；
   - 兩種數字**不能直接相減比較**。
6. **畫面大小不同**：
   - client 區：upstream 1536×864，Chromium 1520×856（含分頁列與工具列）；
   - 預設版面也不同：Edge 開啟大綱、Chrome 開啟縮圖列；
   - 這些都會影響 render 量。
7. **樣本數少**：每個情境 3 次。機器同時有其他負載，每個 run 都記錄了 `cpu_load_pct_before`（本批量測 run 為 16–97%，warm-up 最高 100%）。1.1.0 起另記錄 idle 窗口內的 `idle.system_cpu_busy_pct`。
8. 只有 Windows。只量單一螢幕、100% DPI。
9. **Idle 診斷的限制**（1.1.0）：
   - CPU time 以 15.6 ms 為單位計數，10 秒窗口只能分辨約 0.16%；判斷「接近 0」要看 `threads_detail` 的 cycles 與 context switch；
   - context switch 包含所有喚醒原因：使用者自己的滑鼠移到視窗上也會喚醒主執行緒，這是真實輸入，不是 app 的問題；
   - `threads_detail` 只看 idle 結束時還活著的 thread；中途結束的 thread 只計入 `exited`；
   - GPU driver 的 thread 數與記憶體依廠牌、版本而定（本機 NVIDIA D3D11 driver 自己就有 103 條 thread），不同機器的數字不能直接比較；
   - `-MemoryDetail` 在 segment heap 下拆不出 heap（見〈Idle 診斷〉）。
10. **`-Slowdown` 的限制**（1.3.0）：
    - 它讓 app 的 process 輪流執行與暫停，不是真的降低時脈：快取、記憶體頻寬、I/O 與 GPU 都維持本機的速度，GPU driver 與 DWM 在 app 之外的工作也不受影響；
    - 週期是毫秒級（預設 6 ms），所以小於週期的延遲會被放大或縮小，只有統計上接近「慢 N 倍」；
    - 控制 thread 屬於 bench-app 的 .NET process，GC 暫停會延長某次暫停（記錄在 `max_cycle_overrun_ms`）；
    - 暫停 process 時，kernel 會送 suspend APC 給每條 thread，正在等待的 thread（例如 thread pool worker）會因此醒來：實測每條約 330 次／秒，10 秒 idle 多出約 100 ms CPU。所以 **idle 窗口與互動後的 idle 窗口不降速**（`idle.slowdown_paused`、`slowdown.paused_ms`），idle 的 CPU 時間在慢 N 倍的 CPU 上約為 N 倍，要自行換算；
    - 互動期間仍然降速，上述 APC 會讓互動的 CPU 與 context switch 偏高一些；
    - 控制 thread 會讓一個邏輯 CPU 持續忙碌，所以降速期間的系統忙碌度會多出約 1/邏輯 CPU 數。

---

## Competitor 安裝狀態與待辦（2026-10-04 偵測）

| App | 狀態 | 待辦 |
|---|---|---|
| Microsoft Edge 154.0.4258.53 | 已安裝，已量測（`benchmarks/runs/app-edge.json`） | — |
| Google Chrome 153.0.8010.54 | 已安裝，已量測（`benchmarks/runs/app-chrome.json`） | 下次改用範本 profile 方法重跑（本批用共用 warm profile，見 JSON 的 `note`） |
| **SumatraPDF** | **未安裝**（SPEC §28 必要） | 需要使用者自行安裝（建議官方安裝版或 portable）。裝好後先確認 `-appdata` 參數可用，再執行 `-Preset sumatra` |
| **Adobe Acrobat Reader** | **未安裝**（SPEC §28 必要） | 需要使用者自行安裝，**並自己完成第一次啟動時的條款與登入畫面**（本工具不會點同意）。之後用 `-Preset acrobat -AllowSharedProfile`。注意這會在使用者的 Acrobat 留下最近開啟紀錄 |
| Firefox | 未安裝（選用） | 安裝後執行 `-Preset firefox`。工具會自動建立暫存 profile 與 `user.js` |

## FastPDF 完成後怎麼跑

```powershell
cargo build --release -p fastpdf-app          # 產生 target/release/fastpdf.exe（或用 --profile dist）
# 小 PDF（KPI：首頁 < 200 ms）與 300 頁文件，各 5 次，含捲動、PageDown、縮放
pwsh -File tools/bench-app/bench-app.ps1 -Preset fastpdf -Pdf fixtures/generated/small-text/three-pages-platypus-times.pdf -Runs 5 -ScrollNotches 20 -PageDowns 5 -ZoomSteps 3
pwsh -File tools/bench-app/bench-app.ps1 -Preset fastpdf -Pdf fixtures/generated/large-text/dense-300p-times.pdf -Runs 5 -ScrollNotches 20 -PageDowns 5 -ZoomSteps 3
pwsh -File tools/bench-app/bench-app.ps1 -Preset fastpdf -Scenario launch-empty -Runs 5   # 不帶檔案的空視窗 idle
```

- 結果寫入 `benchmarks/runs/app-fastpdf.json`。要看的欄位：
  - `fastpdf_bench.events_t_ms.first_page_exact`（app 自報）；
  - `launch.t_visual_complete_ms`（外部截圖）；
  - `idle.private_ws_mb`（Idle RAM KPI，目標 < 50 MB），以及 `idle.private_mb`（commit）、`idle.ws_mb`；
  - `idle.cpu_pct_of_one_core`（Idle CPU KPI，目標約 0），加上 `-ThreadDetail` 時的 `idle.threads_detail.*_switches_per_s`；
  - `idle.app_frames`、`post_interaction_idle.app_frames`（1.2.0，preset `fastpdf` 預設讀取）：idle 時 FastPDF 不能畫 frame，summary 的同名標記必須是 `ok`。
  - KPI 的定義、量測時點與報告方式見 `benchmarks/README.md`〈App 層 KPI 定義〉。
- 視窗不在前景時，posted 的 `WM_KEYDOWN`（PageDown）與 Ctrl+滾輪都有效：1.2.0 實測 `pagedown.app_frames`、`zoom.app_frames` 的 render 不為 0（`docs/benchmarks/b8-app.md`〈後續〉第 5 點）。GPUI 會處理 posted 的滑鼠與鍵盤訊息，Ctrl 狀態由工具注入。如果 `effect=false`，請先確認 FastPDF 的 keybinding 是否已綁定。

---

## 首批數據（2026-10-04，warm，每個情境 3 次取中位數）

機器：Ryzen 9 9950X、128 GB、RTX 5090、Windows 11 10.0.26200、1920×1080@60 Hz、100% DPI。測試檔：`fixtures/generated/large-text/dense-300p-times.pdf`（300 頁、1.2 MB）。

| App／情境 | 視窗出現 | 首個非空白 | 視覺完成 | idle private（WS） | idle CPU／10 s | peak private | 捲動：延遲／收斂 | 縮放延遲 |
|---|---:|---:|---:|---|---:|---:|---|---:|
| upstream 空視窗 | 300 ms | 331 ms | 331 ms | 96.5 MB（57 MB） | 63 ms | 101 MB | — | — |
| upstream 開 300 頁（對話框，從 Open 起算） | 299 ms | 315 ms | **226 ms**（208–241） | 163.5 MB（129 MB） | 109 ms | 202 MB | 27 ms／25 ms | 無此功能 |
| upstream 開 3 頁小檔（從 Open 起算） | 310 ms | 337 ms | **180 ms**（177–242） | 156.9 MB（125 MB） | 141 ms | 173 MB | — | — |
| Edge 154（CLI 開 300 頁，從啟動起算） | 229 ms | 270 ms | **1,233 ms** | 351.7 MB（596 MB，10 個 process） | 63 ms | 474 MB | 75 ms／223 ms | 59 ms |
| Chrome 153（CLI 開 300 頁，從啟動起算） | 215 ms | 244 ms | **944 ms** | 373.7 MB（647 MB，10 個 process） | 125 ms | 391 MB | 77 ms／229 ms（n=2） | 37 ms（n=2） |

- upstream 的 PageDown 在 3 次 run 中都沒有任何效果（沒有綁定）。upstream 沒有縮放功能。
- 瀏覽器的捲動帶有 smooth-scroll 動畫，「收斂」時間包含這段動畫。
- 原始資料（含每個 run 的變化時間軸與截圖路徑）在 `benchmarks/runs/app-*.json`。

## 實作備註

- **pwsh 7.6.6 的 bug**：對內含 `OrderedDictionary` 的 `List[object]` 使用 `@(...)` 會丟出 `Argument types do not match`。腳本因此改用 `ArrayList` 加上 `.ToArray()`，修改時請保留這個寫法。
- `BenchApp.cs` 刻意寫成相容 C# 5（不使用字串插值、`out var` 等語法），讓 Windows PowerShell 5.1 也能編譯。
