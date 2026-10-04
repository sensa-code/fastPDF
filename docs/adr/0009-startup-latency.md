# ADR 0009 — 啟動延遲：GPUI 啟動時間線與縮短方式

- 狀態：Accepted（FastPDF 端：不改程式碼）。GPUI 端的 G1、G2 是 upstream 提案草稿（`docs/upstream-issues/gpui-startup.md`），**沒有送出**；FastPDF 仍使用 pin 的 GPUI。
- 日期：2026-10-05（量測在 2026-10-04 晚間）
- 相關：spec §28–§29（小檔首頁 < 200 ms）；B-8（`docs/benchmarks/b8-app.md`）；`docs/PROJECT_AUDIT.md` R12（低階機器）；ADR 0001（使用 GPUI，不 fork）
- 量測對象：
  - HEAD `8270b3e` 的 `git archive` 匯出；
  - pin 的 zed rev `a84689073`（`a84689073d296dfd39987bc7dd478e43ef76d83a`）的複本，用 `[patch."https://github.com/zed-industries/zed"]` 指過去；
  - 兩者都在 scratchpad。repo 的 `Cargo.toml` 沒有改，GPUI 沒有加進 workspace。

## 結論

- **小檔首頁的時間幾乎全花在 GPUI 啟動。** 安靜的機器上（背景負載 16–20%），從 process 啟動到第一頁完全清晰是 211–230 ms：

  | 階段 | 時間 |
  |---|---|
  | process 建立到 `main`（loader 載入 27 個 DLL） | 10–16 ms |
  | GPUI platform 初始化（主執行緒），其中系統字型集合占 27 ms、DragDropHelper 占 7 ms | 37 ms |
  | 建立 DirectX device，其中 `D3D11CreateDevice` 占 104–118 ms | 113–128 ms |
  | 建立並顯示視窗 | 26–32 ms |
  | FastPDF 自己的 view 與第一次 draw | 7–9 ms |
  | 等下一個 frame 再 present | 2–15 ms |

- **FastPDF 端沒有可改的地方。**
  - FastPDF 在 GPUI 啟動前後的同步工作合計不到 0.5 ms。
  - 唯一試過的 FastPDF 端方案 F1（自己先建一個 D3D11 device 預熱 driver）沒有效果，peak private bytes 還多了 18.7 MB，不採用。
- **兩個 GPUI 修正有明確效果**，都只改 `crates/gpui_windows`：
  - **G1**：在 `WindowsPlatform::new` 一開始，就在另一條執行緒上建立 DirectX devices。與同回合 baseline 相比，`first_page_exact` 在乾淨回合差 −24.1 ms 與 −38.9 ms；限制成 2 核時差 −21.7 ms。
  - **G2**：建立 DirectWrite 系統字型集合時不做 update check。platform 初始化本身從 37 ms 降到 10–12 ms，platform 就緒時間從 48–54 ms 提早到 26–28 ms；`first_page_exact` 在 3 個乾淨回合差 −14.8、−27.8、−22.0 ms。
  - 在這台機器上，兩者的效果不會相加：有了 G1，platform 初始化已經和 device 建立同時進行，不在關鍵路徑上。
- **G1 與 G2 都要改 GPUI。** 依 ADR 0001 不 fork，所以只整理成 upstream 草稿與 patch。
  - 合併進 upstream、FastPDF 更新 pin 之後，依這次量到的範圍，安靜機器上的小檔首頁預期為：有 G1 時約 170–190 ms，只有 G2 時約 185–210 ms。
  - 在那之前，B-8 的 < 200 ms 目標無法穩定達成。
- **R12：核心數不是瓶頸。**
  - 用 process affinity 限制成 4 核或 2 核，GPUI 啟動時間不變（`first_page_exact` 200.3 ms、212.2 ms）。
  - 啟動的關鍵路徑是單執行緒的：全部在主執行緒上，`D3D11CreateDevice` 也是在主執行緒上呼叫。
  - 背景負載的影響大得多：負載 83–97% 時，和同一個 build 在安靜時相比慢了 115–195 ms。
- **Shader 不是問題，adapter 也沒有重複列舉。**
  - release／dist build 在編譯時就用 `fxc.exe` 把 shader 編成 bytecode，執行時只建立 shader 物件（1.3–1.6 ms）。
  - Adapter 只列舉一次，device 也只建立一次。

## Context

B-8 最終版的 `first_page_exact` 中位數：3 頁 219 ms，300 頁 213 ms（`docs/benchmarks/b8-app.md`）。UI 第五輪已經讓第一個 frame 就是完全清晰的第一頁，所以剩下的時間是「視窗出現之前」的部分。先前只知道 D3D11 device 大約 115 ms，不知道其餘 100 ms 花在哪裡，也不知道哪些可以由 FastPDF 自己縮短。

本 ADR 回答三個問題：

1. process 啟動到第一頁的每一段時間花在哪裡；
2. 不改 GPUI 時，FastPDF 能做什麼；
3. 改 GPUI 的話，哪些修正值得提給 upstream。

## 方法

### 量測點

- **GPUI 端（只在 scratch 複本中）**
  - 新增 `gpui_windows/src/startup_trace.rs`：設定 `FASTPDF_BENCH` 時，每個量測點在 stdout 印一行 `{"event":"gpui_<name>","t_ms":…}`，每個名稱只印第一次。
  - 時間基準和 FastPDF 的 bench 事件相同：起點是 `GetProcessTimes` 回報的 process 建立時間，現在時間用 `GetSystemTimePreciseAsFileTime`。因此 GPUI 與 FastPDF 的事件在同一條時間軸上。
  - 共 34 個點，分布在 platform、DirectWrite、DirectX、視窗建立、第一次 present 的程式碼中。
  - 這些量測點不在 upstream patch 裡。
- **FastPDF 端（只在匯出副本中）**
  - `main.rs` 加了 `app_before_gpui`、`app_run_closure`、`app_keys_bound` 三個事件。

### Build 與執行

- **Build**
  - `--profile dist`（fat LTO），和 B-8 相同。
  - target dir 是 `target/agent-gpui-startup`。
  - 每個 exe 量測前先以 `--version` 執行一次，避開新檔案第一次執行時的 Defender 掃描（見 B-8 的 F 回合說明）。
- **啟動方式**
  - 用 B-8 的 `tools/bench-app/bench-app.ps1 -Preset fastpdf -Runs 1`，一次一個 launch，開 3 頁的 `small-text/three-pages-platypus-times.pdf`。
  - 每次都設定 `FASTPDF_SETTINGS_FILE=`、`FASTPDF_RECENT_FILE=`（空字串），不讀寫使用者設定。
- **交替配對**：每回合 base、G1、G2、F1 各跑一次，順序每回合反轉：

  | 回合 | 順序 |
  |---|---|
  | R1 | base → G1 → G2 → F1 |
  | R2 | F1 → G2 → G1 → base |
  | R3 | base → G1 → G2 → F1 |
  | R4（補 G2 的第 3 個乾淨配對） | G2 → base |

  每個變體都和**同一回合**的 base 相減。

### 安靜條件與背景負載

- 每次啟動前檢查：
  - 必須沒有 `cargo`、`rustc`、`link`：每 30 秒檢查一次，最多等 10 分鐘。
  - 盡量等到 CPU 負載 ≤ 50%（R4 是 ≤ 35%），而且沒有其他 agent 的 FastPDF 在執行。等約 7 分鐘（R4 約 12 分鐘）仍未達成就照量，並記錄當時狀態。
- 背景負載以 bench-app 的 `cpu_load_pct_before` 記錄在每一筆資料旁。
- 量測期間其他 agent 時常在編譯。R1 的 base（83%）與 G1（97%）受嚴重干擾，下文稱為「受干擾」，其餘稱為「乾淨回合」。
- `vmmemwsl`（使用者的 WSL）沒有動。

### R12、機器與啟動次數

- **R12（低階機器近似）**
  - 用自己的 script 啟動 exe，在 `Process.Start` 回來後立刻設定 process affinity：
    - `0x55`：4 個實體核心，每核 1 個邏輯處理器；
    - `0x5`：2 個實體核心。
  - `CreateProcess` 沒有用 suspended 啟動，所以 loader 最開頭的幾 ms 可能不受限制。
  - 沒有改 `tools/bench-app/`。
- **機器**：和 B-8 相同。
  - CPU：Ryzen 9 9950X（16C／32T）。
  - GPU：RTX 5090（driver 32.0.16.1088，接螢幕，是 adapter 0），另有 AMD 內顯（沒有接螢幕）。
  - 螢幕：1920×1080 @ 60 Hz。
  - OS：Windows 11 Pro 10.0.26200。
- **GUI 啟動次數：24 次（預算 24 次）**

  | 用途 | 次數 |
  |---|---|
  | 時間線 | 2 |
  | 第一次配對嘗試（作廢） | 5 |
  | 交替配對 R1–R3 | 12 |
  | affinity | 3 |
  | R4 | 2 |

  - 第一次配對嘗試作廢的原因：
    - 同一個 label 的輸出檔互相覆蓋，資料遺失；
    - 當時背景負載 100%。
  - 另有 5 次非 GUI 的 `--version` 執行（不開視窗，用來預熱新 build 的 exe；base 重 build 過一次，所以是 5 次）。

## 時間線（baseline，pin 的 GPUI）

數字是 ms。前三欄是安靜回合的 base（背景負載 18%、16%、20%），最後一欄是時間線量測（負載 68%）。

| # | 階段 | 執行緒 | n15 | n16 | n24 | 時間線 2 | 說明 |
|---|---|---|---|---|---|---|---|
| 1 | process 建立 → `main` | 主 | 15.4 | 10.2 | 16.2 | 13.7 | loader：27 個 load-time import，沒有 delay-load |
| 2 | `main` → GPUI `WindowsPlatform::new` | 主 | 0.1 | 0.1 | 0.2 | 0.2 | FastPDF 的參數、設定、bench clock |
| 3 | `OleInitialize` | 主 | 1.5 | 1.4 | 1.4 | 1.4 | |
| 4 | DirectWrite factory、UI 字型名稱 | 主 | 0.2 | 0.2 | 0.2 | 0.3 | |
| 5 | **系統字型集合** | 主 | **26.9** | **27.6** | **27.6** | **31.1** | `GetSystemFontCollection(…, checkForUpdates = true)` |
| 6 | message window | 主 | 0.4 | 0.4 | 0.4 | 0.6 | |
| 7 | **DragDropHelper** | 主 | **7.3** | **7.6** | **7.4** | **9.1** | `CoCreateInstance(CLSID_DragDropHelper)` |
| | *platform 就緒（絕對時間）* | | *52.1* | *47.7* | *53.6* | *56.8* | |
| 8 | `run` → 開始建立 device | 主 | 1.0 | 0.3 | 0.3 | 0.4 | 環境檢查 |
| 9 | `CreateDXGIFactory2` | 主 | 8.4 | 9.1 | 9.2 | 14.8 | |
| 10 | `EnumAdapters(0)` + `GetDesc1` | 主 | 0.1 | 0.1 | 0.1 | 0.1 | 只列舉一次 |
| 11 | **`D3D11CreateDevice`** | 主 | **103.5** | **114.9** | **117.9** | **121.0** | 最大的一段 |
| | *device 就緒（絕對時間）* | | *165.1* | *172.1* | *181.1* | *193.1* | |
| 12 | text system 的 GPU 狀態、vsync 執行緒 | 主 | 0.9 | 0.7 | 0.5 | 0.8 | |
| 13 | FastPDF 的 run closure（key binding 等） | 主 | 0.2 | 0.1 | 0.1 | 0.2 | |
| 14 | `open_window` → `CreateWindowExW` | 主 | 1.8 | 1.8 | 2.1 | 3.0 | |
| 15 | `WM_CREATE`：renderer | 主 | 4.8 | 5.5 | 6.4 | 5.5 | swap chain 2.0–3.3、shader pipeline 1.3–1.6、DirectComposition 0.8–0.9 |
| 16 | `CreateWindowExW` 其餘部分 | 主 | 2.5 | 2.8 | 3.9 | 3.1 | |
| 17 | `RegisterDragDrop` | 主 | 3.5 | 4.2 | 4.3 | 4.6 | |
| 18 | **`SetWindowPlacement`（顯示視窗）** | 主 | **12.6** | **14.5** | **14.9** | **18.1** | |
| | *視窗顯示（絕對時間）* | | *191.4* | *201.7* | *213.4* | *228.4* | |
| 19 | `ReaderView::new`（接手開檔 session） | 主 | 2.2 | 2.5 | 2.7 | 6.4 | → `document_opened` |
| 20 | 第一次 draw（layout、paint，尚未 present） | 主 | 4.8 | 5.9 | 5.6 | 6.5 | → `window_visible` |
| 21 | 等 vsync 執行緒要求下一個 frame | 主 | 13.2 | 0.7 | 8.1 | 11.8 | → `first_paint`＝`first_page_exact` |
| 22 | present | 主 | 1.6 | 1.7 | 1.9 | 2.1 | |
| | ***`first_page_exact`（絕對時間）*** | | ***211.6*** | ***210.9*** | ***229.8*** | ***253.1*** | |

補充：

- **全部 19 次有完整量測點的啟動**
  - 第 21 段（等下一個 frame）：0–13.2 ms，中位數 2.6 ms，平均 5.1 ms；9 次超過 5 ms。
  - 第 18 段（顯示視窗）：11.5–30.7 ms，中位數 15.4 ms。
  - 第 17 段（`RegisterDragDrop`）：3.2–10.5 ms，中位數 4.1 ms。
  - 第 1 段（`main` 之前）：9.7–29.1 ms，中位數 15.8 ms。
- **Shader**
  - `gpui_windows` 的 `build.rs` 在非 debug build 時用 Windows SDK 的 `fxc.exe` 編譯 HLSL，輸出 `shaders_bytes.rs`。
  - 執行時第 15 段只呼叫 `Create*Shader`。
  - debug build 才會在執行時用 `D3DCompileFromFile` 編譯（`directx_renderer.rs`），不影響發佈版。
- **Adapter**
  - `get_adapter` 從 adapter 0 開始，找到第一個能建立 D3D11 device 的 adapter 就停。
  - 這台機器只列舉一次、建立一次。crates.io 上 gpui 0.2.2 重複建立 device 的問題（GPUI audit）在 pin 的 rev 已經不存在。
- **`first_page_exact` 的意義**
  - 它在第一次 draw 之後的「下一個 frame」callback 中發出（`window.on_next_frame`），緊接著就是第一次 present。
  - 因此它約等於第一頁送到 DWM 的時間。
  - B-8 的截圖時間（`PrintWindow`）還要再加上 DWM 合成與 15 ms 取樣間隔。

## 選項與量測

### 總表

| 代號 | 內容 | 修改位置 | 量測 | 效果 | 決定 |
|---|---|---|---|---|---|
| G1 | 在 `WindowsPlatform::new` 開頭用執行緒建立 DirectX devices，第一次 attach 時 join | GPUI `platform.rs` | 交替配對 3 回合 + 2 核配對 | 明確（−22～−39 ms） | 提給 upstream |
| G2 | `GetSystemFontCollection` 的 `checkForUpdates` 改成 `false` | GPUI `direct_write.rs` | 交替配對 4 回合 | 明確（−15～−28 ms） | 提給 upstream |
| F1 | FastPDF 在 `main` 開頭用執行緒建立一個 D3D11 device，預熱 driver | FastPDF `main.rs` | 交替配對 3 回合 | 沒有 | 不採用 |
| — | GPUI 啟動前後的 FastPDF 同步工作 | FastPDF | 量測點 | 0.1–0.2 ms，沒有可省的 | 不改 |
| — | 提早開視窗、改視窗選項 | FastPDF | 分析 | 不可行，或不影響關鍵路徑 | 不改 |
| F2 | delay-load 第一個 frame 用不到的 DLL | FastPDF link 設定 | `LoadLibrary` 估計 | ≤ 2.3 ms | 不改 |
| G3–G5 | 延後 DragDropHelper、第一次 present 不等 vsync、縮短視窗顯示 | GPUI | 由時間線估計 | 未量測 | 後續 |

### 交替配對結果

數字是變體減去同回合 base 的差（ms），負值代表變快；括號內是兩者的背景負載。

| 指標 | 回合 | G1 | G2 | F1 |
|---|---|---|---|---|
| `first_page_exact` | R1 | +22.7（97%／83%，受干擾） | −140.5（45%／83%，base 受干擾） | −154.4（58%／83%，base 受干擾） |
| | R2 | **−24.1**（28%／18%） | **−14.8**（51%／18%） | +23.7（91%／18%） |
| | R3 | **−38.9**（25%／16%） | **−27.8**（48%／16%） | +3.3（62%／16%） |
| | R4 | — | **−22.0**（45%／20%） | — |
| | 2 核 | **−21.7**（40%／7%） | — | — |
| device 就緒 | R1／R2／R3／R4 | −1.3／−17.9／−34.1／— | −122.2／−11.2／−27.1／−21.4 | −129.1／+20.7／−7.6／— |
| `window_visible` | R1／R2／R3／R4 | +29.1／−11.0／−38.2／— | −138.7／−1.8／−29.6／−19.6 | −146.3／+36.8／−8.0／— |
| 截圖首個非空白 | R1／R2／R3／R4 | +77.3／−47.5／−41.1／— | −108.1／−34.9／−1.6／+6.6 | −163.9／−3.1／+31.5／— |

| 變體 | 次數 | `first_page_exact` 中位數（範圍） | device 就緒 | idle CPU（ms／10 s） | idle private（MB） | peak private（MB） |
|---|---|---|---|---|---|---|
| base | 4 | 220.7（210.9–344.2） | 176.6（165.1–272.5） | 101.6（0–109.4） | 107.5（107.4–107.6） | 122.4（122.3–122.6） |
| G1 | 3 | 187.5（171.9–366.9） | 147.2（137.9–271.1） | 62.5（31.2–156.2） | 107.4（107.3–107.5） | 121.7（121.6–121.8） |
| G2 | 4 | 200.2（183.1–207.8） | 152.1（145.0–159.7） | 62.5（15.6–78.1） | 107.3（107.2–107.4） | 121.2（120.9–121.3） |
| F1 | 3 | 214.1（189.8–235.3） | 164.4（143.4–185.8） | 78.1（46.9–171.9） | 108.2（108.0–108.7） | 141.1（140.9–141.1） |

- **畫面**：16 張啟動後的最終截圖（含作廢那次的 2 張）完全相同，SHA-256 前綴都是 `15f9c7925ea1`。
- **Idle CPU**：沒有任何變體增加。各次的數字隨背景負載在 0–172 ms／10 s 之間變動，G1、G2 的中位數比 base 低也只是負載的差異。
- **記憶體**：G1、G2 的 idle 與 peak private bytes 和 base 相同（±1 MB），沒有洩漏。F1 的 peak 多 18.7 MB，那是第二個 D3D11 device 存活的 5 秒。
- **截圖指標**：截圖首個非空白受 15 ms 取樣與找視窗的時間影響，差值雜訊太大（G2 在 R3、R4 是 −1.6／+6.6 ms），只當參考。判斷以 process 內事件為準。

### G1：DirectX devices 與 platform 初始化同時進行（GPUI）

- **Before**：`attach_gpu` 在 platform 初始化之後才建立 device。乾淨回合的 base 在 48–54 ms 開始建立，165–181 ms 就緒。
- **After**：device 執行緒在 14–17 ms 開始（`WindowsPlatform::new` 的第一行），138–147 ms 就緒。
  - `D3D11CreateDevice` 本身的時間和 base 相近（114.0–120.3 ms；base 103.5–117.9 ms）。
  - 同時在主執行緒上進行的字型集合也只慢了 1–2 ms（28.1–29.4 ms；base 26.9–27.6 ms）。
- **Why**：建立 device 不需要 OLE、DirectWrite、message window 或 DragDropHelper 的任何結果。主執行緒那 37 ms 原本排在 device 前面，現在被 device 建立的時間蓋過。
  - D3D11 device 與 DXGI factory 是 free-threaded：device 建立時沒有 `D3D11_CREATE_DEVICE_SINGLETHREADED`。
  - immediate context 在 join 之前不會被使用，所以跨執行緒建立是安全的。
- **Tradeoff**：
  - 多一條短命的執行緒，存活時間約等於 device 建立的時間。
  - 2 核限制下仍有效（−21.7 ms）。兩條執行緒同時執行時，字型集合只慢 1.4 ms、`D3D11CreateDevice` 只慢 6.2 ms（和 2 核的 base 相比）。
  - 執行緒失敗或 panic 時，`attach_gpu` 回到原本在主執行緒建立的路徑。
  - app 以 headless 啟動時（`set_initial_windowing(Headless)`，或無法顯示視窗），handle 會被丟掉，device 在執行緒結束時釋放。這種情況下白做了一次 device 建立。
  - 之後切換到 windowed 的行為不變。
- **Patch**：`docs/upstream-issues/patches/gpui-startup-0001-create-directx-devices-while-the-platform-starts.patch`（`platform.rs`，+38／−4）。
  - 量測用的 build 是較早的版本：第一次 `attach_gpu` 時才從欄位取出 handle，panic 直接忽略。
  - 量測後整理成現在的 patch：handle 只交給 `run` 的第一次 attach，headless 啟動會丟掉，panic 會記錄 log。
  - windowed 啟動的路徑完全相同，所以量測結果適用。
  - 整理後的版本做過 `cargo check`（dev）與 rustfmt（zed 的設定），沒有重新量測。

### G2：建立系統字型集合時不做 update check（GPUI）

- **Before**：`DirectWriteTextSystem::new` 呼叫 `GetSystemFontCollection(false, &mut result, true)`。
  - 安靜時 26.9–27.6 ms，負載 50–68% 時 31–38 ms，重負載時 52–72 ms。
  - platform 在 47.7–53.6 ms 就緒。
- **After**：`checkForUpdates = false`。這次呼叫只要 0.7–0.8 ms，platform 初始化本身是 9.7–11.9 ms，platform 在 25.5–28.4 ms 就緒（4 次都是）。
  - 之後各段：device 就緒到 `window_visible`，G2 是 35.5–47.0 ms（負載 45–51%），base 是 33.3–40.6 ms（負載 16–20%）。
  - 差異可能來自負載；也不能排除有幾 ms 的成本移到了第一次用字型的時候。即使如此，淨效果仍是 −15～−28 ms。
- **Why**：
  - factory 才剛建立，它的系統字型集合就是最新的。
  - `checkForUpdates = true` 會讓 DirectWrite 在回答前先檢查系統字型有沒有變動，這個檢查就是那 27 ms。
  - 依 DirectWrite 文件，`false` 時只要 font cache service 在執行，仍會偵測到變動，只是可能有延遲。
- **Tradeoff**：
  - 在 app 啟動前一刻才安裝的字型，可能不會出現在 `all_font_names()` 的清單裡。這份清單是 Zed 的字型選單用的，FastPDF 沒有用到。
  - 用到這種字型時，`select_and_cache_font` 找不到字型會以 `true` 重新取得集合，所以仍然找得到。
  - FastPDF 的 UI 只用系統 UI 字型，PDF 內的字型由 engine 處理，不受影響。
- **Patch**：`docs/upstream-issues/patches/gpui-startup-0002-skip-the-font-update-check-when-creating-the-text-system.patch`（`direct_write.rs`，+5／−1）。和量測的版本相同，只是沒有量測點。

**G1 + G2**：沒有量測。有 G1 時，關鍵路徑是 device 執行緒（約 125–130 ms，含 DXGI factory），主執行緒的 platform 初始化（37 ms）已經在它的陰影裡。G2 再省 27 ms 也不會提早任何事件，所以這台機器上的效果約等於只有 G1。device 建立快的機器上（例如只有內顯，或 driver 已經在記憶體中），platform 初始化可能變成關鍵路徑，這時 G2 才會疊加出效果。兩個 patch 分開提，任一個被接受都有幫助。

### F1：FastPDF 自己預熱 GPU driver（不採用）

- **做法**：
  - 在 `reader_main` 開頭開一條 `gpu-prewarm` 執行緒。
  - 用 `raw-dylib` 直接呼叫 `D3D11CreateDevice`：預設 adapter，`BGRA_SUPPORT`，feature level 11.1／11.0／10.1，和 GPUI 相同；沒有新增 crate。
  - device 保留 5 秒後再 `Release`，確保 GPUI 建立自己的 device 時 driver 還在。
- **結果**：
  - 預熱從 12–19 ms 開始，到 120.6–153.9 ms 才建立好，耗時 108–138 ms，和 GPUI 自己建立一次差不多。
  - GPUI 的 `CreateDXGIFactory2` 從 8–9 ms 降到 0.2 ms，但 GPUI 的 `D3D11CreateDevice` 仍要 88.3–126.9 ms，而且都在預熱的 device 完成後 22–32 ms 才結束。
  - 看起來同一個 process 內的 driver 初始化是序列化的：第二個 device 要等第一個建立完，之後還要再花 20–30 ms。
  - 乾淨回合的 `first_page_exact`：+23.7 ms（F1 當時負載 91%）、+3.3 ms，沒有改善。
- **代價**：peak private bytes +18.7 MB（141.1 vs 122.4 MB）；idle 時多 0.7 MB。
- **結論**：在 FastPDF 端搶先建立 device，比不上 G1 直接把 GPUI 自己的 device 提早建立。不採用，也沒有建議的 FastPDF diff。

### FastPDF 端的其他項目

- **GPUI 啟動前的同步工作**：`main` → `WindowsPlatform::new` 只有 0.1–0.2 ms，包括參數解析、`Env::read`、設定（量測時為空）、bench clock。沒有東西可以移走。
- **run closure**：key binding 與 `open_window` 之前的工作 0.1–0.2 ms。
- **提早開視窗**：
  - GPUI 的 `open_window` 在 `WM_CREATE` 中建立 renderer，需要已經 attach 的 DirectX devices。
  - `on_finish_launching`（FastPDF 的 run closure）本來就是在 device 就緒後的第一個時間點，FastPDF 無法更早開視窗。
  - 要更早只能改 GPUI：先顯示空白的 HWND，device 就緒後再接上 renderer。但這只會讓空白視窗提早出現，不會讓第一頁提早，所以沒有做。
- **視窗選項**：
  - FastPDF 只設定 `window_bounds`、titlebar 標題、`window_min_size`、`app_id`，其餘是預設值（`show`、`focus` 為 true）。
  - 視窗顯示的成本（第 17–18 段，約 16–19 ms）來自 GPUI 的 `RegisterDragDrop` 與 `SetWindowPlacement`，沒有 FastPDF 的選項能避開。
- **FastPDF 自己的 view 與第一次 draw**（第 19–20 段）：7–9 ms，是 FastPDF 唯一在關鍵路徑上的程式碼。
  - 第一次 draw 包含第一頁 bitmap 上傳到 GPUI 的 atlas。
  - 這部分屬於 UI／render 的檔案，這一輪沒有動，列為後續 F3。
- **F2：delay-load DLL**
  - exe 有 27 個 load-time import，沒有 delay-load。第一個 frame 用不到的有：
    - `uiautomationcore`（GPUI 的 UI Automation：`events.rs` 與 accesskit）；
    - `winspool.drv`（FastPDF 的 `fastpdf-print`：`EnumPrintersW`、`GetDefaultPrinterW`）；
    - `icuuc`（GPUI 的 `destination_list.rs`，只用 `u_strlen`）；
    - `comctl32`、`psapi`、`userenv`、`winmm`。
  - 估計方式：用新的 process 先載入第一個 frame 本來就需要的 DLL，再量 `LoadLibraryW` 的時間，每組 7 次。
  - 結果：前三個合計 1.4 ms，七個合計 2.3 ms。
  - 只有第 1 段（10–16 ms）的一小部分，不值得增加 link 設定的複雜度。

### R12：限制核心數

用 process affinity 限制核心數，每個條件量 1 次。背景負載取自啟動前的量測。

| 條件 | 背景負載 | `main` | platform 就緒 | `D3D11CreateDevice` | device 就緒 | `window_visible` | `first_page_exact` |
|---|---|---|---|---|---|---|---|
| 32 執行緒（base，n15／n16／n24） | 16–20% | 10.2–16.2 | 47.7–53.6 | 103.5–117.9 | 165.1–181.1 | 198.4–221.7 | 210.9–229.8 |
| 4 核（`0x55`） | 22% | 15.6 | 53.8 | 104.3 | 168.0 | 199.1 | 200.3 |
| 2 核（`0x5`） | 7% | 9.7 | 45.5 | 115.0 | 169.1 | 200.6 | 212.2 |
| 2 核 + G1 | 40% | 16.9 | 58.5 | 121.2 | 153.5 | 189.9 | 190.5 |

- **核心數**：
  - 限制成 4 核、2 核，各段時間都在 32 執行緒的範圍內。
  - 4 核與 2 核的 `first_page_exact` 差 12 ms，來自第 21 段的 vsync 等待（1.3 vs 11.6 ms）。
  - `D3D11CreateDevice` 在 2 核時也沒有變長（115.0 ms；32 執行緒時 103.5–117.9 ms）。
  - 結論是 GPUI 啟動不依賴核心數：關鍵路徑是單執行緒的。
- **2 核 + G1**：仍然有效（device 就緒 −15.6 ms，`first_page_exact` −21.7 ms）。和 2 核的 base 相比，主執行緒的字型集合只慢 1.4 ms（28.2 vs 26.8 ms），DragDropHelper 不變（7.0 ms）。
- **真正的低階機器還差在哪裡**（這次沒有模擬）：
  - 單核速度：9950X 是 5 GHz 以上的 Zen 5；
  - GPU 與 driver：內顯的 `D3D11CreateDevice` 時間未知；
  - 儲存裝置：exe 與 DLL 的 cold load。
  - 換 adapter 或 GPU 偏好設定在這次的規則下不允許，所以內顯的情況沒有量。
- **背景負載的影響**：比核心數大得多。R1 的 base 與 G1 在 83–97% 負載下，`first_page_exact` 是 344–367 ms，比同一個 build 在安靜回合慢 115–195 ms。變慢最多的是 `D3D11CreateDevice`（166–226 ms）與字型集合（52–72 ms）。

## Decision

1. **FastPDF 端不改程式碼。**
   - GPUI 啟動前後的 FastPDF 工作不到 0.5 ms。
   - F1 沒有效果，還增加記憶體。
   - F2 最多省 2.3 ms。
   - `crates/fastpdf-app/src/main.rs` 與 `crates/fastpdf-ui/src/startup.rs` 都不需要修改。
2. **把 G1、G2 提給 upstream（zed）**：
   - 草稿與送出前的檢查清單在 `docs/upstream-issues/gpui-startup.md`；
   - patch 在 `docs/upstream-issues/patches/gpui-startup-000{1,2}-*.patch`。
   - 要不要送出由使用者決定，這一輪沒有建立任何 issue、PR 或 discussion。
3. **維持 ADR 0001：不 fork GPUI。**
   - 在 upstream 合併之前，FastPDF 的小檔首頁停在約 211–230 ms（安靜機器）。
   - B-8 的 < 200 ms 目標在 pin 的 GPUI 上無法穩定達成。
   - 合併後更新 pin，並依〈Validation〉重新量測。

## Consequences

- **B-8**：`docs/benchmarks/b8-app.md` 的「小檔首頁 < 200 ms 尚未達成」維持不變。原因已經拆解清楚，達成條件是 upstream 的 G1 或 G2。
- **GPUI pin**：之後更新 pin 時，可以用本文的時間線當基準，比對 platform 初始化、device 建立、視窗顯示各段有沒有變化。
- **R12**：「核心少的機器啟動會更慢」的推測不成立。風險改成「單核慢、GPU driver 慢、背景負載高」。
- **量測工具**：量測點只存在於 scratch 複本中，本文的〈方法〉足以重現。repo 沒有加入任何 GPUI 的量測程式碼。

## Alternatives considered

- **Fork GPUI 或在 repo 內 `[patch]` GPUI**：可以立刻拿到 G1、G2 的效果，但違反 ADR 0001，也要長期跟上 upstream。不採用。
- **F1（FastPDF 端預熱 driver）**：量測後不採用，見上文。
- **讓 GPUI 改用內顯**：內顯的 device 建立可能比較快，但會影響 render 效能，也牽涉 GPU 偏好設定。不在範圍內，沒有評估。
- **完全不建立系統字型集合，等第一次需要字型時才建**：可以多省 1 ms 左右，但 GPUI 要大改。G2 已經拿到絕大部分的效果。
- **把顯示視窗的成本移出關鍵路徑**（例如先 present 再 `RegisterDragDrop`）：列為後續 G5，需要先用 ETW 拆解 `SetWindowPlacement` 內部的時間（安靜時 12.6–14.9 ms）。

## 後續

1. **G3：延後建立 DragDropHelper**（GPUI）。目前在主執行緒上花 7–10 ms。沒有 G1 時直接省這段；有 G1 時它在 device 建立的陰影裡，這台機器上不會變快。
2. **G4：第一次 present 不等 vsync 執行緒**（GPUI）。
   - `open_window` 已經 draw 好第一個 frame，但要等 vsync 執行緒下一次 invalidate 才 present。
   - 19 次啟動中這段等待是 0–13.2 ms，平均 5.1 ms。
   - 在 `open_window` 之後直接 present，或立即要求一個 frame，平均可省 5 ms，最多 13 ms。
   - FastPDF 端可以在 `open_window` 之後對 HWND 呼叫 `InvalidateRect`，讓 message loop 立即送出 `WM_PAINT`，但這依賴 GPUI 的 paint 實作細節。用 `RDW_UPDATENOW` 同步重畫，會在 `App` 已經被借用時重新進入 GPUI。所以沒有試。
3. **G5：視窗顯示的成本**（GPUI）。
   - `SetWindowPlacement` 11.5–30.7 ms，`RegisterDragDrop` 3.2–10.5 ms。
   - swap chain 在 `WM_CREATE` 時以 1×1 建立，`WM_SIZE` 時再 `ResizeBuffers`。
   - 需要 ETW 或 PIX 拆解後才知道能不能縮短。
4. **F3：FastPDF 第一次 draw 的 7–9 ms**。量測第一頁 bitmap 上傳與 layout 各占多少；這屬於 UI／render 的範圍。
5. **R12 實機**：在真正的低階筆電（內顯、低時脈）上量同一條時間線，特別是 `D3D11CreateDevice` 與字型集合。bench-app 加上 `-Affinity` 之後（另一個 agent 進行中），R12 的近似量測可以重複執行。
6. **Upstream 追蹤**：如果 G1／G2 被接受，更新 GPUI pin，依〈Validation〉重新量測，並更新 B-8 與本 ADR 的狀態。

## Validation

- **重新量測的方法**：GPUI pin 更新（含 G1／G2，或任何 `gpui_windows` 啟動相關的改動）時：
  - 用 dist build，以 B-8 的 bench-app 對新舊 pin 做交替配對，至少 3 個乾淨配對。
  - 量測前確認沒有編譯中的 process，並記錄背景負載。
- **接受條件**：
  - 安靜機器上 3 頁檔的 `first_page_exact` 中位數 < 200 ms；
  - 啟動後的截圖與舊 pin 相同；
  - idle CPU 與 private bytes 不增加（±1 MB）。
- **需要拆解時**：在 scratch 的 GPUI 複本加上本文〈方法〉的量測點，對照上面的時間線表。

## 附錄：每次啟動的數字

時間是 ms，從 process 啟動起算；「字型集合」與「D3D11」是該段的耗時。

| 回合 | 啟動 | 變體 | 背景負載 % | `main` | platform 就緒 | 字型集合 | D3D11 | device 就緒 | 視窗顯示 | `window_visible` | `first_page_exact` | 截圖首個非空白 | idle CPU ms／10 s | idle private MB | peak private MB |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| R1 | n8 | base | 83 | 25.4 | 93.7 | 51.6 | 165.6 | 272.5 | 321.8 | 336.0 | 344.2 | 375.8 | 109.4 | 107.5 | 122.6 |
| R1 | n9 | G1 | 97 | 29.1 | 122.4 | 71.8 | 225.7 | 271.1 | 342.4 | 365.1 | 366.9 | 453.1 | 62.5 | 107.3 | 121.6 |
| R1 | n10 | G2 | 45 | 14.7 | 26.6 | 0.8 | 113.6 | 150.3 | 187.1 | 197.3 | 203.7 | 267.7 | 78.1 | 107.4 | 121.2 |
| R1 | n11 | F1 | 58 | 12.2 | 54.4 | 29.1 | 88.3 | 143.4 | 178.7 | 189.7 | 189.8 | 211.9 | 46.9 | 108.7 | 141.1 |
| R2 | n12 | F1 | 91 | 16.3 | 58.3 | 30.8 | 126.9 | 185.8 | 223.0 | 235.2 | 235.3 | 252.1 | 171.9 | 108.2 | 140.9 |
| R2 | n13 | G2 | 51 | 17.6 | 28.3 | 0.7 | 116.3 | 153.9 | 187.5 | 196.6 | 196.8 | 220.3 | 62.5 | 107.2 | 121.3 |
| R2 | n14 | G1 | 28 | 16.3 | 58.3 | 29.4 | 120.3 | 147.2 | 178.3 | 187.4 | 187.5 | 207.7 | 156.2 | 107.5 | 121.7 |
| R2 | n15 | base | 18 | 15.4 | 52.1 | 26.9 | 103.5 | 165.1 | 191.4 | 198.4 | 211.6 | 255.2 | 0.0 | 107.4 | 122.3 |
| R3 | n16 | base | 16 | 10.2 | 47.7 | 27.6 | 114.9 | 172.1 | 201.7 | 210.2 | 210.9 | 234.5 | 93.8 | 107.6 | 122.6 |
| R3 | n17 | G1 | 25 | 14.2 | 52.9 | 28.1 | 114.0 | 137.9 | 164.2 | 171.9 | 171.9 | 193.4 | 31.2 | 107.4 | 121.8 |
| R3 | n18 | G2 | 48 | 15.8 | 25.5 | 0.7 | 110.0 | 145.0 | 172.3 | 180.5 | 183.1 | 232.9 | 62.5 | 107.3 | 120.9 |
| R3 | n19 | F1 | 62 | 18.4 | 61.1 | 31.1 | 102.8 | 164.4 | 194.5 | 202.2 | 214.1 | 266.0 | 78.1 | 108.0 | 141.1 |
| R4 | n23 | G2 | 45 | 17.4 | 28.4 | 0.7 | 120.9 | 159.7 | 192.6 | 202.1 | 207.8 | 255.8 | 15.6 | 107.3 | 121.3 |
| R4 | n24 | base | 20 | 16.2 | 53.6 | 27.6 | 117.9 | 181.1 | 213.4 | 221.7 | 229.8 | 249.2 | 109.4 | 107.4 | 122.3 |

- **其他啟動**：
  - 時間線 1（負載 50%）與時間線 2（負載 68%）是 single 啟動，`first_page_exact` 分別為 223.3、253.1 ms。
  - affinity 的 3 次見〈R12〉。
- **G1 的字型集合**：G1 的這一段和 device 執行緒同時進行，所以 G1 的「字型集合」欄是在有 device 建立同時進行時量到的。
- **F1 的 D3D11**：F1 的「D3D11」欄是 GPUI 自己那次 `D3D11CreateDevice` 的耗時。預熱的 device 分別在 120.6、153.9、142.3 ms 就緒（n11、n12、n19）。
