# 首頁的 GPU 初始化：WARP 優先與 driver 多執行緒（研究，2026-10-07）

對應：`docs/benchmarks/low-end.md`〈後續〉第 2 點、ADR 0009（啟動延遲）、ADR 0011（GPUI 本地 patch）。

這是研究與原型，**沒有**改動 `vendor/gpui_windows`。原型 patch：[`warp-first-experiment.patch`](warp-first-experiment.patch)，以環境變數開關。

## 結論

- **WARP 優先能把 GPU driver 的啟動移出首頁的關鍵路徑**（第一個 frame 送出）：
  - RTX 5090：171 → 78 ms（螢幕上 215 → 105 ms）；
  - 內顯（基準扣除本機特有的跨 adapter 成本後估計）：全速 103 → 71 ms，1/3 速 288 → 213 ms；
  - **2 核、1/6 速反而變慢**：611 → 665 ms（估計）。WARP 用 CPU 畫第一個 frame，在 2 個慢核心上要約 250 ms；
  - 切換之後，捲動與縮放和基準相同；切換後的畫面和從頭就用硬體的畫面逐像素相同。
- **WARP 優先的代價**：
  - WARP 的 worker thread 留在 process 裡（32 個邏輯 CPU 時多 27 條），idle private working set 多 0.7–2.6 MB（邏輯 CPU 越多越多）；切換期間兩個 device 同時存在，peak 多 28–35 MB；
  - 切換時 UI thread 要重建 renderer：RTX 5090 約 10 ms，內顯（不含跨 adapter，估計）14–82 ms；
  - 4 次中有 1 次在切換後閃了一下（約 50 ms），要調整 renderer 重建的順序才能消除；
  - 原型中 WARP 優先本身約 170 行，跨平台初始化、vsync thread 與 renderer，而且每次啟動都會走 device lost 的流程。
- **關閉 driver 多執行緒是低風險的小改進**：
  - 每一種 GPU 的 idle private working set 都少 3–4 MB；NVIDIA 少 67 條 thread；peak 少 4–20 MB；
  - 全速時首頁快 4–12 ms（NVIDIA 建 device 約快 10 ms）；D 慢 24 ms，但兩組範圍重疊；
  - 捲動與縮放沒有可判定的差異（各 2 次）；
  - 只需要在建立 device 時多加一個旗標。
- **附帶發現**：本機內顯的首頁時間含有跨 adapter swap chain 的成本（全速約 107 ms，在慢 CPU 上等比例放大），`low-end.md` 已據此更正。

## 建議

1. **關閉 driver 多執行緒：建議採用**，作為 `vendor/gpui_windows-patches/` 的 patch 0004（ADR 0011 增補）。
   - 採用前依專案規則，用正式的 dist build 做 B-8 配對量測（記憶體、首頁、捲動與縮放各 6 對），確認縮放沒有變慢。
   - **2026-10-07 已採用**（patch 0004）。B-8 第十一輪（3 頁 6 對、300 頁 10 對）：idle private working set −3.7 MB、thread −67、peak −5～−13 MB；啟動、捲動、PageDown 沒有差異；**縮放的最後一個畫面晚約 4 ms**（8 對有效數據中 7 對較晚），上表各 2 次的「沒有可判定的差異」不成立。見 `b8-app.md`〈第十一輪〉、ADR 0011〈增補：patch 0004〉。
2. **WARP 優先：目前不建議採用**，保留原型與數據。理由：
   - 效益最大的是 NVIDIA 接螢幕的桌機，而它們已經達成 < 200 ms；
   - 在最慢的機器上反而變慢，需要再加判斷，例如邏輯 CPU 少於 4 個時不用 WARP；
   - patch 大，而且閃爍要先解決；
   - 內顯筆電上的實際效益，要等實機量測（螢幕由內顯驅動、Intel 內顯）才知道。
3. 如果之後要做 WARP 優先：
   - 先修閃爍（硬體 swap chain 先畫再接上 DirectComposition），並依 CPU 數決定是否啟用；
   - 寫 ADR；
   - 考慮把硬體的 swap chain 與 DirectComposition 也在背景 thread 建好，讓切換時 UI thread 只需要交換。

## 背景

- **GPUI 的啟動順序**：
  - 在獨立的 thread 建立 D3D11 device，和平台初始化平行進行（ADR 0011 的 patch 0001）；
  - UI thread 在 `attach_gpu` 等這個 thread 結束，接著為視窗建立 renderer：swap chain、shader pipeline、DirectComposition；
  - 這些全部完成，才畫得出第一個 frame。
- **GPUI 已有的 device lost 復原流程**：
  - vsync thread 發現 device 被移除，或 `invalidate_devices` 被設定時，先等 350 ms，再重建 device；
  - 接著依序通知平台視窗、文字系統與每個視窗重建 renderer（swap chain、pipeline、DirectComposition，atlas 清空後依需要重新上傳）；
  - 再等 200 ms，強制重畫。
- **WARP**（Microsoft Basic Render Driver）是 Windows 內建的軟體 D3D11。建立它不需要啟動 GPU driver，但所有繪圖都由 CPU 完成。

## 原型

在測試 build 上加入三個開關（`5985ff5` 加上 [`low-end-adapter-override.patch`](low-end-adapter-override.patch) 與本研究的 patch）：

1. **WARP 優先**（`FASTPDF_TEST_WARP_FIRST=1`）：
   - 啟動時先建立 WARP device，renderer 與第一個 frame 都用 WARP；
   - WARP device 建好之後，另一條 thread 以低於一般的優先權建立硬體 device，再建立一次 shader pipeline 並丟掉，讓 driver 先編譯好 shader；
   - 硬體 device 好了、而且第一個 frame 已經在螢幕上 50 ms 以上之後，vsync thread 走 device lost 的流程切換過去，但不做那兩段等待（它們是給真的壞掉的 device 恢復用的）。
2. **關閉 driver 內部的多執行緒**（`FASTPDF_TEST_D3D_NO_THREADING=1`）：建立硬體 device 時加上 `D3D11_CREATE_DEVICE_PREVENT_INTERNAL_THREADING_OPTIMIZATIONS`。
3. **時間點**：GPU 的各個步驟以 `{"event":..,"t_ms":..}` 印在 stdout，和 FastPDF 的 `FASTPDF_BENCH` 事件一樣從 process 建立起算，bench-app 會一併記錄。
   - device 建好：`gpu_device_ready`、`gpu_warp_ready`、`gpu_hardware_ready`；
   - renderer 的各步驟：`new_begin`、`new_swap_chain`、`new_pipelines`、`new_dcomp`；切換時是 `switch_*`；
   - 第一個 frame 送出：`renderer_first_present`；切換後的第一個 frame：`renderer_present_after_switch`；
   - 建 swap chain 前後各檢查一次 NVIDIA 的 user-mode driver（`nvwgf2umx.dll`）有沒有載入，事件名稱結尾是 `_nv0` 或 `_nv1`。

### 做原型時的發現

1. **D3D11 runtime 一次只建立一個 device。**
   - 第一版同時開始建立 WARP 與硬體 device。在 RTX 5090 上，WARP 在 129.6 ms 才好，就在硬體 device（128.1 ms）之後；
   - WARP 本身只要約 12 ms（改成 WARP 先建、硬體後建之後，`gpu_warp_ready` 是 22–24 ms）；
   - 所以硬體 device 一定要等 WARP 建好才開始。
2. **「送出」不等於「顯示」。**
   - 硬體 device 若在第一個 WARP frame 送出前就好了，切換會讓 UI thread 忙著建立硬體的 swap chain，首頁反而更晚（內顯、1/3 速：583 ms）。所以切換要等第一個 frame 送出；
   - 只等送出也不夠：內顯全速的一次 run，WARP frame 在 73 ms 送出，80 ms 就開始切換，結果這個 frame 從來沒有出現在螢幕上，第一個畫面要等到硬體 frame（264 ms）；
   - 所以第二版改成送出後至少再等 50 ms（約三個 60 Hz 的 frame）。
3. **內顯的成本在跨 adapter 的 swap chain，而且只在本機出現。**
   - 本機的螢幕接在 RTX 5090 上。內顯建立 swap chain 之前，process 裡沒有 NVIDIA driver（`new_begin_nv0`），建好之後就有了（`new_swap_chain_nv1`），每一次都是如此；
   - DXGI 為了把內顯的畫面複製到 NVIDIA，在這一步載入 NVIDIA driver。這一步全速約 107 ms，1/3 速 330 ms，1/6 速 608 ms；
   - NVIDIA 自己建 swap chain 只要約 2 ms，WARP 約 1–6 ms，兩者都不會載入其他 driver；
   - 螢幕由內顯驅動的筆電沒有這一步，所以 `low-end.md` 的內顯首頁時間是上限，已在該文件更正。

## 量測

- 測試 build 與 bench-app 1.3.0 的設定同 `low-end.md`：
  - A：RTX 5090，全速；
  - B：Radeon 內顯，全速；
  - C：內顯，4 核 8 緒、1/3 速；
  - D：內顯，2 核 2 緒、1/6 速。
- 每種設定三個變體：0 基準、1 WARP 優先、2 關閉 driver 多執行緒。同一個 exe，以環境變數切換。
- 3 頁文件每組 3 次，300 頁文件（A、C，含捲動與縮放）每組 2 次，WARP 優先第二版另外每組 2–4 次。每輪內順序輪換；每次啟動前等系統忙碌度 < 30% 且沒有編譯在跑。
- 數字是中位數，括號內是範圍。時間從 process 建立起算。「送出」是 app 送出第一個 frame（`renderer_first_present`），「螢幕」是 bench-app 截圖看到第一個非空白畫面。

### 首頁（3 頁）

| 第一個 frame 送出（ms） | A NVIDIA | B 內顯 | C 內顯 1/3 速 | D 內顯 1/6 速 |
|---|---:|---:|---:|---:|
| 基準 | 171.3（166.7–171.5） | 208.3（204.9–236.3） | 616.3（574.3–639.4） | 1217.6（1180.6–1349.8） |
| 基準，扣除跨 adapter（估計） | — | 約 103 | 約 288 | 約 611 |
| WARP 優先（第二版） | **77.9**（66.7–79.1） | **71.2**（66.5–79.1） | **212.5**（212.1–230.9） | 665.4（632.7–698.1） |
| 關閉 driver 多執行緒 | 166.7（166.6–166.7） | 196.3（196.0–209.9） | 580.5（567.3–747.9） | 1241.9（1132.5–1328.4） |

| 螢幕：第一個非空白畫面（ms） | A | B | C | D |
|---|---:|---:|---:|---:|
| 基準 | 215.4（189.1–219.9） | 223.2（222.0–261.1） | 653.1（608.6–676.3） | 1242.5（1209.6–1388.5） |
| 基準，扣除跨 adapter（估計） | — | 約 118 | 約 325 | 約 636 |
| WARP 優先（第二版） | **104.9**（94.1–132.9） | **97.7**（97.1–110.6） | **247.8**（243.6–267.7） | 696.0（667.4–724.7） |
| 關閉 driver 多執行緒 | 191.8（182.6–199.2） | 223.4（209.8–225.1） | 628.1（591.6–779.8） | 1276.3（1160.9–1361.6） |

- 次數：基準與關閉多執行緒各 3 次；WARP 優先第二版 A、B、C 各 4 次，D 2 次。
- 「扣除跨 adapter」是基準減去建立 swap chain 的時間，再加回 NVIDIA 建 swap chain 的約 2 ms。這是估計：螢幕由內顯驅動時，內顯建 swap chain 的成本沒有實測。
- 基準的逐步拆解（中位數，從 `main` 起算）：

  | | 建 device | swap chain | shader pipeline |
  |---|---:|---:|---:|
  | A NVIDIA | 116 | 2 | 1.3 |
  | B AMD | 35 | 107（跨 adapter） | 2.5 |
  | C AMD 1/3 速 | 133 | 330（跨 adapter） | 12 |
  | D AMD 1/6 速 | 271 | 608（跨 adapter） | 18 |

- WARP 用 CPU 畫圖，所以「首頁完全清晰」（app 在 paint 時自報）和真正送出之間的差距變大：A 69 → 78 ms，C 164 → 213 ms，D 412 → 665 ms。判定 WARP 優先的首頁，要看送出或螢幕的時間。
- D 的 WARP 優先比基準（扣除後）慢：在 2 個 1/6 速的核心上，WARP 畫第一個 frame 要約 250 ms。

### 切換（WARP 優先第二版）

| | A | B | C | D |
|---|---:|---:|---:|---:|
| 硬體 device 好了（ms） | 129.4 | 50.4 | 150.7 | 665.6 |
| 開始切換（ms） | 142.2 | 125.3 | 280.3 | 857.8 |
| UI thread 忙於切換（ms） | 9.9 | 113.2 | 327.3 | 654.8 |
| 　其中跨 adapter 的 swap chain（ms） | 2.0 | 101.6 | 290.1 | 575.1 |
| 　扣除後（估計，ms） | 約 10 | 約 14 | 約 39 | 約 82 |

- 切換在硬體 device 好了、而且第一個 frame 送出 50 ms 以上之後才開始。D 的硬體 device 比較晚好，因為它的 thread 優先權較低，和 WARP 畫第一個 frame 搶 2 個核心，這是刻意的。
- 切換期間 UI thread 不處理輸入。切換在首頁出現之後，所以使用者在這段時間的第一個操作會延遲：RTX 5090 約 10 ms，內顯（不含跨 adapter，估計）14–82 ms。
- 第二版的 14 次 run，WARP 畫的首頁**全部**在切換完成前就出現在螢幕上。
- **閃爍**：C 的 4 次中有 1 次，切換後約 50 ms 內畫面整個變了兩次（626、676 ms）。A、B、D 沒有出現。
  - 推測是新的 DirectComposition 先接上了還沒畫任何東西的 swap chain，DWM 在硬體的第一個 frame 之前合成了一次；
  - 修正方向：硬體的 swap chain 先畫好第一個 frame，再接到 DirectComposition 上。這需要調整 renderer 重建的順序。

### 記憶體與 thread（3 頁，idle 10 秒結束時）

| | A | B | C | D |
|---|---:|---:|---:|---:|
| private working set，基準（MB） | 27.9 | 44.6 | 44.5 | 44.4 |
| 　WARP 優先 | 30.4 | 47.2 | 46.2 | 45.0 |
| 　關閉 driver 多執行緒 | **24.1** | **41.5** | **41.4** | **41.3** |
| private bytes，基準（MB） | 112.9 | 165.3 | 165.1 | 165.1 |
| 　WARP 優先 | 114.5 | 170.3 | 169.7 | 170.0 |
| 　關閉 driver 多執行緒 | **108.2** | **157.9** | **159.1** | **157.5** |
| thread 數，基準 | 156 | 125 | 125 | 124 |
| 　WARP 優先 | 183 | 152 | 131 | 125 |
| 　關閉 driver 多執行緒 | **89** | **120** | **120** | **119** |
| peak private bytes，基準（MB） | 121.2 | 173.7 | 173.8 | 174.2 |
| 　WARP 優先 | 149.1 | 209.2 | 207.6 | 208.9 |
| 　關閉 driver 多執行緒 | 116.5 | 168.8 | 169.4 | 166.2 |

- **WARP 優先**：切換之後，WARP 的 worker thread 還留在 process 裡（隨邏輯 CPU 數增加：32 個時多 27 條，8 個時多 6 條，2 個時多 1 條），private working set 多 0.7–2.6 MB；切換期間兩個 device 同時存在，peak 多 28–35 MB（表中 WARP 優先是第一版的 3 次；第二版的 private working set 相同，peak 差 0–4 MB）。
- **關閉 driver 多執行緒**：每一種 GPU 的 private working set 都少 3–4 MB；NVIDIA 少 67 條 thread。
- idle CPU 在所有組合都是 0–0.5% 單核；整個 tree 的喚醒都是每秒約 62–65 次，來自 NVIDIA driver 那條每秒 60 次的 thread（B、C、D 是跨 adapter 載入的 NVIDIA driver）。關閉多執行緒不影響這條 thread。

### 互動（300 頁，各 2 次）

| | A 基準 | A WARP 優先 | A 關閉多執行緒 | C 基準 | C WARP 優先 | C 關閉多執行緒 |
|---|---:|---:|---:|---:|---:|---:|
| 滾輪：第一個畫面變化（ms） | 22.4 | 24.4 | 26.1 | 22.3 | 21.9 | 20.2 |
| 滾輪期間每秒畫面變化數 | 52.9 | 51.1 | 51.6 | 48.1 | 50.1 | 51.7 |
| 滾輪：最後輸入後到穩定（ms） | 144.0 | 158.0 | 153.0 | 164.5 | 165.0 | 156.0 |
| 縮放：最後輸入後到穩定（ms） | 37.5（35.0–40.0） | 42.5（38.0–47.0） | 44.5（40.0–49.0） | 97.0 | 94.5 | 92.0 |
| peak private bytes（MB） | 285.0 | 285.5 | 271.2 | 360.5 | 361.1 | 340.2 |
| idle private working set（MB） | 29.9 | 32.2 | 25.9 | 46.4 | 47.8 | 43.4 |

- 切換到硬體之後，捲動與縮放和基準相同：WARP 只用在第一個畫面，沒有留下效能影響。
- 關閉多執行緒在 C 沒有差異；A 的縮放穩定時間多 7 ms，但兩組的範圍相鄰、各只有 2 次，判斷不了。互動後的 peak private bytes 少 14–20 MB。

### 畫面一致性

- 同一次 run 中，以 WARP 畫的第一個畫面和切換到硬體後的最後畫面逐像素比對：
  - 3.6–3.8% 的像素不同，每個顏色通道最多差 2（滿分 255），平均差 1，都在文字的反鋸齒邊緣，肉眼看不出來；
  - bench-app 的變化偵測以 4 px 格子比對，會把這種差異記成一次變化（`change_timeline` 的最後一筆）。
- 切換後的硬體畫面和基準（從頭就用硬體）的畫面**逐像素相同**。

## 重現方式

```powershell
# 在乾淨的 clone 上套用 patch（不要在 D:\fastPDF 套用）；它已包含 low-end-adapter-override.patch 的修改
git clone D:\fastPDF warp; cd warp
git apply docs/benchmarks/warp-first-experiment.patch
$env:CARGO_TARGET_DIR = 'D:\fastPDF\target\agent-lowend'
cargo build --profile dist -p fastpdf-app --locked

# 例：RTX 5090 上的 WARP 優先（開關由 bench-app 的環境傳給 app）
$env:FASTPDF_TEST_WARP_FIRST = '1'
pwsh -File tools/bench-app/bench-app.ps1 -Preset fastpdf -Exe D:\fastPDF\target\agent-lowend\dist\fastpdf.exe `
    -Pdf fixtures/generated/small-text/three-pages-platypus-times.pdf -Runs 3 -ThreadDetail
```
