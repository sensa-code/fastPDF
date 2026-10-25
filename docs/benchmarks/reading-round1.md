# 閱讀 KPI：round 1 導覽功能之後（2026-10-25）

對應 spec §28–§30、benchmark plan B-1 與 B-8、[ADR 0014](../adr/0014-encrypted-documents.md)、[ADR 0015](../adr/0015-external-links.md)。round 1 加入上一個／下一個檢視、加密文件的密碼提示、頁面標籤與跳頁、搜尋選項與結果清單、文件內的連結與外部連結確認、文件內容面板、記住閱讀位置，以及工具列的「⋯」選單。這份文件確認這些功能沒有拖慢閱讀路徑：開檔、第一頁、idle 與記憶體。

- **舊版**：`90a66d8`（寫入功能之後的 KPI 紀錄，ADR 0013）。
- **新版**：`d28b5e6`（round 1 的最後一個程式 commit；本文件所在的 commit 只改文件與 doc comment）。

## 結論

- **B-1（engine 層）沒有退步。**
  - 99 個檔案的狀態完全相同。
  - 幾何平均（新／舊）：open 0.965、first page 0.998、time to first page 0.970、text 1.043、RSS peak 1.001、private peak 1.000。
  - 5 對時唯一超過門檻的檔案，用 15 對重測後是 x1.025，屬於雜訊。
  - 一項改善：Info 字串很長的惡意檔案，讀取文件資訊從約 1.4 ms 降到 0.1 ms（字串解碼有了 16 KiB 上限）。
- **B-8 啟動沒有可判定的差異，KPI 仍然達成。**
  - 量測期間機器上有其他持續的負載，啟動時間主要跟著負載走。
  - 每對的差（新 − 舊）的中位數：3 頁 −3.3 ms（8 對，範圍 −245～+48 ms）、300 頁 +6.7 ms（9 對，範圍 −83～+87 ms），兩個方向都有。
  - 啟動前負載 ≤ 20% 的幾次，第一頁完全清晰的中位數：3 頁 165.0 → 158.4 ms，300 頁 165.8 → 156.2 ms。小檔首頁 < 200 ms **達成**。
- **Idle 不變。**
  - private working set +0.3 MB：3 頁 24.4 → 24.7 MB，300 頁 26.3 → 26.5 MB。Idle RAM < 50 MB **達成**。
  - FastPDF 的主執行緒每秒 0 次喚醒，idle 時 0 frame。idle CPU 的中位數 0.16–0.31% 單核，全部來自 GPU driver 的 thread，兩版相同。Idle CPU 接近 0 **達成**。
  - private bytes 反而少 5.6–5.7 MB，來自 app process 中一塊幾乎沒有被使用的 commit（見〈Before／After／Why／Tradeoff〉）。
- **捲動、PageDown 與縮放的第一個反應沒有可判定的差異**（配對差中位數 +1.5、−2.6、+4.0 ms，都小於一個截圖間隔，約 15 ms）。縮放「最後一次輸入到畫面穩定」的配對差中位數是 +18 ms，但範圍是 −125～+61 ms，兩版最快的幾次相同（39 與 40 ms）。不經 GUI 的縮放路徑配對量測完全相同（比值 0.997），frame 數也相同。目前判斷為負載造成的雜訊，建議在安靜的機器上重測（〈後續〉）。
- **記住位置：從第 150 頁開啟和從第 1 頁一樣快。**
  - 第一個非空白畫面就是第 150 頁，沒有先畫第 1 頁。
  - 4 對：第一頁完全清晰的中位數 176.8 ms（第 150 頁）與 174.8 ms（第 1 頁），配對差中位數 +2.1 ms；兩邊都沒有干擾的 2 對是 +3.6 與 +0.5 ms。
  - idle private working set 多 1.8 MB（28.4 對 26.6 MB），idle 時同樣 0 frame。
- **exe**：dist build 21.00 → 22.06 MiB（+1.06 MiB、+5.1%），仍低於 30 MB。

## 方法

### Build

- B-1 與 B-8 用 `cargo build --release`（thin LTO）。兩版各自 build 後複製到 target 以外，量測途中不會被重新 build 取代。
- exe 大小另外用 `cargo build --profile dist -p fastpdf-app --locked`（fat LTO、strip，README KPI 用的 profile）。
  - 舊版從 `git archive` 匯出的乾淨原始碼 build。
  - 兩版都沒有 `tools/package.ps1` 的路徑改寫，所以大小和發佈用的 exe 不完全相同（內嵌的路徑字串不同）。
- rustc 1.99.0（`rust-toolchain.toml`，兩版相同）。

### B-1（engine 層）

- 照 `tools/bench_paired.py` 的方法：每個檔案用兩個 `fastpdf-bench full`（in-process Hayro）交替執行（A B、B A、…），每次都是新的 process，取每對比值（新／舊）的中位數。判定門檻照 `docs/benchmarks/b1-paired.md`。
- 語料：`fixtures/generated/manifest.json` 的 99 個檔案（round 1 新增頁面標籤、連結、文件內容與加密的 fixture），每個檔案 5 對。
- 負載閘門：在工具的暫存副本裡加上等待，每個檔案開始前等系統忙碌度（3 次 1 秒取樣的平均）低於 30%，而且沒有 cargo／rustc 在跑。工具本身沒有修改，判定與輸出相同。
- 被判定的檔案，以及沒有被判定但比值超過 ±10% 的檔案，再用 15 對重測。

### B-8（app 層）

- `tools/bench-app/bench-app.ps1` 1.3.0，preset `fastpdf`，`-ThreadDetail`，每次啟動只量一次，idle 10 秒。
  - preset 把 `FASTPDF_SETTINGS_FILE`、`FASTPDF_RECENT_FILE` 設為空值：預設設定，不讀寫最近開啟清單與閱讀位置。
- 情境與之前的回合相同：
  - **3 頁**：`small-text/three-pages-platypus-times.pdf`，只量啟動與 idle；
  - **300 頁**：`large-text/dense-300p-times.pdf`，再加 20 格滾輪（50 ms 間隔）、5 次 PageDown、3 次 Ctrl+滾輪縮放，以及互動後的 idle 5 秒。
- 舊版與新版交替：奇數對先舊版，偶數對先新版。
- 每次啟動前等三個條件：鍵盤滑鼠閒置 20 秒以上、沒有 cargo／rustc、系統忙碌度（`Win32_Processor.LoadPercentage` 3 次取樣的平均）低於 30%。
- 干擾的判定（每次啟動）：
  - 啟動：`main` 晚於 process 建立 30 ms 以上，或 bench-app 在啟動前一刻自己取的系統負載（`cpu_load_pct_before`）達 60% 以上；
  - 互動：滾輪期間每秒變化的 frame 少於 40（平常 45–50）；
  - idle：主執行緒每秒喚醒超過 2 次，或 idle 時畫了 frame。這次沒有任何一次發生。
- 判讀：
  - 時間看每對的差（新 − 舊）的中位數，另外列出兩邊都沒有干擾的對；
  - 記憶體與 idle 指標不受負載影響，用全部的對；
  - 前 3 對有干擾，所以兩個情境都補量：3 頁共 8 對，300 頁共 9 對。
- 「視覺完成」是外部截圖量到的最後一次畫面變化，換算成從 process 建立起算（`launch.t_visual_complete_ms − process_created_offset_ms`）。
- 3 頁的第 4、5 對另外加 `-MemoryDetail`。它在 idle 量完之後才走訪位址空間，不影響前面的數字。
- 記住位置：
  - 只用新版、300 頁，只量啟動與 idle，第 150 頁與第 1 頁交替；
  - `FASTPDF_RECENT_FILE` 指到量測用的暫存資料夾。啟動前在旁邊的 `recent-positions.txt` 寫好同一個檔案（大小、修改時間、頁數都相符）在該頁的位置（`fit-width`）；
  - 結束後讀回檔案，確認寫回的頁碼；
  - 保存第一個非空白畫面與最後畫面的截圖。

### 不經 GUI 的縮放路徑（輔助）

- `fastpdf-bench scroll`（B-5 的腳本：session、scheduler、in-process Hayro）。300 頁從頭捲到尾，3 次縮放到 400% 再回來。
- 舊版與新版交替 5 對，閘門同 B-1。
- 比較整個腳本的時間，以及 zoom in 各步驟等到可見 tile 全部清晰的時間。

### 機器與負載

- AMD Ryzen 9 9950X（16C／32T）、125 GB RAM、RTX 5090 與內顯、1920×1080 @ 60 Hz、Windows 11 Pro 10.0.26200。
- 量測機上同時有其他長時間執行的工作，系統忙碌度在 10–100% 之間跳動。
- B-1：
  - 每個檔案開始前的忙碌度中位數 13.9%（6.9–29.5%）；
  - 為了等安靜的空檔，總共等了 31 分鐘。
- B-8：
  - 第一次嘗試等了 60 分鐘都沒有達到閘門，沒有啟動任何一次；
  - 之後的 42 次啟動，每次等待的中位數 2.2 分鐘、最長 36 分鐘，共約 4.7 小時；
  - 閘門通過時的忙碌度 8.7–29.3%，但 bench-app 在啟動前一刻自己取的負載是 9–99%（中位數 37.5%）：閘門通過後的幾秒內，負載仍會跳高；
  - 時間指標因此雜訊很大。記憶體、thread 數與 idle 時 FastPDF 自己的活動不受影響。

### 原始資料

都在量測機器上的 `benchmarks/runs/reading-round1/`（git-ignored，不 commit）：

- `b1-paired.json`、`b1-summary.txt`、`b1-progress.log`：B-1 全語料。每個檔案的負載與等待時間記在 `load_before`、`waited_s`；
- `b1-flagged-15.json`：15 對重測；
- `b8/*.json`：B-8 每次啟動一個檔案（bench-app 的格式），檔名是 `<情境>-<版本>-<對>.json` 與 `pos-p<頁>-<對>.json`。`b8/shots/` 是記住位置那幾次的截圖；
- `scroll/`：不經 GUI 的縮放路徑。

## 結果

### B-1：全語料（99 個檔案，各 5 對）

- 狀態：92 個 ok、4 個 partial、1 個 load_error、2 個 open_error，**新舊完全相同**（0 個狀態改變）。

| 指標 | 檔案數（兩版都 ok） | 幾何平均（新／舊） | 退步（判定） | 改善（判定） |
|---|---:|---:|---:|---:|
| `open_ms` | 92 | 0.965 | 0 | 0 |
| `first_page_ms` | 92 | 0.998 | 1 | 0 |
| `time_to_first_page_ms` | 92 | 0.970 | 1 | 4 |
| `text_ms` | 92 | 1.043 | 0 | 0 |
| `rss_peak_mb` | 92 | 1.001 | 0 | 0 |
| `private_peak_mb` | 92 | 1.000 | 0 | 0 |
| `cpu_ms` | 89 | 0.942 | 0 | 0 |

15 對重測（8 個檔案：被判定的 5 個，加上 3 個沒被判定但比值超過 ±10% 的）：

| 檔案 | 指標 | 5 對（舊 → 新） | 15 對（舊 → 新） | 判讀 |
|---|---|---|---|---|
| `japanese/embedded-type0.pdf` | `first_page_ms` | 8.11 → 9.49 ms（x1.17） | 4.62 → 4.71 ms（x1.025） | 雜訊：5 對時負載 27.9%，時間是安靜時的將近兩倍 |
| `malformed/info-hostile-strings.pdf` | `time_to_first_page_ms` | 10.22 → 7.75 ms（x0.76） | 5.69 → 4.35 ms（x0.78） | **改善**，見下 |
| `malformed/info-missing.pdf` | `time_to_first_page_ms` | 5.80 → 2.77 ms | 2.15 → 2.16 ms | 雜訊 |
| `small-text/one-page-helvetica.pdf` | `time_to_first_page_ms` | 7.91 → 5.54 ms | 5.60 → 5.79 ms（x1.026） | 雜訊 |
| `vector-heavy/fills-gradients-strokes.pdf` | `time_to_first_page_ms` | 40.08 → 33.55 ms | 33.54 → 33.44 ms | 雜訊 |
| `malformed/xref-prev-cycle.pdf` | `time_to_first_page_ms` | 4.81 → 4.24 ms（差 0.57 ms，低於門檻） | 2.10 → 2.13 ms | 雜訊 |
| `malformed/broken-embedded-fonts.pdf` | `time_to_first_page_ms` | 2.71 → 2.41 ms（低於門檻） | 2.91 → 2.90 ms | 雜訊 |
| `malformed/decompression-bomb-512mib.pdf`（partial） | `time_to_first_page_ms` | 4.30 → 6.04 ms（x1.33） | 2.88 → 2.74 ms | 雜訊 |

- 15 對的重測沒有任何判定的退步。7 個兩版都 ok 的檔案，`first_page_ms` 幾何平均 1.012、`time_to_first_page_ms` 0.975。
- **`info-hostile-strings.pdf` 的改善**：
  - `time_to_first_page_ms` 包含讀取文件資訊（`metadata()`），這個 fixture 的 Info 字串非常長；
  - round 1 的文件內容功能把文件字串的解碼集中到 `crates/fastpdf-engine-hayro/src/text_string.rs`，每個字串最多解碼 16 KiB；舊版會解碼整個字串；
  - 另外單獨跑 3 對：`metadata_ms` 1.37–2.40 ms → 0.08–0.11 ms；15 對的 `private_peak_mb` 11.1 → 10.1 MB。

### B-8：3 頁（8 對）

| 指標 | 舊版 | 新版 | 配對差 新 − 舊（中位數，範圍） | 無干擾的對（3 對） |
|---|---:|---:|---:|---:|
| `window_visible`（ms） | 188.4 | 182.9 | +0.0（−244.4～+44.3） | +3.7（−3.6～+30.3） |
| `first_page_exact`（ms） | 192.5 | 185.7 | −3.3（−245.4～+48.0） | −1.0（−5.7～+26.5） |
| 視覺完成（外部截圖，ms） | 216.2 | 206.2 | +4.4（−250.3～+39.2） | +14.2（−5.4～+33.7） |
| `first_page_exact`，啟動前負載 ≤ 20%（ms） | 165.0（2 次） | 158.4（3 次） | — | — |
| idle private working set（MB） | 24.4 | 24.7 | +0.3（+0.2～+0.4） | |
| idle private bytes（MB） | 109.0 | 103.3 | −5.6（−5.8～−5.5） | |
| idle CPU（% 單核，全部是 GPU driver 的 thread） | 0.31 | 0.23 | 0.00（−1.41～+0.47） | |
| idle 主執行緒喚醒（次／秒） | 0 | 0 | 0 | |
| FastPDF 在 idle 時畫的 frame | 0 | 0 | 0 | |
| thread 數（idle 結束時） | 89 | 89 | 0 | |
| peak private bytes（MB） | 117.3 | 111.6 | −5.7（−5.8～−5.6） | |

- 時間的大範圍來自啟動前一刻的負載：例如舊版第 1 對（負載 99%）是 410 ms，新版第 7 對（`main` 晚到 42 ms）是 221 ms。
- 兩邊負載都低的第 8 對（16% 與 9%）：163.5 與 157.9 ms。

### B-8：300 頁（9 對）

| 指標 | 舊版 | 新版 | 配對差 新 − 舊（中位數，範圍） | 無干擾的對 |
|---|---:|---:|---:|---:|
| `window_visible`（ms） | 171.8 | 163.4 | +3.4（−82.6～+88.5） | +3.4（−45.2～+11.0，5 對） |
| `first_page_exact`（ms） | 173.7 | 168.2 | +6.7（−82.9～+86.8） | +6.7（−44.0～+9.6，5 對） |
| 視覺完成（外部截圖，ms） | 203.3 | 198.3 | −23.7（−54.6～+76.2） | −23.7（−31.8～+15.7，5 對） |
| `first_page_exact`，啟動前負載 ≤ 20%（ms） | 165.8（2 次） | 156.2（1 次） | — | — |
| idle private working set（MB） | 26.3 | 26.5 | +0.3（+0.1～+0.5） | |
| idle private bytes（MB） | 111.0 | 105.2 | −5.7（−5.9～−5.6） | |
| idle CPU（% 單核，全部是 GPU driver 的 thread） | 0.16 | 0.16 | −0.15（−0.47～+0.31） | |
| idle 主執行緒喚醒（次／秒） | 0 | 0 | 0 | |
| FastPDF 在 idle 時畫的 frame（含互動後） | 0 | 0 | 0 | |
| 滾輪：第一個畫面變化（ms） | 24.8 | 25.5 | +1.5（−22.3～+41.7） | +8.7（+1.5～+15.8，2 對） |
| 滾輪：最後輸入後到穩定（ms） | 156.0 | 146.0 | +1.0（−63.0～+27.0） | −7.5（−29.0～+14.0，2 對） |
| PageDown：第一個畫面變化（ms） | 24.6 | 25.1 | −2.6（−19.4～+20.0） | −1.3（−4.1～+1.5，2 對） |
| 縮放：第一個畫面變化（ms） | 27.7 | 28.5 | +4.0（−7.5～+14.7） | +3.4（+2.7～+4.0，2 對） |
| 縮放：最後輸入後到穩定（ms） | 59.0 | 61.0 | +18.0（−125.0～+61.0） | +41.5（+22.0～+61.0，2 對） |
| peak private bytes（MB，含互動） | 274.8 | 277.4 | +2.9（−5.0～+25.2） | |

- 互動受干擾的次數很多：9 對中只有 2 對兩邊的滾輪都有每秒 40 個以上的畫面變化。這些互動指標只當參考。
- **縮放「最後輸入後到穩定」**：
  - 各對的差：−5、+22、+49、+61、+18、−33、−20、+48、−125 ms；
  - 兩版最快的幾次相同：舊版 39、39、40、41 ms，新版 40、40、45、54 ms；
  - 每次縮放 FastPDF 畫的 frame 數相同（7–12 個，兩版的中位數都是 10）；
  - 不經 GUI 的縮放路徑相同（見下）。
  - round 1 沒有修改 render、scheduler 與 render host 的 render 路徑。每個 frame 新增的工作是工具列的寬度檢查與連結 hover 的重新判斷（滑鼠沒有移到文件上時不做事），都只是簡單的比較。
  - 判斷為負載造成的雜訊，但這次的資料無法排除約一個 frame 的差距，列入〈後續〉在安靜的機器上重測。

### 不經 GUI 的縮放路徑（5 對）

- `fastpdf-bench scroll`，300 頁。
- 整個腳本：3.50 → 3.50 s，配對比值中位數 1.000（0.673～1.111）。
- zoom in 各步驟：0.741 → 0.734 s，比值 0.997（0.658～1.063）。
- 兩版都沒有 unsettled 的步驟。

### 記住位置（新版，300 頁）

| 指標 | 第 1 頁 | 第 150 頁 | 配對差 150 − 1（中位數，範圍） |
|---|---:|---:|---:|
| `window_visible`（ms） | 167.3 | 171.0 | −1.3（−126.2～+104.2） |
| `first_page_exact`（ms） | 174.8 | 176.8 | +2.1（−124.5～+98.8）；無干擾的 2 對 +3.6、+0.5 |
| 視覺完成（外部截圖，ms） | 207.6 | 191.5 | −21.4（−142.0～+97.7） |
| idle private working set（MB） | 26.6 | 28.4 | +1.8 |
| idle private bytes（MB） | 105.2 | 107.1 | +1.9 |
| peak private bytes（MB） | 115.5 | 120.8 | +5.3 |
| FastPDF 在 idle 時畫的 frame | 0 | 0 | 0 |

- 4 對，順序交替。第 1 對兩邊都受到干擾（第 150 頁啟動前負載 68%，第 1 頁的視窗晚到 301 ms），第 2 對第 150 頁的 `main` 晚到 39 ms；後 2 對兩邊都沒有干擾。
- **第一個畫面就是第 150 頁**：8 次啟動的「第一個非空白畫面」與「最後畫面」的截圖逐位元組相同。第 150 頁的 4 次在工具列顯示「150 / 300」、頁面顯示該頁的標題，沒有先畫第 1 頁；第 1 頁的 4 次顯示「1 / 300」。
- **寫回**：每次結束後讀回位置檔，寫回的頁碼、頁內位置、縮放與事先寫入的相同（第 150 頁或第 1 頁、`0`、`fit-width`）。
- **記憶體**：從第 150 頁開啟時，idle private working set 多 1.8 MB、peak 多 5.3 MB。來源沒有拆解；仍遠低於 50 MB 的 KPI。
- **idle**：第 150 頁第 1 次在 idle 時主執行緒每秒被喚醒 0.89 次（沒有畫 frame），其他 7 次都是 0，沒有重現。

### exe 大小

| build | 舊版 `90a66d8` | 新版 `d28b5e6` | 差 |
|---|---:|---:|---:|
| release（B-1、B-8 用） | 25,979,392 B（24.78 MiB） | 27,278,336 B（26.01 MiB） | +1,298,944 B（+5.0%） |
| dist（fat LTO、strip） | 22,021,120 B（21.00 MiB） | 23,135,232 B（22.06 MiB） | +1,114,112 B（+5.1%） |

- dist 的 22.06 MiB（23.1 MB）仍低於 spec §29 的 30 MB。
- 對照：
  - 0.0.2 發佈的 exe（寫入功能之前，`tools/package.ps1`）是 17,415,680 B（16.61 MiB）；
  - README 原本寫的 16.1 MiB 是第九輪（2026-10-05）的 dist build；
  - v0.0.2 到 `90a66d8`（寫入功能與捲軸／頁面配置，ADR 0013）增加約 4.4 MiB，round 1 再增加 1.06 MiB。

## Before／After／Why／Tradeoff

### 開檔與第一頁

- **Before**：3 頁 192.5 ms、300 頁 173.7 ms（8／9 對的中位數）；啟動前負載 ≤ 20% 時 165.0、165.8 ms。
- **After**：185.7、168.2 ms；負載 ≤ 20% 時 158.4、156.2 ms。配對差的中位數 −3.3 與 +6.7 ms，範圍兩個方向都有。
- **Why**：round 1 的功能都不在開檔與第一頁的路徑上：
  - 連結在滑鼠第一次移到某一頁時才讀；
  - 頁面標籤在第一頁完全清晰之後才問；
  - 文件內容在開面板時才問；
  - 密碼提示只有需要密碼的文件才建立；
  - 上一頁／下一頁只在跳頁時記錄（最多 100 個檢視）。
  - 記住位置開啟時，開檔執行緒多取一次檔案大小與修改時間，位置檔由另一條執行緒在載入的同時讀取（`docs/DEVELOPMENT.md`〈本機狀態檔〉）。bench-app 關閉了這個功能，所以上面的數字不含它；它的成本另外量在〈記住位置〉。
- **Tradeoff**：用密碼開啟時，每次嘗試多啟動一次 render host（ADR 0014）。未加密的文件只多一次 `DocumentSource` 的 Arc clone。

### Idle

- **Before／After**：
  - private working set：24.4 → 24.7 MB（3 頁）、26.3 → 26.5 MB（300 頁）；
  - 主執行緒每秒 0 次喚醒，idle 時 0 frame，89 個 thread，兩版相同。
- **Why**：
  - round 1 沒有任何計時器在 idle 時執行。閱讀位置的計時器只在畫面移動後啟動一次（2 秒），bench-app 的情境裡沒有啟動。
  - +0.3 MB 沒有再拆解，推測是新增的狀態與工具列元素（←、→、ⓘ、「⋯」）。
- **private bytes 少 5.6–5.7 MB**：
  - `-MemoryDetail`（3 頁第 4、5 對）顯示差異全部在 app process 的 `other_private`（63.3 → 57.5 MB）；
  - 舊版有一塊 20.00 MB 的配置，新版是 14.01 MB，兩者都 commit 了但幾乎沒有被使用（private working set 0.01 MB）；
  - 來源沒有追查：可能是 GPU driver／D3D runtime 的 buffer，也可能是預留了容量但沒有寫入的配置。它不影響 private working set（Idle RAM 的 KPI）。
- **Tradeoff**：無。

### 記憶體上限

- 新的 cache 與清單都有上限：
  - `links` cache 2 MiB（登記在 `MemoryBudgetManager`）；
  - 上一頁／下一頁最多 100 個檢視；
  - 搜尋結果最多 20,000 筆，清單只建立看得到的列；
  - 閱讀位置最多 200 份文件、512 KiB；
  - 頁面標籤最多 4096 個範圍；
  - 文件字串解碼 16 KiB、顯示 2,000 個字元。
- 300 頁互動後的 peak private bytes 274.8 → 277.4 MB（+2.9，範圍 −5.0～+25.2），在 tile cache 預算內隨互動時機變動。

### exe

- **Before／After**：dist 21.00 → 22.06 MiB（+1.06 MiB），release 24.78 → 26.01 MiB。
- **Why**：新的程式碼。依賴樹只多了 `url`，它原本就在 GPUI 的依賴裡（ADR 0015）。
- **Tradeoff**：exe 變大不影響啟動（見上）；距離 30 MB 還有約 7 MB。

## 後續

1. **安靜機器重測縮放與 3 頁啟動**：照 `docs/RELEASE.md` §1.4，在沒有其他負載的機器上重跑 300 頁的互動（特別是縮放「最後輸入後到穩定」）與 3 頁的啟動，各 6 對以上，確認沒有約一個 frame 的差距。
2. **private bytes 的 6 MB**：要知道那塊 20 MB → 14 MB 的配置屬於誰，需要在 app 內記錄 GPU buffer 的建立（或用 ETW 的 VirtualAlloc stack）。這不影響 KPI，優先順序低。
3. **B-1 的負載閘門**：這次在工具的暫存副本裡加了每個檔案的負載等待。若之後都要在有背景負載的機器上量，可以考慮把 `--load-limit` 做進 `tools/bench_paired.py`。
