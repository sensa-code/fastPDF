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
