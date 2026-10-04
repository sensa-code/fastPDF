# Render host（ADR 0008）：吞吐量、每個 tile 的 CPU 與 B-8 驗收

PR 4 第二輪與第三輪的量測紀錄。第二輪確認 render host 的吞吐量與啟動時間追上 in-process，再決定 Windows 的預設 engine；第三輪把每個 tile 的 CPU 降回 in-process 的水準，讓預設 isolated 不增加筆電的耗電。設計與決定見 [ADR 0008](../adr/0008-out-of-process-rendering.md)。

## 結論（第三輪）

- **每個 tile 的 CPU 回到 in-process 的水準。**
  - 改法：tile render 改走 slot channel（不經 pipe，每個 tile 只喚醒 2 條 thread）；像素以 RGBA 傳，parent 從 slot 複製時順便換成 BGRA；取消 pipelining。
  - 結果：4 個 fixture 的 CPU／tile（parent＋host）與 in-process 相差的中位數，新內容 −2.0% 至 +2.5%，cache 命中 −2.2% 至 +6.5%。修改前是 +8.3% 至 +28.4% 與 +9.7% 至 +56.4%。
- **吞吐量與 in-process 相同。** 新內容 −2.3% 至 +0.7%，cache 命中 −3.8% 至 +0.5%。
  - 第二輪的 pipelining 讓 cache 命中比 in-process 快（本輪量到 +0.3% 至 +14.9%）；這一輪拿掉它，換回 2–22 個百分點的 CPU（見〈第三輪〉的〈取捨〉）。
- **B-8 抽查**（3 頁，4 對）：`window_visible` 配對差中位數 +2.95 ms；`first_page_exact` +8.45 ms，超過 5 ms。
  - 差距看起來來自 GUI 啟動與畫面更新的時機，而不是 render：B 有 2 對的整個啟動就晚了 6–11 ms；第一頁畫上去的時間只會落在第一個 frame 或晚一個 frame（0 或 6–15 ms）。
  - 不開 UI 量第一頁（開檔加第一頁所有 tile，各 18 對）：remote 比 in-process 多 1.16–1.34 ms，有沒有 pipelining 都一樣。
- **idle 不變**：兩個 host 的 FastPDF thread 都是 0 cycles、0 次喚醒。
- **正確性**：84 個 fixture 的 in-process／remote 比對 0 差異。

## 結論（第二輪）

- **吞吐量追上 in-process。**
  - 改法：每個同時進行的 render 在 host 多排一個請求（pipelining）。
  - 結果：4 個 fixture 的新內容（cold）中位數都在 −2.4% 至 +5.3% 之間，cache 命中在 −2.0% 至 +18.0% 之間。修改前 cold 落後 1.4–7.3%，cache 命中落後 2.5–23.9%。
- **B-8 啟動時間沒有差別。**
  - 3 頁與 300 頁情境各 6 對，有負載閘門。
  - `window_visible` 配對差的中位數：−0.43 ms 與 +3.07 ms；`first_page_exact`：−3.04 ms 與 −3.49 ms。
- **idle 不變，記憶體多 4–7 MiB。**
  - idle 時兩個 render host 的所有 thread 都是 0 cycles、0 次喚醒。
  - private working set 多 3.7 MiB，private bytes 多 6.4 MiB（多了文件 host 與待命 host 兩個 process）。
- **正確性**：
  - 84 個 fixture 的 in-process／remote 比對 0 差異。這次比對抓到並修正一個空檔案的問題（見〈正確性〉）。
  - hostile 語料經由 UI 跑完，UI process 0 次結束。
- **代價**：每個 tile 的總 CPU cycles（parent＋host）比 in-process 多。
  - 新內容多 8–31%，cache 命中多 13–59%。
  - 來源：一次 IPC 往返（約 110–130 kcycles）、一次 1 MiB 複製，以及同時執行的工作變多後的快取與記憶體頻寬競爭。第三輪已處理，見〈第三輪〉。

## 吞吐量（第二輪）

### 方法

- **量測程式**：scratch 量測程式（不在 repo 內），經由 FastPDF 自己的 `RenderScheduler` render，設定與 session 相同：2 個 worker、BGRA、508 px tile 加 2 px gutter。
- **sink**：做 `to_render_image` 會做的事（逐像素檢查 alpha），並保留 tile 到該輪結束，讓每個 tile 都拿到新的記憶體。
- **兩邊都從乾淨的 process 開始**：
  - A（in-process）在新的子 process 中執行；
  - B（remote）使用新的 render host，parent 只開 handle。
- **每次執行**：
  1. 開檔；
  2. render 第 1 頁，不計時（字型等 per-process cache 在這裡載入）；
  3. cold：第 2–9 頁的所有 tile，scale 2；
  4. cache 命中：同一批 tile 再 render 9 次，取中位數。
- **配對**：7 對 AB 交替。每對開始前先取樣系統負載，負載低於 30% 才開始（量到的負載 4–14%），量測期間沒有編譯。
- **CPU**：每個 tile 的 CPU 以 `QueryProcessCycleTime` 量 parent 加 document host 的 cycles。

### 結果

B 相對 A 的差距，7 對的中位數〔範圍〕。吞吐量的正值表示 remote 較快；CPU 的正值表示 remote 每個 tile 用較多 cycles。修改前是 HEAD `a199559`，修改後是本輪。

| 檔案 | 修改前 cold | 修改前 cache 命中 | 修改後 cold | 修改後 cache 命中 | CPU／tile 修改前（cold／cache） | CPU／tile 修改後（cold／cache） |
|---|---|---|---|---|---|---|
| three-pages（簡單頁） | −7.3%〔−16.6, −1.5〕 | −23.9%〔−29.9, −20.7〕 | −2.4%〔−7.7, +4.0〕 | +9.0%〔−8.2, +12.7〕 | +12.6%／+26.8% | +30.6%／+59.1% |
| dense-300p（密集文字） | −1.4%〔−2.0, +1.4〕 | −2.5%〔−4.3, −1.1〕 | −1.4%〔−23.1, +3.7〕 | −2.0%〔−18.1, +0.5〕 | +3.5%／+5.4% | +13.7%／+13.2% |
| photos（JPEG 影像） | −2.0%〔−4.2, −1.2〕 | −20.1%〔−26.4, −18.8〕 | −0.8%〔−2.9, +1.4〕 | +18.0%〔+11.5, +20.5〕 | +4.1%／+21.5% | +8.5%／+48.0% |
| gov-letter（非內嵌 CJK） | −6.5%〔−20.4, +0.5〕 | −20.7%〔−28.0, −5.7〕 | +5.3%〔−5.5, +18.5〕 | +12.8%〔+0.4, +34.2〕 | +12.1%／+16.2% | +20.2%／+51.7% |

- **cache 命中時 remote 反而較快**：
  - in-process 的 worker 要依序做配置、render、轉換；
  - remote 時，host 的 render thread 只負責 render，而且寫進常駐的 slot，沒有 page fault；
  - 複製與轉換在 parent 的另外兩條 thread 上同時進行。
  - 兩邊同時 render 的頁數一樣都是 2。

### 每個請求的成本

單一 thread 依序呼叫，2000 次的中位數，dense-300p 第 1 頁，scale 2：

| 呼叫 | in-process | remote |
|---|---|---|
| `metadata`（只有 IPC） | 0.7 µs（3 kcycles） | 24.7–26.1 µs（107–115 kcycles） |
| 1×1 px render | 0.2 µs | 19.9–22.8 µs（126–133 kcycles） |
| cache 命中的 512 px tile | 162–166 µs（731–744 kcycles） | 216–228 µs（953–1012 kcycles） |

### 剩下的成本在哪裡

- **IPC 往返（約 20–26 µs、110–130 kcycles）**：
  - 每個請求要喚醒 4 條 thread：host 讀命令的 thread、host 的 render thread、parent 讀回覆的 thread、等待中的 render worker。
  - pipelining 把這段時間藏在 host 的下一個 render 後面，吞吐量不再受影響，但 CPU 仍然要付。
- **1 MiB 複製（約 30–50 µs）**：host 寫進共享的 slot，parent 再複製進 tile buffer。這一次複製省不掉：GPUI 的 `RenderImage` 只接受 process 自己 heap 上的 `Vec`，而 tile cache 會長期持有 tile，不能借用 slot。
- **pipelining 本身**：同時執行的工作變多（host 2 條 render thread，加上 parent 2 條複製與轉換的 thread），快取與記憶體頻寬的競爭讓每個 tile 的 cycles 增加。修改前後 CPU／tile 差距的增加主要來自這裡。
- **要再降低 CPU，有兩個做法，本輪都沒有做**：
  - 改傳輸方式：每個 slot 一個完成 event，host 直接喚醒等待的 worker，或讓 host 的 worker 自己讀命令（leader/follower）。每個請求可以少 1–2 次喚醒，估計少 30–50 kcycles，約 cache 命中 tile 的 3–5%。
  - 少一次複製：需要 GPUI 接受外部記憶體當作 image 的來源。

## B-8（第二輪）

### 方法

- **工具**：`tools/bench-app/bench-app.ps1` 1.1.0，preset `fastpdf`。
  - 參數：`-Runs 1 -ThreadDetail -NoScreenshots`；
  - dist build（fat LTO，16,881,664 B）；
  - `FASTPDF_SETTINGS_FILE`、`FASTPDF_RECENT_FILE` 為空值。
- **配對**：A＝`--engine hayro`，B＝`--engine hayro-isolated`。每對 A 先 B 後；3 頁與 300 頁交錯，各 6 對，共 24 次啟動。
- **負載閘門**：
  - 每次啟動前以 `GetSystemTimes` 取樣 1 s 的系統忙碌度，並確認沒有 cargo、rustc、link。
  - 忙碌度低於 30% 才啟動，否則每 10 s 重試，最多 5 分鐘。
  - 實際啟動時的負載是 4.1–15.3%，24 次都沒有等到上限。
- **判定**：配對差（B − A）的中位數。

### 結果

| 情境 | 指標 | A 中位數〔範圍〕 | B 中位數〔範圍〕 | 配對差中位數〔範圍〕 |
|---|---|---|---|---|
| 3 頁 | `window_visible` | 199.42 ms〔194.64, 201.40〕 | 196.95 ms〔190.88, 206.79〕 | −0.43 ms〔−4.43, +6.76〕 |
| 3 頁 | `first_page_exact` | 201.68 ms〔194.88, 211.13〕 | 201.65 ms〔196.41, 211.67〕 | −3.04 ms〔−10.20, +16.79〕 |
| 300 頁 | `window_visible` | 191.19 ms〔181.86, 203.03〕 | 195.05 ms〔183.67, 199.06〕 | +3.07 ms〔−19.37, +12.18〕 |
| 300 頁 | `first_page_exact` | 197.99 ms〔182.52, 214.11〕 | 196.05 ms〔184.02, 199.50〕 | −3.49 ms〔−27.92, +13.45〕 |

24 次啟動中，`document_opened` 都早於 `window_visible`：開檔與第一頁 render 都在 GPUI 啟動期間完成。

### Idle CPU（`-ThreadDetail`，idle 10 秒）

| 情境 | 指標 | A | B | 配對差中位數 |
|---|---|---|---|---|
| 3 頁 | 整個 tree 的 Mcycles／s | 23.86〔18.24, 71.84〕 | 19.85〔17.67, 22.70〕 | −4.15 |
| 3 頁 | 整個 tree 的喚醒次數／s | 215.81〔192.67, 315.45〕 | 195.09〔191.28, 196.87〕 | −21.73 |
| 3 頁 | CPU time（% 單核） | 0.31〔0.31, 1.41〕 | 0.62〔0.00, 1.25〕 | +0.00 |
| 300 頁 | 整個 tree 的 Mcycles／s | 20.27〔18.23, 22.75〕 | 20.90〔18.14, 45.53〕 | +1.27 |
| 300 頁 | 整個 tree 的喚醒次數／s | 195.69〔192.21, 204.06〕 | 197.95〔196.30, 310.10〕 | +4.08 |
| 300 頁 | CPU time（% 單核） | 0.54〔0.16, 1.41〕 | 1.09〔0.00, 1.56〕 | +0.70 |

- **兩種模式的 idle CPU 都在同兩條 thread 上**：主執行緒約 70 次／s、`VSyncProvider` 約 62 次／s，都是 GPUI 的 vsync 迴圈；另外還有 NVIDIA driver 的 thread。
- **render host 本身沒有 idle 成本**：
  - B 的兩個 host process（文件 host 與待命 host）的所有 thread（`fastpdf-render-host-*`）在 idle 窗口內都是 0 cycles、0 次喚醒；
  - parent 的 reply reader（`fastpdf-remote-*`）與多出來的 scheduler thread 也是。
- **300 頁的 CPU time 差 +0.70%**：這是 15.6 ms 計時粒度在 GPUI 兩條 thread 上的雜訊，各次執行在 0–156 ms 之間跳動。
  - 精確的 cycles 與喚醒次數中位數只差 +1.27 Mcycles／s 與 +4.08 次／s，最大值來自 1 次 idle 期間系統忙碌度 84.7% 的執行。
  - 3 頁情境則是 B 比較低。

### 記憶體（idle 時整個 process tree 的總和）

| 情境 | 指標 | A | B | 配對差中位數 |
|---|---|---|---|---|
| 3 頁 | private working set（KPI） | 24.65 MB | 28.30 MB | +3.70 MB |
| 3 頁 | private bytes | 107.15 MB | 113.55 MB | +6.40 MB〔+5.70, +7.40〕 |
| 300 頁 | private working set | 26.45 MB | 30.20 MB | +3.75 MB |
| 300 頁 | private bytes | 109.15 MB | 115.60 MB | +6.45 MB〔+6.20, +6.90〕 |

B 有 3 個 process（app、文件 host、待命 host）。

## 正確性（第二輪）

- **84 個 fixture**：每個 fixture 的第一、中間、最後一頁，比較以下項目：
  - page geometry；
  - 整頁 render（長邊最多 1600 px）；
  - scale 2 的一個 512 px tile；
  - text layer。
  - **結果**：
    - 81 個檔案兩邊都開得起來，3 個檔案的開檔錯誤相同。
    - 192 頁中，372 個 render 逐位元組相同；2 頁的 geometry 錯誤與 8 個 render 錯誤兩邊相同；190 個 text layer 相同。
    - 0 個差異，host 0 次 crash。
- **修正**：空檔案（`malformed/empty-file.pdf`，0 bytes）在只開 handle 的模式下讓 host 以 protocol 錯誤結束（結束碼 4），in-process 則回報 `Malformed`。
  - 原因：長度 0 的檔案被當成 section 傳給 host，host 的 decoder 拒絕。
  - 修正：長度 0 的檔案改當成空的來源。
  - app 的 loader 本來就會先拒絕空檔，所以 UI 不受影響。
- **hostile 語料經由 UI**：46 個檔案，783 步，預設 engine（isolated）並以 `FASTPDF_HOST_MEMORY_MB=48` 執行。
  - host 因記憶體上限結束 6 次；`deep-nesting-content-100000` 與 `form_dag_depth20` 各 3 次後停止重啟。
  - 腳本跑完，結束碼 0，0 次 panic。
- **測試**：取消、crash 歸責、deadline、slot 回收的既有整合測試全部通過。新增的測試：
  - render 超過 host 同時 render 數時在 host 排隊；
  - 排隊中的 render 不會被誤判為卡住；
  - 有 render queue 的文件，scheduler 為每個在途請求開一條 thread；
  - 空檔案。

## 第三輪：每個 tile 的 CPU

### 問題與目標

- 第二輪之後，每個 tile 的 CPU cycles（parent＋host）比 in-process 多：新內容 8–31%，cache 命中 13–59%。預設改成 isolated 後，筆電 render 時的耗電會跟著增加。
- 目標：新內容與 cache 命中都在 in-process 的 +10% 內，同時維持吞吐量與 B-8（配對差 ≤ 5 ms）。

### 成本在哪裡

用每個 thread 的 cycles（`QueryThreadCycleTime`）、render 呼叫內外的分段，以及單獨的微量測拆開（scratch 量測程式，three-pages，cache 命中）：

- **IPC**：每個 tile 喚醒 4 條 thread（host 讀命令的 thread、host 的 render thread、parent 的 reply reader、等待中的 worker），兩端各一次 pipe 寫入與讀取。修改前 host 主執行緒每個 tile 24–47 kcycles，parent 的 reply reader 33–77 kcycles。
- **host 裡的 BGRA 轉換（最大的一項）**：
  - hayro 的 block cache 存 RGBA，`copy_from_rgba` 逐像素換位寫進 BGRA 的 target。來源 block 不在 CPU 快取裡（實際情況），每 MiB 約 570–680 kcycles；同樣的資料以 RGBA 逐列複製只要 190–290 kcycles。
  - in-process 時，這個轉換和新記憶體的 page fault 在同一趟完成（寫進新 buffer 共 910–1010 kcycles）；remote 時，host 先轉換寫進 slot，parent 再複製一趟（slot 到新 buffer 520–560 kcycles）。
- **pipelining**：同時忙碌的 thread 變多（host 2 條 render、parent 最多 2 條複製），每個 tile 的 cycles 多 2–22 個百分點（簡單頁最多）。in-process 也一樣：worker 從 1 條加到 4 條，每個 tile 多 14–18%。同時跑的工作越多，記憶體頻寬與快取的競爭越大，同樣的工作就要更多 cycles。
- **1 MiB 複製本身**：parent 從 slot 複製進 tile buffer 約 60 kcycles（不含 page fault）。這一次複製省不掉：GPUI 的 `RenderImage` 只接受 process 自己的 `Vec`。

### 修改

1. **slot channel**（`win/channel.rs`、`win/sync.rs`）：tile render 不經 pipe。
   - 每個 slot 有一個 4 KiB 控制區塊，放在第二個共享 section。
   - parent 把編碼好的 `Render` 寫進區塊、標成 REQUESTED（在 table 的鎖內，順序與註冊順序相同），再 release 一個 semaphore。
   - host 的 render thread 等這個 semaphore，以 CAS 取 order 最小的區塊，render 後把 `Done` 寫回區塊，再 set 該 slot 的完成 event。
   - 等待中的 worker 直接等自己 slot 的 event（加上「host 已結束」的 event）。
   - 結果：每個 tile 只喚醒 2 條 thread，沒有 `ReadFile`／`WriteFile`。
   - 不變的部分：取消仍送 `Cancel`（另外在區塊設 cancel flag，給還沒被取走的請求）；deadline、crash 歸責照舊；被放棄的 slot 等 host 做完後由 reader 的 tick 回收。
   - 權限：semaphore 只 duplicate `SYNCHRONIZE`，event 只 duplicate `EVENT_MODIFY_STATE`。parent 不相信區塊的內容：檢查狀態、id、長度之後才以 protocol decoder 解碼。
   - 大於 slot 的 render 仍走命令，由同一組 render thread 處理（以 host 自己的 semaphore 喚醒），所以 host 同時 render 的數量仍是 `render_threads`。
2. **RGBA 傳輸**（`remote.rs`、`win/section.rs`）：
   - parent 一律向 host 要 RGBA（engine 自己的順序），host 的 copy out 變成逐列複製。
   - parent 從 slot 複製時順便換成 BGRA（u32 的遮罩與位移，會向量化）。
   - 結果與 in-process 逐位元組相同。
3. **取消 pipelining**：`render_queue_depth` 回到預設的 1，每個 worker 只有一個請求在途。

### 方法

- **量測程式**：與第二輪相同（scratch，經由 `RenderScheduler`，2 個 worker、BGRA、508 px tile 加 2 px gutter；A 在新的子 process，B 在新的 host）。
- **配對**：每輪各版本執行一次（每個檔案 1 對 AB），版本的順序每輪輪替、AB 的先後每輪交替，共 7 輪，所以每個版本都有 7 對。
- **負載閘門**：每次執行前等系統忙碌度 < 20%（最多 5 分鐘），量測程式在每對之前再確認 < 30%。每對開始前的負載最高 16–25%。
- **版本**：修改前是 HEAD `3f4d104`（pipe、BGRA、pipelining）；修改後是本輪。拆解貢獻的那一組另外加入只有 slot channel、以及 slot channel 加 RGBA 但保留 pipelining 的版本（負載閘門只有 30%）。

### 結果

B 相對 A 的差距，7 對的中位數〔範圍〕。CPU 的正值表示 remote 每個 tile 用較多 cycles；吞吐量的正值表示 remote 較快。

| 檔案 | CPU 新內容 | CPU cache 命中 | 吞吐量 新內容 | 吞吐量 cache 命中 |
|---|---|---|---|---|
| three-pages（簡單頁） | +28.4% → **−2.0%**〔−8.2, +6.2〕 | +53.7% → **−2.2%**〔−7.8, +6.1〕 | −0.4% → −0.1%〔−5.4, +7.1〕 | +12.3% → +0.2%〔−6.4, +10.4〕 |
| dense-300p（密集文字） | +11.7% → **+2.5%**〔+0.3, +19.6〕 | +9.7% → **+1.0%**〔−5.8, +2.1〕 | +0.0% → −2.3%〔−13.6, +0.1〕 | +0.3% → −1.3%〔−6.3, +3.3〕 |
| photos（JPEG 影像） | +8.3% → **+1.4%**〔−5.8, +6.2〕 | +50.1% → **+0.8%**〔−7.5, +7.3〕 | −1.0% → −1.0%〔−5.0, +5.4〕 | +14.9% → +0.5%〔−1.6, +9.0〕 |
| gov-letter（非內嵌 CJK） | +27.6% → **−1.4%**〔−7.2, +5.7〕 | +56.4% → **+6.5%**〔−3.3, +11.9〕 | −4.0% → +0.7%〔−4.8, +5.7〕 | +6.9% → −3.8%〔−13.6, +3.6〕 |

每個 tile 的 kcycles（中位數；A＝in-process，B＝parent＋host，括號內是 host 的部分）：

| 檔案 | 新內容 修改前 A／B | 新內容 修改後 A／B | cache 命中 修改前 A／B | cache 命中 修改後 A／B |
|---|---|---|---|---|
| three-pages | 2220／2851（1793） | 2259／2320（1464） | 970／1510（531） | 970／959（161） |
| dense-300p | 5418／6100（5036） | 5428／5497（4600） | 5576／6197（5120） | 5569／5627（4696） |
| photos | 9665／10446（9316） | 9681／9808（8868） | 1098／1649（622） | 1065／1076（200） |
| gov-letter | 2177／2779（1674） | 2230／2254（1360） | 903／1386（367） | 925／974（167） |

cache 命中時 host 的部分從 367–622 降到 161–200 kcycles：host 不再換位，只逐列複製（dense-300p 除外：8 頁的 block 約 62 MiB，超過 hayro block cache 的 48 MiB 預算，「cache 命中」那幾輪其實都重新 render）。

### 各項修改的貢獻

同樣的方法，4 個版本一起輪替（負載閘門 30%）。每格是新內容／cache 命中：

| 檔案 | 修改前 | ① slot channel | ①＋② RGBA 傳輸（保留 pipelining） | ①＋②＋③ 取消 pipelining（本輪） |
|---|---|---|---|---|
| three-pages CPU | +28.2%／+46.0% | +23.8%／+40.1% | +14.6%／+17.4% | +1.7%／−3.1% |
| dense-300p CPU | +10.1%／+10.1% | +10.0%／+10.0% | +4.6%／+6.0% | +2.3%／+0.3% |
| photos CPU | +7.5%／+39.1% | +6.2%／+31.1% | +4.5%／+17.9% | +0.6%／−3.9% |
| gov-letter CPU | +24.9%／+48.5% | +26.0%／+37.8% | +17.9%／+22.9% | −0.6%／+4.8% |
| three-pages 吞吐量 | +0.8%／+14.1% | +1.0%／+19.5% | +3.8%／+60.2% | −1.3%／+3.6% |
| dense-300p 吞吐量 | +1.1%／+1.7% | +0.4%／+0.3% | +3.2%／+1.7% | −2.1%／+0.1% |
| photos 吞吐量 | −0.8%／+22.6% | +0.1%／+34.8% | +0.2%／+60.8% | +0.0%／+3.7% |
| gov-letter 吞吐量 | +1.5%／+12.8% | −3.3%／+22.1% | +3.2%／+43.8% | −0.9%／−4.1% |

- slot channel 本身只省 IPC：cache 命中少 0–11 個百分點（dense-300p 的 tile 很貴，幾乎看不出來）。
- RGBA 傳輸：cache 命中再少 4–23 個百分點。host 不再做慢的逐像素換位，parent 的換位和原本就要做的複製合成一趟。
- 取消 pipelining：新內容再少 2–19、cache 命中再少 6–22 個百分點，吞吐量回到與 in-process 相同。

### 取捨

- **cache 命中的吞吐量**：第二輪比 in-process 快（本輪量到 +0.3% 至 +14.9%，dense-300p 本來就沒有差），本輪與 in-process 相同。保留 pipelining（`render_queue_depth` 2）時快 44–61%（dense-300p 沒有差），但 CPU 多 5–23%。新內容的吞吐量兩種做法都與 in-process 相同。
- **背景負載較高時**：另一組 7 輪（只有 30% 閘門，每對開始前的負載最高 24–30%）。修改後的 CPU 為新內容 +4.1% 至 +6.1%、cache 命中 +2.7% 至 +18.9%（修改前 +9.0% 至 +32.6% 與 +14.3% 至 +69.2%）；cache 命中的吞吐量 −1.6% 至 −19.2%。每個 tile 有兩次 thread 交接，tile 很便宜時，喚醒延遲就顯得明顯；in-process 沒有交接。
- **第一頁**：不開 UI，量開檔加第一頁所有 tile（three-pages，scale 1.5，新的 process 與新的 host，兩種做法各 18 對）。remote 比 in-process 多：取消 pipelining 時 1.16–1.23 ms，保留時 1.24–1.34 ms。

### 每個請求的成本

單一 thread 依序呼叫，2000 次的中位數，dense-300p 第 1 頁，scale 2；各版本跑 2 次，數字是 2 次的範圍（負載 < 20% 才開始）：

| 呼叫 | 修改前 remote | 修改後 remote | in-process |
|---|---|---|---|
| 1×1 px render | 22.5–38.6 µs（135–212 kcycles） | 9.5–21.8 µs（65–118 kcycles） | 0.2 µs |
| cache 命中的 512 px tile | 927–1161 kcycles（host 266–353） | 988–1045 kcycles（host 164–216） | 738–901 kcycles |

### B-8 抽查

- **方法**：bench-app 1.1.0、preset `fastpdf`、`-Runs 1 -ThreadDetail -NoScreenshots`。dist build 取 HEAD 加本輪的 crate（不含其他進行中的修改），16,715,776 B。A＝`--engine hayro`，B＝`--engine hayro-isolated`，3 頁情境 4 對，每次啟動前負載 < 30%（實際 4–18%）。第一次啟動前先執行一次 `--version`（不開 UI），避開新 binary 的掃毒。

| 指標 | A 中位數〔範圍〕 | B 中位數〔範圍〕 | 配對差中位數〔範圍〕 |
|---|---|---|---|
| `window_visible` | 200.31 ms〔196.58, 203.05〕 | 201.40 ms〔192.88, 214.46〕 | +2.95 ms〔−7.42, +11.41〕 |
| `first_page_exact` | 204.78 ms〔196.62, 210.00〕 | 207.08 ms〔200.14, 227.62〕 | +8.45 ms〔−9.86, +18.68〕 |
| `document_opened` | 194.61 ms | 196.19 ms | +4.00 ms〔−7.63, +10.58〕 |
| idle 整個 tree 的 Mcycles／s | 27.20 | 28.64 | +1.34 |
| idle 整個 tree 的喚醒次數／s | 199.37 | 196.75 | −3.26 |
| private bytes | 107.30 MB | 113.40 MB | +6.00 MB |
| private working set | 24.70 MB | 28.20 MB | +3.50 MB |

- `first_page_exact` 的配對差超過 5 ms。各對的差：+11.05、−9.86、+18.68、+5.86 ms。
  - 第 3、4 對的 B 整個啟動就晚了（`window_visible` +11.4 與 +6.0 ms），與 render 無關。
  - `first_page_exact` − `window_visible`（第一頁在第一個 frame 之後多久畫上去）只有兩種值：0 或 6–15 ms（晚一個 frame）。A 是 0.0、9.7、5.9、0.3 ms，B 是 11.1、7.3、13.2、0.2 ms。第二輪的 6 對中，A 與 B 也都在 0–15 ms 之間跳。
  - 不開 UI 的第一頁量測（見〈取捨〉）顯示 render 的差距是 1.2 ms，有沒有 pipelining 都一樣。
  - 4 對不足以判定；需要時以第二輪的方法（每個情境 6 對）重跑。
- **idle**：B 的兩個 host 共 30 條 FastPDF thread 都是 0 cycles、0 次喚醒，parent 的 `fastpdf-render-*`、`fastpdf-remote-*` 也是。其中一個 host 有一條 OS 的 thread pool worker（起始位址在 `ntdll`）在 10 秒內醒 1 次（0.1 Mcycles／s），第二輪也有，不是 FastPDF 建立的 thread。

### Idle（不開 UI）

- 量測：開檔、render 第一頁，等 8 秒再量 10 秒。hayro 的 render pool 在沒有工作 5 秒後結束 thread，這是 engine 的一次性行為，in-process 也有，所以等它結束後才量。
- 結果（2 次）：document host 0.00 Mcycles、待命 host 0.00 Mcycles，兩個 host 在窗口內沒有任何 thread 有 cycles；parent 0.45–1.38 Mcycles，來自 OS 結束閒置的 thread pool worker（沒有名稱的 thread），FastPDF 的 thread 都是 0。

### 正確性

- **84 個 fixture**：方法同第二輪。372 個 render 逐位元組相同；2 頁的 geometry 錯誤與 8 個 render 錯誤兩邊相同；190 個 text layer 相同；0 個差異，host 0 次 crash。
- **測試**：取消、crash 歸責、deadline、slot 回收、空檔、只交 handle 的既有測試全部通過（engine-remote：55 個單元測試、27 個整合測試）。新增或修改的測試：
  - slot channel 的請求順序、回覆、cancel flag 與長度上限；
  - event 與 semaphore 的喚醒；
  - 從 slot 複製時換 R／B（含不足一個像素的尾端與越界）；
  - in-process／remote 比對加上 BGRA、夜間模式、R180 的組合；大於 slot 的 render 走命令，由 render thread 處理；
  - `SlotSpec` 的新欄位（控制區塊大小、event 數量、handle 值）的驗證。
