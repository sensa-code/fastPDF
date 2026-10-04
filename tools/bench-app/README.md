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
```

同一個 `-OutFile` 可以放多個 scenario。scenario 名稱相同的資料會被取代，名稱不同就附加。

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
{"event":"window_visible","t_ms":171.902}
{"event":"first_paint","t_ms":189.334}
{"event":"document_opened","t_ms":192.017}
{"event":"first_page_exact","t_ms":236.551}
```

工具的處理方式：
- 每一行都會記錄收到時的 host 時間（`events[].host_ms`）；
- 有 `event` 和 `t_ms` 的行會整理成 `fastpdf_bench.events_t_ms.<event>`，並進入 summary；
- 不是 JSON 的行保留為 `raw`；
- 換算方式：`host_ms ≈ process_created_offset_ms + t_ms`。

這樣可以拿 app 內部的 `first_page_exact` 對照外部截圖量到的 `launch.t_visual_complete_ms`，作為互相驗證。

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
   - 比較時以 private bytes 總和為主；
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
7. **樣本數少**：每個情境 3 次。機器同時有其他負載，每個 run 都記錄了 `cpu_load_pct_before`（本批量測 run 為 16–97%，warm-up 最高 100%）。
8. 只有 Windows。只量單一螢幕、100% DPI。

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
  - `idle.private_mb`（KPI 目標 < 50 MB）；
  - `idle.cpu_ms`（目標約 0）。
- 尚未驗證的部分：posted 的 `WM_KEYDOWN`（PageDown）與 Ctrl+滾輪在 FastPDF 視窗不在前景時是否有效。GPUI 會處理 posted 的滑鼠與鍵盤訊息，Ctrl 狀態由工具注入。如果 `effect=false`，請先確認 FastPDF 的 keybinding 是否已綁定。

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
