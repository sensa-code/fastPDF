# B-8：App 層 KPI（啟動、首頁、idle、捲動）

對應 spec §28–§29、benchmark plan B-8。從 app **外部**量測：啟動到視窗出現、第一頁完全清晰、idle RAM／CPU、捲動與縮放的反應。Engine 層的數字（open、render、text）見 B-1（`benchmarks/baseline.json`）。

## 結論

- **第一個 frame 就是完全清晰的第一頁。** UI 第五輪把開檔和第 1 頁的 render 提前到 GPUI 啟動期間（`crates/fastpdf-ui/src/startup.rs`）。`first_paint` 到 `first_page_exact` 的差距從 33–38 ms 降到 0–0.1 ms。
- **小檔首頁 < 200 ms 尚未達成。** 最終版（F）的 `first_page_exact` 中位數：3 頁 219 ms（205–232 ms），300 頁 213 ms。第一頁和視窗幾乎同時出現，下限是 GPUI 自己的啟動：建立 D3D11 device 約 115 ms，視窗在 process 啟動後 204–219 ms 才出現。要再快只能縮短 GPUI 的啟動，見〈後續〉。
- **Idle RAM < 50 MB 未達成。** 開著文件 idle 時 private bytes 107–109 MB、working set 66–68 MB，大部分是 GPUI、D3D11 與 DirectWrite 的固定成本。
- **Idle CPU 很低，但不是 0。** idle 時 render worker 不執行，只剩 GPUI 的主執行緒與 vsync 執行緒。各回合在 0.2–1.4% 單核之間（整台 32 執行緒機器的 0.04% 以下），隨背景負載變動。F 回合 3 頁情境量到 1.4%，但量測期間系統負載升到 95%，需要在安靜的機器上確認。
- **exe 16.0 MiB**（F：16,730,624 B，含 render host 與檔案關聯），低於 30 MB 的目標。
- **平滑捲動的代價符合預期。** 滾輪捲動期間畫面變化的 frame 數加倍（20 → 46 frame/s），輸入停止後多 135 ms 動畫時間，首次反應時間不變（24 → 27 ms，在量測誤差內）。

## 方法

- 工具：`tools/bench-app/bench-app.ps1` 1.0.0，preset `fastpdf`。
  - 執行 `fastpdf <file.pdf>`（CLI 開檔），讀取 `FASTPDF_BENCH=1` 的 stdout 事件（process 內的時間戳）。
  - 以 `PrintWindow` 截圖偵測畫面變化，每次截圖約 15 ms。
  - `FASTPDF_SETTINGS_FILE` 與 `FASTPDF_RECENT_FILE` 設為空值，不讀寫使用者設定。
- 每個情境 3 次，取中位數；warm cache（檔案已在 OS cache 中）；dist build（fat LTO）。
- 情境：
  - **3 頁**：`small-text/three-pages-platypus-times.pdf`（7 KB），只量啟動與 idle。
  - **300 頁**：`large-text/dense-300p-times.pdf`，再加上 20 格滾輪、5 次 PageDown、3 次 Ctrl+滾輪縮放。
- 機器：AMD Ryzen 9 9950X（16C／32T）、125 GB RAM、RTX 5090 與內顯、1920×1080 @ 60 Hz、Windows 11 Pro 10.0.26200。
- 這是高階桌機的數字。核心數少、硬碟慢的機器上，GPUI 啟動與開檔都會更慢。

| 回合 | 程式 | exe 大小 | 量測時的背景負載 |
|---|---|---|---|
| R2 | `335b81f`（UI 第二輪） | 15,898,624 B | 4–77%，多數時間偏低 |
| R5 | `2f162e0` 加上 UI 第五輪（`8687f17`） | 16,366,592 B | 40–77%（其他 agent 在編譯） |
| F | `8a3ac54`（最終版：`tools/package.ps1` 的 dist build，含路徑改寫） | 16,730,624 B | 300 頁：11–27%；3 頁：23–95%（WSL 的 VM，不是 FastPDF） |

- R5 的 exe 多了 icon、VERSIONINFO 和繁中字串表。F 再加上 render host（`fastpdf-engine-remote`，opt-in，預設不啟動）與檔案關聯的命令列。
- R5 的絕對時間受背景負載影響：視窗出現時間慢了 11–25 ms，這段時間沒有任何 FastPDF 的程式在跑。比較 R2 與 R5 時，請看不受負載影響的指標，例如第一個 frame 到完全清晰的差距。
- F 的 3 頁情境第一次量測是 dist build 剛完成之後，exe 是第一次執行：`first_page_exact` 中位數 279 ms（216–361 ms），應該是 Defender 掃描新檔案與 cold image 造成的。表中 F 的 3 頁數字是 exe 已執行過之後重跑的 3 次。

## 結果

### 啟動（中位數，ms，從 process 啟動起算）

| 指標 | 3 頁 R2 | 3 頁 R5 | 3 頁 F | 300 頁 R2 | 300 頁 R5 | 300 頁 F |
|---|---|---|---|---|---|---|
| `main` 開始 | 9.4 | 14.7 | 11.7 | 11.7 | 16.0 | 14.1 |
| 視窗出現（`window_visible`） | 188.4 | 213.6 | 217.1 | 215.6 | 226.7 | 211.2 |
| 文件開好（`document_opened`） | 188.6 | 207.7 | 211.3 | 216.0 | 220.8 | 204.5 |
| 第一個 frame（`first_paint`） | 194.8 | 226.8 | 219.2 | 216.9 | 238.4 | 212.7 |
| 第一頁完全清晰（`first_page_exact`） | 232.8 | 226.9 | **219.2** | 250.3 | 238.4 | **212.7** |
| `first_paint` → `first_page_exact` | 38.0 | **0.1** | **0** | 33.4 | **0** | **0** |
| 截圖測到的第一個非空白畫面 | 257 | 296 | 249 | 240 | 307 | 256 |

- F 的範圍：3 頁 `first_page_exact` 204.9–232.4 ms；300 頁 207.6–467.2 ms，其中一次 467 ms 的離群值發生在背景負載升高時。

- R5 的 `document_opened` 是 view 接手 session 的時間，不是 engine 開檔完成的時間。開檔與第 1 頁 render 在視窗出現前就已在背景執行緒完成：debug log 顯示 59 ms 時 4 個 tile 已經排入。
- 截圖測到的時間比 process 內的事件晚 30–70 ms：視窗要先出現，DWM 才能合成並讓 `PrintWindow` 拿到內容，而且截圖本身是 15 ms 的取樣。

### Idle（開檔後靜置 10 秒）

| 指標 | 3 頁 R2 | 3 頁 R5 | 3 頁 F | 300 頁 R2 | 300 頁 R5 | 300 頁 F |
|---|---|---|---|---|---|---|
| Private bytes（MB） | 114.7 | 107.3 | 107.5 | 119.1 | 109.4 | 109.4 |
| Working set（MB） | 73.0 | 66.0 | 66.0 | 76.4 | 67.7 | 67.7 |
| GPU dedicated（MB） | 34.8 | 31.6 | 31.6 | 34.8 | 31.6 | 31.6 |
| 執行緒數 | 119 | 116 | 116 | 119 | 116 | 116 |
| CPU（% 單核） | 0.2 | 0.8 | 1.4 | 0.8 | 0.9 | 0.6 |

- R5 另外做了 idle 診斷（dist build，靜置 10 秒）：主執行緒每秒 34 Mcycles、GPUI 的 vsync 執行緒每秒 21 Mcycles，render worker 為 0。滾兩格之後再量，結果一樣（57.6 對 61.2 Mcycles/s），表示平滑捲動的動畫結束後不會繼續要求 frame。
- idle CPU 的 3 頁 R2 → R5 → F（0.2 → 0.8 → 1.4）落在各回合的變動範圍內：F 的 3 次是 0.6–2.2%，300 頁 F 只有 0.6%（0.3–0.9%），而 F 3 頁量測時系統負載升到 95%。目前不能判定為退步，需要在安靜的機器上重測；若確認，再用 ETW 找出主執行緒每個 vsync 被喚醒的原因。

### 互動（300 頁，中位數）

| 指標 | R2 | R5 | F |
|---|---|---|---|
| 滾輪：首次畫面變化（ms） | 24.4 | 26.5 | 21.7 |
| 滾輪：最後一次輸入到畫面穩定（ms） | 22 | 157 | 139 |
| 滾輪：輸入期間每秒變化的 frame | 20.0 | 45.8 | 52.0 |
| 滾輪：CPU（扣除截圖，ms） | 49.8 | 114.8 | 261.3 |
| PageDown：首次畫面變化（ms） | 23.0 | 20.2 | 23.3 |
| Ctrl+滾輪縮放：首次畫面變化（ms） | 22.4 | 16.5 | 26.2 |
| 峰值 private bytes（MB） | 328.8 | 311.3 | 311.9 |
| 互動後 idle 5 秒的 private bytes（MB） | 311.1 | 259.6 | 260.1 |

- 滾輪的「畫面穩定」時間與 CPU 增加來自平滑捲動：每格滾輪用 120 ms 的 ease-out 分成多個 frame 畫出。設定檔 `smooth_scrolling = false` 可以回到 R2 的行為。
- F 的滾輪 CPU（183–386 ms，中位數 261 ms）比 R5 高，量測時有 WSL 的負載。捲動期間的 frame 數與畫面穩定時間和 R5 相近，看不出程式路徑的變化。R5 → F 之間沒有修改捲動與 render 路徑，render host 預設也不啟動。
- 互動後的 private bytes 下降（311 → 260 MB）不是這兩輪刻意改善的項目，可能只是量測時機不同造成的差異。

## 後續

1. **GPUI 啟動時間**：要讓小檔首頁進入 200 ms 以內，得縮短 GPUI 的平台初始化。主要成本是 D3D11 device 建立，約 115 ms。可以研究的方向：
   - 延後建立 DirectWrite 字型集合；
   - 平行化 device 建立；
   - 先顯示視窗，再初始化 renderer。

   這需要修改 GPUI（zed upstream），應另開 ADR。
2. **安靜機器重測**：R5 與 F 的絕對時間、idle CPU 與捲動 CPU 都受背景負載影響（F 是 WSL 的 VM）。正式發佈前照 `docs/RELEASE.md` §1.4 在沒有其他負載的機器上重跑，再把結果補進這份文件。
3. **Idle RAM**：50 MB 的目標需要先拆出 GPUI／D3D11／DirectWrite 的固定成本。可以先用空視窗（`launch-empty`）量測 FastPDF 本身之外的部分。
4. **對照組**：SumatraPDF、Adobe Acrobat Reader 沒有安裝在量測機上，尚未比較（`tools/bench-app/README.md`）。

## Idle 歸因與 KPI（第六輪，2026-10-04）

這一節回答三件事：Idle RAM 與 Idle CPU 的 KPI 用哪個指標（定義在 `benchmarks/README.md`〈App 層 KPI 定義〉），idle CPU 是誰造成的，idle RAM 花在哪裡。

### 結論

- **Idle RAM 以 private working set 判定時，已達成 < 50 MB。**
  - 3 頁文件：24.9 MB（24.7–25.0）；空視窗：16.3 MB；GPUI 最小視窗：15.0 MB。
  - 同一時間 private bytes（commit）是 107.4 MB、working set 66.2 MB，都超過 50 MB。差距主要來自 GPU driver 與共用頁面，不是 FastPDF 自己的資料（見〈Idle RAM 拆解〉）。
- **FastPDF 在 idle 時沒有要求任何 frame。** 有 probe 的 build 在 idle 10 秒內的 render、prepaint、paint、wake 次數，中位數都是 0。20 次 run 中有 3 次各畫了 1 個 frame（都是原本 GPUI 的 build），當時沒有輸入，也沒有背景工作喚醒。
- **Idle CPU 全部來自 GPUI 的 vsync 迴圈與 GPU driver。**
  - 主執行緒每秒被喚醒 113–234 次，`VSyncProvider` 每秒 64–101 次；
  - NVIDIA D3D11 driver 有一條 thread 固定每秒醒來 60 次，與 GPUI 無關。
- **GPUI patch（沒有視窗要求 frame 時 park vsync thread）的效果**，交替配對 4 對：
  - 主執行緒喚醒：139 → 0 次／秒；`VSyncProvider`：68 → 1.1 次／秒；
  - 整個 process 的 cycles：49.2 → 4.9 Mcycles／秒（−90%）；
  - CPU time：1.41% → 0.08% 單核。

  剩下的幾乎都是 NVIDIA driver 那條 thread。Patch 與 upstream 草稿見 `docs/upstream-issues/gpui-idle.md`。
- **Idle CPU 的 KPI**（≤ 0.1% 單核，而且主執行緒與 `VSyncProvider` 每秒醒來 ≤ 2 次）：
  - 目前的 GPUI：未達成，1.41%，主執行緒 139 次、`VSyncProvider` 68 次；
  - 套用 GPUI patch 後：達成，0.08%，0 次與 1.1 次。
- **輸入反應、捲動延遲、frame pacing 沒有可分辨的退步。**
  - idle 後第一個輸入到 paint 的時間：4.2 → 0.6 ms（中位數，3 對；A 端有 build 瑕疵，見〈量測上的注意事項〉）。
  - 外部截圖量到的縮放延遲中位數多了 4 ms（24.9 → 28.9 ms），但數值集中在一或兩個截圖週期（約 23 ms 或 42 ms），4 對中 A、B 各有 1 次落在 42 ms。依 patch 的邏輯，互動期間的行為和原本相同（見〈輸入反應〉），列為待確認。
- **低核心近似**（`-Affinity`，各 1 次）：4 核與 2 核下的首頁時間（212.5、238.1 ms）落在全核的範圍內（中位數 239.9 ms，204.6–345.1 ms）。idle 行為與 thread 數不變。
- **508 px tile（atlas 對齊）**：
  - 不影響開檔後的 idle；
  - 互動後 idle 的 private bytes 207.2 → 176.8 MB（−30 MB），peak private bytes 250.2 → 219.0 MB；
  - private working set 則多了 1.6 MB。

  需要修改 `fastpdf-core` 的測試，所以只提出 diff，沒有實作（見〈508 px tile 實驗〉）。

### 方法

- 工具：`tools/bench-app` 1.1.0：
  - `-ThreadDetail`：逐 thread 的 `QueryThreadCycleTime` 與 context switch；
  - `-MemoryDetail`：`VirtualQueryEx` 分類加上 working set；
  - `-AppProbe`、`-Affinity`；
  - 新的記憶體欄位：`private_ws_mb`、`commit_charge_mb`。
- Build：全部是 dist profile，從 HEAD `8270b3e` 以 `git archive` 匯出後在 scratch 建置，不修改 repo 的 `Cargo.toml`。

  | 代號 | 內容 |
  |---|---|
  | HEAD | `8270b3e` 原樣 |
  | instr | HEAD 加上 scratch instrumentation：view 的 render、prepaint、paint、wake 計數、輸入到 paint 的延遲、paint 間隔，以及 `HeapSummary`／`HeapWalk` 的 heap 統計。只在 `FASTPDF_BENCH=1` 時多一條等待 event 的 thread，平常不醒來 |
  | instr+patch | instr，加上以 `[patch]` 指向套了 vsync park patch 的 GPUI（zed `a84689073` 的副本） |
  | instr+508 | instr，`DEFAULT_TILE_SIZE` 512 → 508 |
  | gpui-min | 同一個 GPUI rev、相同 features、rustflags 與 profile 的最小視窗（一個 view 畫純色背景），大小與 FastPDF 預設視窗相同（826×918） |

- 情境：3 頁 `small-text/three-pages-platypus-times.pdf`，加上 20 格滾輪、5 次 PageDown、3 次 Ctrl+滾輪；另有空視窗（`launch-empty`）。
- 交替配對：
  - 主要的 A／B 是 instr 對 instr+patch，共 4 對（第 1 對單獨跑，之後 3 輪按 instr、instr+patch、instr+508 的順序輪流）；
  - 另外 3 對量 idle 後第一個輸入的延遲（`lat-*`）。這 3 對的 A 端有 build 瑕疵，見〈量測上的注意事項〉。
- 機器：與上面相同（Ryzen 9 9950X、RTX 5090、driver 32.0.16.1088、1920×1080 @ 60 Hz、100% DPI）。
- 背景負載：
  - 量測前都確認沒有 cargo、rustc、link 在跑；
  - 但使用者的 WSL VM 與其他 agent 的工作仍在進行，主要 A／B 期間 idle 窗口的 `system_cpu_busy_pct` 是 19–100%，`lat-*` 是 12–31%；
  - 另一個 agent 也在啟動 `fastpdf.exe`，每次啟動前都會等它結束。
- GUI 共啟動 23 次（上限 24），每次結束後確認沒有殘留的 process。

### Idle CPU：誰在醒來（3 頁，idle 10 秒，交替配對 4 對的中位數與範圍）

| thread | 原本：喚醒／秒 | 原本：Mcycles／秒 | patch 後：喚醒／秒 | patch 後：Mcycles／秒 |
|---|---:|---:|---:|---:|
| 主執行緒（GPUI UI thread） | 139（113–234） | 28.3（21.4–33.9） | 0（0–0.3） | 0（0–0.3） |
| `VSyncProvider`（GPUI） | 68（64–101） | 16.0（10.2–16.9） | 1.1（1.0–1.2） | 0.12（0.08–0.12） |
| NVIDIA driver（`nvwgf2umx.dll`，103 條中的 1 條） | 60–69 | 2.3–5.7 | 60–62 | 2.7–5.6 |
| Windows thread pool（`ntdll`，5 條） | 1–39 | 0.5–8.6 | 0.3–0.4 | 0.3–0.5 |
| 其他（render worker 2、COM 2、DManip、CRYPT32、probe） | 0 | 0 | 0 | 0 |
| **整個 process** | **309（242–362）** | **49.2（34.4–65.0）** | **62.2（61.5–63.5）** | **4.9（3.1–6.2）** |
| CPU time（% 單核） | 1.41（0.16–1.56） | | 0.08（0–0.16） | |

- 主執行緒的喚醒次數比 60 多，而且各次差很多（113–234）。除了每個 vsync 的 `WM_PAINT`，還有其他訊息；負載高時更多。patch 後一律為 0，表示這些都跟著 vsync 的 invalidation 而來。
- 安靜時段的 3 對（`lat-*`，idle 窗口忙碌 12–31%）結果一致：
  - 整個 process：29.3（18.8–33.9）→ 2.7（2.6–2.9）Mcycles／秒；
  - 主執行緒喚醒：79 → 0.1 次／秒；
  - CPU time：0.62% → 0%（0–0.16%）。
- GPUI 最小視窗（gpui-min，各 1 次）：
  - 原本：主執行緒 110、`VSyncProvider` 65 次／秒，31.2 Mcycles／秒，CPU time 78 ms／10 s；
  - patch 後：0 與 1.0 次／秒，2.9 Mcycles／秒，0 ms。

  FastPDF 與 GPUI 最小視窗的 idle 行為相同，差別只在主執行緒每次被喚醒時多做的一點工作。
- CPU time 以 15.6 ms 為單位計數，10 秒窗口的解析度約 0.16%；cycles 與喚醒次數才是可靠的比較（KPI 定義的第二個條件就是為此而設）。

### FastPDF 自己有沒有要求 frame

instr build 的 probe（idle 窗口前後各讀一次計數）：

| | render | prepaint | paint | wake（背景工作喚醒 UI） |
|---|---:|---:|---:|---:|
| 原本 GPUI（instr、instr+508，含空視窗與 affinity），13 次 | 0（0–1） | 0（0–1） | 0（0–1） | 0 |
| patch 後，7 次 | 0 | 0 | 0 | 0 |

- 13 次中有 3 次各畫了 1 個 frame，當時沒有輸入，也沒有 wake。可能是其他程式的視窗搶走焦點，造成 activation 改變；次數太少，看不出和 patch 有關。
- 結論：idle CPU 不是 FastPDF 造成的，FastPDF 這一側沒有需要修改的地方。平滑捲動、tile 上傳與 retire queue 在動畫結束後都不再要求 frame（`smooth_scroll.rs`、`viewport.rs` 的設計如預期運作）。

### 輸入反應、捲動延遲、frame pacing（交替配對 4 對）

| 指標 | 原本 GPUI | patch 後 |
|---|---:|---:|
| 滾輪：首次畫面變化（外部截圖，ms） | 33.4（9.6–43.0） | 39.9（12.1–52.1） |
| 滾輪：最後一次輸入到畫面穩定（ms） | 151（130–187） | 147（131–164） |
| PageDown：首次畫面變化（ms） | 24.7（23.1–27.0） | 26.5（21.6–27.9） |
| Ctrl+滾輪縮放：首次畫面變化（ms） | 24.9（23.1–43.1） | 28.9（26.1–43.3） |
| 輸入到 paint（app 內，p50，ms） | 8.0（5.5–16.9） | 6.8（5.4–8.7） |
| 輸入到 paint（app 內，最大，ms） | 21.0（10.6–36.5） | 20.5（12.6–27.9） |
| 互動期間 frame 間隔 p50（app 內，ms，3 對） | 17.1（16.6–40.9） | 17.8（16.7–18.0） |
| 互動期間超過 34 ms 的 frame 間隔（3 對） | 9（7–32） | 7（7–8） |
| idle 後第一個輸入到 paint（app 內，ms，`lat-*` 3 對） | 4.2（0.5–5.3） | 0.6（0.5–4.1） |

- 外部截圖的時間解析度約 ±20–35 ms。A／B 的差距在解析度之內，而且各對的方向不一致。
- app 內的量法：
  - 「輸入到 paint」是輸入 handler 到下一次 `paint_viewport` 的時間；
  - frame 間隔把相距 4 ms 內的 paint 視為同一個 frame；
  - 超過 34 ms 的間隔大多是兩次 PageDown 或縮放之間的空檔，不是掉 frame。
- **為什麼互動中的行為不變**：patch 只在「超過 1 秒沒有任何 frame 要求」時才 park。互動期間各階段之間的空檔約 0.2 秒，vsync thread 一直在 tick，程式路徑與原本相同，只多一次 atomic swap。
- **idle 後的第一個輸入**：原本要等下一次 vsync tick（平均半個 frame）；patch 後在要求 frame 時立刻 invalidate，所以通常更快。
- **縮放的外部延遲**：中位數多 4 ms，但數值集中在 23 ms 與 42 ms 兩個截圖週期上，依上面的程式路徑分析，應該是截圖量化造成的。送 upstream 前建議以 app 內逐次輸入的時間再確認一次。

### Idle RAM 拆解（idle 10 秒結束時，MB）

- (a)、(b) 各 1 次，(c) 是 4 次中的一次（A2），4 次之間的差異在 0.5 MB 內。
- heap 用 app 內的 `HeapSummary`／`HeapWalk`。FastPDF 與 GPUI 都使用 segment heap（GPUI manifest），從外部分不出 heap。
- 其餘類別用 `VirtualQueryEx` 與 `QueryWorkingSet`，詳見 `tools/bench-app/README.md`〈Idle 診斷〉。

| | (a) GPUI 最小視窗 | (b) FastPDF 空視窗 | (c) FastPDF 3 頁 | (c) − (a) |
|---|---:|---:|---:|---:|
| **Private working set（KPI）** | **15.0** | **16.3** | **24.7** | **+9.7** |
| Working set | 57.6 | 55.7 | 66.0 | +8.4 |
| Private bytes（commit charge） | 82.5 | 94.6 | 107.4 | +24.9 |
| commit charge + shared commit | 99.6 | 111.7 | 124.5 | +24.9 |
| GPU dedicated | 19.5 | 27.6 | 34.9 | +15.4 |
| thread 數 | 114 | 115 | 117 | +3 |
| **private commit 分類** | | | | |
| heap（所有 process heap） | 9.6 | 10.5 | 15.7 | +6.1 |
| stack | 2.8 | 2.8 | 2.8 | 0 |
| TEB／PEB | 0.9 | 0.9 | 0.9 | 0 |
| 其他 `VirtualAlloc`（GPU driver、D3D、DComp 等） | 35.3 | 46.2 | 54.1 | +18.8 |
| image（DLL 的 copy-on-write 頁） | 31.8 | 31.9 | 31.9 | 0 |
| kernel 等（private bytes 與上面合計的差） | 2.1 | 2.4 | 2.2 | 0 |
| **private working set 分類** | | | | |
| heap | 7.2 | 8.0 | 13.4 | +6.2 |
| stack | 1.4 | 1.4 | 1.3 | 0 |
| TEB／PEB | 0.9 | 0.9 | 0.9 | 0 |
| 其他 `VirtualAlloc` | 2.4 | 2.8 | 6.2 | +3.8 |
| image | 3.2 | 3.2 | 3.2 | 0 |
| mapped file／pagefile section | 0 | 0 | 0 | 0 |
| **不算在 private 的部分** | | | | |
| pagefile-backed section（shared commit，`mapped_pagefile`） | 6.0 | 6.0 | 6.0 | 0 |
| 檔案映射的位址空間（字型、PDF） | 6.1 | 78.6 | 82.6 | +76.5 |

解讀：

- **GPUI＋D3D11＋NVIDIA 的固定成本**就是 (a)：private working set 15 MB、private bytes 82.5 MB。
  - private bytes 中有 31.8 MB 是 image 的 copy-on-write commit，主要是 `nvwgf2umx.dll` 的 24.6 MB、`nvppex.dll` 3.7 MB、`nvgpucomp64.dll` 2.3 MB，實際寫過的只有約 1.5 MB；
  - 另有 35 MB 的 `VirtualAlloc`，最大的幾塊是 14.4 MB（write-combined）、12.2 MB、5 MB，幾乎都不在實體記憶體中。
- **(b) − (a)，FastPDF 的 UI**：private bytes +12.1 MB、private working set +1.3 MB。
  - 主要是 driver 端的 +10.9 MB，同時 GPU dedicated 多了 8.1 MB（UI 繪製用的 GPU 資源，例如 glyph 與 icon 的 atlas；沒有逐項拆開）；
  - heap 只多 0.9 MB；
  - 字型檔映射多了 72 MB 位址空間（DirectWrite 映射繁中 UI 字型），不佔 private 也不佔 commit。
- **(c) − (b)，開著 3 頁文件**：private bytes +12.8 MB、private working set +8.4 MB。
  - heap +5.2 MB：tile 的 CPU 端影像，heap 中有 8 個 ≥ 512 KB 的大區塊，共 6.8 MB；
  - driver 端 +7.9 MB（其中 3.4 MB 在實體記憶體中），對應 GPU dedicated +7.3 MB 的 tile texture；
  - render worker 2 條 thread。
- **FastPDF 可以控制的部分**，以 private working set 計約 10 MB，也就是 (c) 與 (a) 的差：
  - tile 的 CPU 端影像：約 5–7 MB，包含顯示中的 tile 與 P1–P3 預先 render 的 tile；
  - 與 texture 上傳相關的 driver 記憶體：約 3–4 MB；
  - UI（文字排版、glyph atlas 等）：約 1.3 MB。

  前兩項是「開檔即看、捲動不等待」的直接成本。顯示中的 tile 也必須保留 CPU 端資料，因為 GPUI 在 device lost 時要從 `RenderImage` 重新上傳（`docs/audit/gpui.md`）。本輪沒有在 `fastpdf-render`、`fastpdf-cache`、`fastpdf-search` 找到可以減少 idle private working set 而不影響延遲的項目。
- 因為 private working set（24.9 MB）只有目標的一半，目前沒有為 idle RAM 犧牲捲動延遲的理由。
- **量測時點會影響結果。** 3 頁文件的 heap 在 idle 窗口開始時（啟動後約 2.9 秒）是 23.5 MB committed、21 MB 在實體記憶體中，其中有 10 個大區塊共 13.4 MB；窗口結束時（約 13 秒）只剩 15.6 MB、13.3 MB、8 個大區塊共 6.8 MB。
  - 差額約 8 MB，是 Hayro render thread 閒置 5 秒後結束時釋放的（`IDLE_EXIT`；每條 thread 的 vello render context 約 3.3 MiB）。
  - 20 次 3 頁 run 都一樣（7.7–8.1 MB，都是 2 個大區塊），空視窗與 GPUI 最小視窗沒有這個變化。
  - 所以 KPI 取窗口結束時的值，反映的是穩定的 idle 狀態；若在開檔後 5 秒內量，private working set 會多約 8 MB。

### Thread 數量與來源（idle 結束時）

| 來源 | (a) | (b) | (c) |
|---|---:|---:|---:|
| NVIDIA D3D11 driver（`nvwgf2umx.dll`） | 103 | 103 | 103 |
| Windows thread pool（`ntdll`；GPUI 的 background executor 也用它） | 5 | 5 | 5 |
| COM（`combase.dll`） | 2 | 2 | 2 |
| DirectManipulation（`DManip Delegate Thread`） | 1 | 1 | 1 |
| GPUI：主執行緒、`VSyncProvider` | 2 | 2 | 2 |
| `CRYPT32.dll` 起點的 thread | 0 | 1 | 1 |
| FastPDF：`fastpdf-render-0`、`-1` | 0 | 0 | 2 |
| scratch probe（`bench-probe`，HEAD 沒有） | 1 | 1 | 1 |
| **合計** | **114** | **115** | **117** |

- HEAD（沒有 probe）是 116 條，與前面各輪一致。FastPDF 自己的 thread 只有 2 條 render worker。
- Hayro 的 render thread 在閒置 5 秒後結束（`fastpdf-engine-hayro/src/pool.rs` 的 `IDLE_EXIT`），開檔用的 `fastpdf-open` 也已結束。
- 103 條是 NVIDIA driver 自己建立的，不論 affinity 限制在 2 或 4 個 CPU 都一樣；其中只有 1 條會週期性醒來。
- `CRYPT32.dll` 起點的 thread 只出現在 FastPDF，來源還沒確認（應該是啟動路徑上某個 Windows 元件建立的），在 idle 時不醒來。

### 低核心近似（`-Affinity`，instr build，3 頁，各 1 次，只量啟動與 idle）

mask 依本機拓撲選擇：相鄰的兩個邏輯 CPU 是同一個實體核心，所以每個核心只取一個邏輯 CPU。

| | 首頁完全清晰（app 自報，ms） | 視窗出現（app 自報，ms） | 外部：視覺完成（ms） | idle：Mcycles／秒 | idle：private working set（MB） | thread |
|---|---:|---:|---:|---:|---:|---:|
| 全部 32 個邏輯 CPU | 239.9（204.6–345.1） | 235.5（203.8–339.1） | 309（246–690） | 49.2（34.4–65.0） | 24.9（24.7–25.0） | 117 |
| 4 個核心（`0x55`） | 212.5 | 206.2 | 252.6 | 38.5 | 24.6 | 117 |
| 2 個核心（`0x5`） | 238.1 | 236.9 | 278.4 | 33.8 | 24.5 | 117 |

- 全核那一列：
  - 啟動三欄是 512 px 的 instr 與 instr+patch 共 10 次，排除背景負載 100% 的那一次。patch 不影響啟動（vsync thread 前 1 秒照常 tick）。
  - idle 三欄是原本 GPUI 的 4 次。
- 4 核與 2 核都落在全核的範圍內，而且在中位數以下。3 頁小檔的啟動被單執行緒的 GPUI 初始化主導（D3D11 device 建立約 115 ms），開檔與第 1 頁 render 和它重疊，並不需要很多核心。
- 限制：
  - affinity 只減少核心數，每個核心仍是 5 GHz 等級，快取、記憶體與 GPU 也都還在。這不能代表低階筆電的絕對時間，只說明 FastPDF 不依賴多核心；
  - 每種只有 1 次，背景負載也會落在同樣的 CPU 上；
  - 300 頁、掃描檔等 render 較重的文件，以及互動中的表現，沒有在低核心下量。

### 508 px tile 實驗（atlas 對齊）

- GPUI 的 atlas texture 最小是 1024×1024。FastPDF 的 tile 是 512 px 再加上兩側各 2 px 的 gutter，所以是 516 px，一張 1024² texture 放不下兩個。
- 把 tile 改成 508 px（加 gutter 剛好 512），一張 texture 可以放 4 個。
- instr 對 instr+508，交替 3 輪（另有 `lat-*` 的 3 次 A 端也是 508，結果相同）：

| | 512（instr，4 次） | 508（instr+508，3 次） |
|---|---:|---:|
| 開檔後 idle：private working set（MB） | 24.9（24.7–25.0） | 24.8（24.6–25.1） |
| 開檔後 idle：private bytes（MB） | 107.4（107.2–107.7） | 107.5（106.9–107.8） |
| 互動後 idle：private bytes（MB） | 207.2（206.8–207.3） | 176.8（176.5–177.2） |
| 互動後 idle：commit charge + shared（MB） | 224.3（224.0–224.4） | 193.9（193.7–194.4） |
| 互動後 idle：private working set（MB） | 60.5（60.1–60.7） | 62.1（62.0–62.2） |
| peak private bytes（MB） | 250.2（249.7–250.3） | 219.0（218.7–219.3） |

- 減少的應該是縮放後 driver 端為 atlas texture 配置的 commit：`docs/audit/gpui.md` 實測 NVIDIA 的 commit 大約和 VRAM 配置 1:1 成長；本輪互動後沒有量 GPU 記憶體，所以這是推論。
- 開檔後的 idle 不變；互動後的 private working set 反而多了 1.6 MB，原因沒有追查（508 時同樣面積的 tile 數多約 1.6%）。
- **本輪的判斷**：對 KPI（private working set）沒有幫助；改 `DEFAULT_TILE_SIZE` 要一併修改 `fastpdf-core` 的一個測試；B-3 也要重跑。所以先只提出 diff。
- **後續：已採用**。B-3 的配對重測（`docs/benchmarks/b3-b4-tiles-workers.md`〈508 vs 512〉）顯示 viewport fill 時間在雜訊範圍內，所以採用 508。理由是 GPU atlas 的空間不再浪費，這在內顯上就是系統記憶體。

### 量測上的注意事項

- **build 瑕疵（`lat-*` 的 A 端）**：
  - 同一個 target dir 先 build 了 instr+508，再 build 另一份從 `git archive` 匯出、內容是 512 的副本。
  - workspace 內 path package 的 fingerprint 以相對路徑計算，匯出檔的 mtime 又比較舊，cargo 因此沿用了 508 的 `fastpdf-render` artifact。build log 只重新編譯了 `fastpdf-ui` 與 `fastpdf-app`；互動後的 private bytes（175–177 MB）也和 508 一致。
  - 所以 `lat-*` 的 A 端實際上是「原本 GPUI＋508 px tile」。idle 不受 tile 大小影響（508 與 512 的 idle 相同），但 idle 後第一個輸入的延遲那一列不是乾淨的 A／B。
  - 本節其餘的 A／B 都來自乾淨的 4 對。
  - 之後在 scratch 建置多份匯出副本時，請每份用自己的 target dir，或在建置前更新所有原始檔的 mtime。
- **背景負載**：主要 A／B 的 idle 窗口忙碌度是 19–100%。負載高時，原本 GPUI 的主執行緒與 `VSyncProvider` 喚醒次數會增加（例如 100% 時分別是 146 與 101 次／秒），CPU time 也跟著增加；patch 後兩者在任何負載下都是 0 與約 1 次／秒。
- **HEAD 與 instr 的差別**：HEAD 只跑了 1 次驗證（private working set 25.7 MB、private bytes 108.4 MB、116 條 thread）。和 instr 的差在 1 MB 以內，instr 多 1 條 thread。那次 NVIDIA driver 在 idle 期間特別忙（28 Mcycles／秒、96 次／秒），CPU time 不具代表性。

### 後續（本輪新增）

1. **GPUI vsync park patch**：照 `docs/upstream-issues/gpui-idle.md` 的步驟先和 zed 維護者討論。若決定在 FastPDF 先用 `[patch]` 套用，需要新的 ADR（依賴策略，ADR 0001）。
2. **縮放延遲**：用 app 內逐次輸入的時間戳記確認縮放沒有變慢，再送 upstream。
3. **508 px tile**：已採用（`1a252c6`）。B-3 的配對重測在雜訊範圍內，見下一節的最終量測。
4. **低階機器**：iGPU 筆電、高更新率螢幕上重量 idle CPU 與 RAM（R12）。NVIDIA driver 那條每秒 60 次的 thread，在其他 GPU 上不一定存在。
5. **probe**：`-AppProbe` 的 FastPDF 端目前只在 scratch。若要讓之後的 B-8 自動檢查「idle 不畫 frame」，可以把 probe 併入 `fastpdf-app`／`fastpdf-ui`，只在 `FASTPDF_BENCH=1` 時啟用。

## 最終版（第七輪，`21dd4da`）

- **build**：`tools/package.ps1` 的 dist build，exe 16,823,296 B（16.0 MiB）。內容包含 508 px tile、render host（opt-in，預設不啟動）、MSIX 打包工具，以及 B-8 工具 1.1.0。
- **量測前**：先執行 5 次 `--version`，讓新 exe 完成 Defender 掃描並載入快取。
- **背景負載**：使用者的 WSL VM、本機的 llama-server、Defender、Google Drive 都在跑。每次啟動前的瞬間負載常常是 79–97%，idle 窗口是 13–99%。這些都不是 FastPDF 的程式，量測時沒有動它們。
- **情境與次數**：3 頁情境量了兩組，每組 3 次。第一組帶 `-ThreadDetail`，開始時負載 93–97%；第二組在負載降下來後重跑。300 頁情境 3 次。

| 指標（中位數） | 3 頁（重跑） | 3 頁（第一組） | 300 頁 |
|---|---|---|---|
| `window_visible`（ms） | 205.6（190.8–367.6） | 223.4（199.8–410.4） | 326.9（302.3–661.3） |
| `first_page_exact`（ms） | **218.2**（204.6–368.5） | 235.9（201.1–411.6） | 336.7（304.8–662.7） |
| idle private working set（MB）＝ KPI | **24.7** | 24.6 | 26.5 |
| idle private bytes（MB） | 107.4 | 107.3 | 109.3 |
| idle commit charge（MB） | — | 124.4 | 126.4 |
| idle CPU（% 單核） | 0.2（0–1.4） | 1.1（0.3–2.0） | 0.3（0.2–1.7） |
| 主執行緒／vsync thread 喚醒（次／秒） | — | 97／65 | — |
| peak private bytes（MB） | 121.6 | 122.2 | **280.5** |
| 互動後 idle：private bytes／private working set（MB） | — | — | **217.3**／86.0 |
| 滾輪／PageDown／縮放的首次畫面變化（ms） | — | — | 13.6／22.1／30.0 |

- **KPI 判定**（定義見 `benchmarks/README.md`）：
  - Idle RAM < 50 MB：**達成**，24.7 MB。
  - 小檔首頁 < 200 ms：**未達成**，中位數 218 ms，最快 205 ms，和 ADR 0009 安靜回合的 211–230 ms 一致。下限是 GPUI 的啟動；G1、G2 兩個 upstream 草稿合計預期可降到約 170–190 ms。
  - Idle CPU：**未達成定義**。CPU time 已經很低（0.2–1.1% 單核），但主執行緒每秒仍被 GPUI 的 vsync 迴圈喚醒約 100 次。草稿 `gpui-idle` 可以降為 0。
  - exe < 30 MB：**達成**。
- **與 F 回合相比（300 頁）**：peak private 311.9 → 280.5 MB，互動後 idle 260.1 → 217.3 MB，和 508 px tile 實驗的減少量相符。
- **300 頁情境的啟動時間比 F 回合高約 120 ms**，是因為這 3 次啟動時系統負載偏高（idle 窗口中位數 55%，最高 99%），不代表程式路徑有變。可以對照同一個 build 在 3 頁重跑時的 218 ms。

