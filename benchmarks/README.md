# Benchmarks

- `baseline.json`：M1 baseline（spec §41）。所有效能修改都和它比較。
- `runs/`：每次量測的輸出（不 commit）。
- 量測方法、指標定義、計畫（B-0 到 B-8）見 `docs/PROJECT_AUDIT.md` 的〈Benchmark Plan〉；profiler 用法見 `docs/profiling.md`。

## 產生 baseline 的程序

```bash
# 1. 測試語料（deterministic）
uv run tools/fixtures/generate.py --profile quick

# 2. 關掉其他重度負載（瀏覽器影片、編譯、索引），插電、電源模式「最佳效能」

# 3. 每個檔案 3 次獨立子 process，取 median
cargo run --release -p fastpdf-bench -- corpus fixtures/generated/manifest.json \
    --repeat 3 --timeout 120 --out benchmarks/baseline.json
```

## 目前的 baseline

- `baseline.json`：2026-10-04，engine Hayro（git `ced00dd0`），FastPDF `a920ea8`（bench 從該 commit 的乾淨匯出目錄 build；報告中的 `-dirty` 來自執行時工作目錄裡尚未 commit 的列印 crate，與量測的程式碼無關），rustc 1.99.0，release profile，每檔 3 次子 process 取中位數，量測前機器 CPU 2.8%。
- 機器：AMD Ryzen 9 9950X（16C/32T）、128 GB RAM、Windows 11 Pro。
- 結果摘要與 zpdf 的比較見 `docs/engine-comparison.md`。
- 2026-10-05 以配對方式比較 M1 與 HEAD（`756dfe1`）：84 個檔案狀態相同，到第一頁的時間幾何平均比值 1.001，沒有退步，所以 baseline 沒有更新（`docs/benchmarks/b1-paired.md`）。

## 有背景負載時：配對比較

`fastpdf-bench compare` 拿一次量測和 `baseline.json` 比較，只有在安靜的機器上才準。機器上有其他負載（編譯、VM、索引）時，請改用配對比較：每個檔案用兩個 build 交替執行，比較每一對的比值。

```bash
python tools/bench_paired.py --a OLD/fastpdf-bench.exe --b NEW/fastpdf-bench.exe --pairs 3
```

判定規則與讀法見 `docs/benchmarks/b1-paired.md`。

## 讀數字時要注意

- 數字只能和**同一台機器、同一個 toolchain**（報告裡的 `machine`、`rustc`）的數字比較。
- 時間是 **warm file cache**（檔案已在 OS cache）。cold 數字要另外量並註明。
- `rss_peak_mb` 是該檔案子 process 的 peak working set，包含 harness 本身（約 5–10 MB）。
- `time_to_first_page_ms` 是 engine 端的「read + open + metadata + 第 1 頁 render」，**不含**視窗建立、GPU 上傳與合成；完整的 time to first visible page 由 app 層 benchmark（B-8）量。
- `status` 不是 `ok` 的檔案要逐一檢視：malformed 類別預期會是 `open_error` 或 `partial`；`crash`／`timeout` 一律是 bug。

## App 層 KPI 定義：Idle RAM、Idle CPU（spec §29；PROJECT_AUDIT R8、R9）

量測工具是 `tools/bench-app`（1.1.0 起），量法細節見它的 README。目前的數字與歸因見 `docs/benchmarks/b8-app.md`。

### Idle RAM

三種記憶體指標都要列出（都是 process tree 的總和）：

| 指標 | bench-app 欄位 | Windows 來源 | 內容 | 工作管理員 |
|---|---|---|---|---|
| **Private working set**（KPI） | `idle.private_ws_mb` | `PROCESS_MEMORY_COUNTERS_EX2.PrivateWorkingSetSize` | 目前在實體記憶體中、只屬於這個 process 的頁 | 「處理程序」分頁的「記憶體」欄 |
| Working set | `idle.ws_mb` | `WorkingSetSize` | 目前在實體記憶體中的所有頁，包含和其他 process 共用的 DLL 程式碼、字型檔映射 | 「詳細資料」分頁的「工作集（記憶體）」 |
| Commit charge（private bytes） | `idle.private_mb` | `PrivateUsage` | process 向系統承諾的 private 記憶體，不論是否在實體記憶體中；包含 DLL 的 copy-on-write 頁 | 「詳細資料」分頁的「認可大小」 |

另有 `idle.commit_charge_mb`（commit charge 再加上 `SharedCommitUsage`），供需要系統 commit 總量時參考。

**spec §29 的「Idle RAM < 50 MB」以 private working set 判定。** 理由：

1. **這是使用者看到的數字。** 工作管理員預設顯示的「記憶體」就是 private working set，使用者拿 FastPDF 和其他 reader 比較時看的也是這一欄。
2. **Working set 不是 FastPDF 獨占的成本。** 它把系統 DLL 的程式碼頁、DirectWrite 映射的字型檔等共用頁面算進每一個 process，多個 process 加總時會重複計算。
3. **Commit charge 主要由 GPU driver 決定，FastPDF 控制不了。** 本機（NVIDIA RTX 5090）的 GPUI 最小視窗就有 82 MB commit：
   - NVIDIA user-mode driver 的 DLL 以 copy-on-write 方式載入，光是 image 的 commit 就約 31 MB，實際寫過、在實體記憶體中的只有約 1.5 MB；
   - driver 與 D3D 另外用 `VirtualAlloc` 配置了約 35 MB，大部分不在實體記憶體中。

   換一張顯示卡或換一版 driver，這個數字就會改變（R12）。
4. **Private working set 可以被修剪，所以要加上 guard：**
   - 量測時視窗必須可見、沒有最小化；
   - app 不得呼叫 `EmptyWorkingSet`、`SetProcessWorkingSetSize` 這類修剪 working set 的 API（spec §29「不要作弊」）；
   - commit charge 一律一起報告。比較兩個版本時，commit charge 增加超過 10% 也算 regression（spec §30），即使 private working set 沒有變。

### Idle CPU

- **KPI：`idle.cpu_pct_of_one_core`**：process tree 在 idle 10 秒內使用的 CPU time（kernel + user），除以 10 秒，以「單一核心的百分比」表示。
- **「接近 0%」（spec §29）的判定**：以下兩個條件都要成立。
  - 中位數 ≤ 0.1%，也就是 10 秒內 ≤ 10 ms；
  - 沒有 thread 以 refresh rate 週期性醒來：`idle.threads_detail.main_switches_per_s` 與 `vsync_switches_per_s` 都 ≤ 2 次／秒（需要 `-ThreadDetail`）。
- **為什麼需要第二個條件**：
  - Windows 的 CPU time 以 15.6 ms 為單位累計，10 秒窗口的解析度只有約 ±0.16%，分不出 0.05% 和 0.15%；
  - 每秒被喚醒的次數（context switch／秒）和 `QueryThreadCycleTime` 的 cycles 才看得出週期性喚醒；
  - 週期性喚醒會隨螢幕更新率等比放大（144 Hz、240 Hz），在筆電上也會妨礙 CPU 進入省電狀態。
- **GPU driver 自己的 thread 不列入判定**，但要報告。本機 NVIDIA D3D11 driver 有一條 thread 固定每秒醒來 60 次，不論 GPUI 或 FastPDF 做什麼。
- **FastPDF 端的判定（回歸防線）**：idle 窗口內 FastPDF 不畫任何 frame。
  - 條件：每次 run 的 `idle.app_frames.render` 與 `idle.app_frames.paint` 都是 0；互動後回到 idle 的窗口（`post_interaction_idle.app_frames`）也一樣。
  - 量法：bench-app 1.2.0 的 `-AppProbe`（preset `fastpdf` 預設開啟）在窗口前後讀 FastPDF 的 frame 計數（需要 `FASTPDF_BENCH=1`，preset 會設定）。不為 0 時，summary 的 `idle.app_frames` 標成 `frames_while_idle`，並印出警告。
  - 這一條和上面兩個條件分開判定：GPUI 的 vsync 迴圈每秒喚醒主執行緒，但不會讓 FastPDF render，所以 GPUI 沒有 patch 時上面兩條不會成立，這一條卻必須成立。讓它失敗的修改就是 regression。
  - 偶爾 1 次 render＋paint、wake 為 0，可能來自視窗 activation 改變或系統廣播，要看當次 run 再判斷；持續的 frame（例如每秒數十次）一定是 regression。細節見 `tools/bench-app/README.md`〈-AppProbe：FastPDF 的 frame 計數〉。
- **歸因**：
  - `-ThreadDetail` 列出每條 thread 的 cycles 與喚醒次數；
  - `-AppProbe` 的 `idle.app_frames` 分出 FastPDF 自己的 frame（render、prepaint、paint）與背景工作的喚醒（wake）。

### 何時量

- **情境**：
  - 不開檔的空視窗（`-Scenario launch-empty`）；
  - 3 頁小檔 `fixtures/generated/small-text/three-pages-platypus-times.pdf`；
  - 需要時再加 300 頁的 `large-text/dense-300p-times.pdf`。
- **時點**：啟動後等畫面穩定（連續 5 張截圖、而且 1.5 秒沒有變化），再等 1 秒，取接下來的 10 秒（`-IdleSeconds 10`）。記憶體取窗口結束時的值，CPU 取窗口內的差值。
  - 窗口結束時，Hayro 的 render thread 已經閒置超過 5 秒並結束，它們的 cache（3 頁文件約 8 MB）也已釋放，所以量到的是穩定的 idle 狀態。
  - 縮短 `-IdleSeconds` 或提早取樣時，記憶體會偏高。
- **狀態**：
  - 開檔後不做任何操作。互動後的 idle（`post_interaction_idle.*`）另外報告，不算 KPI，因為它反映的是各 cache 的 budget；
  - 視窗可見、沒有最小化，不需要在前景；
  - `FASTPDF_SETTINGS_FILE`、`FASTPDF_RECENT_FILE` 設為空值（preset `fastpdf` 會設定）；
  - 使用 dist build。
- **環境**：量測前確認沒有 cargo、rustc、link 在跑。記錄背景負載：`cpu_load_pct_before` 與 `idle.system_cpu_busy_pct`。

### 如何報告

- 每個情境至少 3 次，報告中位數與範圍（min–max）。比較兩個版本時用交替配對（A B A B …，至少 3 對）。
- 三種記憶體指標都要列出，並標明 KPI 是 private working set。
- Idle CPU 同時列出 CPU time（% 單核）、整個 tree 的 context switch／秒與 Mcycles／秒，以及主執行緒、`VSyncProvider` 各自的數字。
- 註明以下環境資訊，換機器的數字不能直接比較（R12）：
  - 機器、GPU 與 driver 版本；
  - 螢幕解析度、更新率、DPI；
  - build profile（dist）與 git revision；
  - 背景負載。
- 低階機器的近似量測（內顯、WARP、較少與較慢的核心）見 `docs/benchmarks/low-end.md`：用 bench-app 1.3.0 的 `-Affinity`、`-Slowdown`，加上可選 GPU 的測試 build。
