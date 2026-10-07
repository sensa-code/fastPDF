# 低階設備模擬量測（R12，2026-10-06）

對應：`docs/PROJECT_AUDIT.md` R12（目前所有量測都在高階機器上）、ADR 0009〈R12〉、spec §29 KPI。

## 結論

- **小檔首頁 < 200 ms 只在 CPU 快的機器上成立。**
  - 實測：
    - 弱內顯配快 CPU（B）：3 頁 193 ms，300 頁約 202–206 ms，落在門檻附近；
    - 主流舊筆電（C，4 核 8 緒、1/3 速）約 570 ms；
    - 入門筆電（D，2 核 2 緒、1/6 速）約 1.15 秒。
  - **2026-10-07 更正：B、C、D 的實測包含只有本機才有的成本。**
    - 螢幕接在 RTX 5090 上。內顯第一次建立 swap chain 時，DXGI 會把 NVIDIA driver 載入 FastPDF 的 process，以便跨 adapter 複製；
    - 這一步全速約 107 ms，1/3 速 330 ms，1/6 速 608 ms（逐步時間點見 `docs/benchmarks/warp-first.md`）；
    - 螢幕由內顯驅動的筆電沒有這一段。扣除後估計（app 自報）：B 約 97 ms，C 約 0.27 秒，D 約 0.58 秒。C、D 仍未達成。
  - 啟動時間幾乎和單核速度成正比（C ≈ 3 × B、D ≈ 6 × B），和核心數無關，與 ADR 0009〈R12〉的結論一致。
  - **時間主要花在 GPU 初始化**：
    - 沒有硬體 driver 的 WARP（E），在 1/3 速度下仍然 155–161 ms 就畫出首頁，換算成全速約 52 ms，這是 FastPDF 與 GPUI 其餘的工作；
    - NVIDIA 建立 D3D11 device 約 105–115 ms（全速）。AMD 內顯建立 device 只要約 35 ms，另外是上述跨 adapter 的 swap chain 約 107 ms；
    - 這些都是 CPU 工作，在慢 CPU 上等比例放大。
  - 開 2000 頁和開 3 頁一樣快（D：1142 ms 對 1142 ms），「開檔不處理整份文件」在慢 CPU 上也成立。
- **Idle RAM < 50 MB：在內顯上接近上限。**
  - AMD 內顯 44–47 MB；2000 頁 50.2 MB，略為超過。
  - FastPDF 自己的 heap 不變（3.2 MB）。多出的約 17 MB 是 driver 的配置，其中包含跨 adapter 複製所載入的 NVIDIA driver，所以是上限（見〈限制〉）。
  - WARP 只有 20–23 MB。
- **Idle CPU 接近 0：所有設定都成立。**
  - idle 實測 0–0.3% 單核，依降速倍數換算後中位數 ≤ 0.5%，最多約 1%；
  - 主執行緒每秒 0 次喚醒（中位數）；
  - FastPDF 在 idle 時畫 0 個 frame。
- **使用硬體 GPU 時，捲動的反應不受 CPU 速度影響。**
  - 滾輪與 PageDown 的第一個畫面變化都在約 22–26 ms（一到兩個 frame）；
  - 捲動期間每秒約 50 次畫面變化（受截圖頻率限制）；
  - 縮放需要重新 render，所以最後一次輸入後到穩定的時間隨 CPU 變慢：A 40 ms、C 99 ms、D 230 ms。
- **WARP（沒有 GPU 加速）可以用，但捲動不順**：首頁很快，但捲動期間每秒只有約 9 次畫面變化，第一個畫面變化 74 ms，最後一次輸入後約 0.3 秒才穩定。

## 為什麼不用 Docker 或 WSL

- spec §29 的 KPI 量的是 Windows GUI app 的時間線：D3D11 device、DirectComposition、視窗。Docker（Linux container）與 WSL（Linux VM）都執行不了 Windows 版的 FastPDF，只能跑 headless 的 `fastpdf-bench`，量到的是 Linux 上的 engine，不是 app。
- 它們限制資源的機制（cgroup 的 CPU quota、memory limit），在 Windows 上有對應的 job object，可以直接套在真正的 FastPDF 上。
- 在 WSL 裡限制資源，要修改 `.wslconfig` 再重啟 WSL，會中斷同一台機器上執行中的 WSL 工作（本機有一個 GitHub Actions runner 的 distro）。
- 本機沒有安裝 Docker。

所以改用 Windows 原生的方式模擬低階機器，直接量 app。

## 方法

### 本機硬體

- CPU：Ryzen 9 9950X，16 核 32 緒。相鄰的兩個邏輯 CPU 是同一個實體核心（SMT 兄弟）。
- GPU：
  - RTX 5090，driver 32.0.16.1088，**螢幕接在這張卡上**；
  - 9950X 內建的 AMD Radeon(TM) Graphics（RDNA 2，2 CU），driver 32.0.21043.5001。它本身就是很弱的內顯，比多數筆電的內顯還弱。
- 125 GB RAM，Windows 11 Pro 10.0.26200。

### 模擬設定

| | 名稱 | GPU | CPU | 近似的機器 |
|---|---|---|---|---|
| A | 基準 | RTX 5090 | 32 個邏輯 CPU，全速 | 本機 |
| B | 內顯 | Radeon 內顯 | 32 個邏輯 CPU，全速 | 快 CPU 配弱內顯 |
| C | 主流舊筆電 | Radeon 內顯 | 4 核 8 緒（`0xFF`），1/3 速 | i5-8250U、N100 等級 |
| D | 入門筆電 | Radeon 內顯 | 2 核 2 緒（`0x5`），1/6 速 | Celeron N4020、N4500 等級 |
| E | 沒有 GPU 加速 | WARP（軟體繪圖） | 4 核 8 緒（`0xFF`），1/3 速 | VM、遠端桌面、driver 故障的舊機器 |

- 速度倍數依公開的單核 benchmark（Geekbench 6）粗估：上述主流舊筆電的單核約為 9950X 的 1/2.5–1/3，入門筆電約 1/5–1/7。
- 兩兩比較：A↔B 只差 GPU；B↔C、C↔D 只差 CPU；C↔E 只差 GPU（內顯與 WARP）。

### 工具

- **bench-app 1.3.0**（`tools/bench-app/`）：
  - `-Affinity`：app 以暫停狀態建立，加入帶 affinity 限制的 job object 之後才開始執行，所以限制從第一個指令就生效，FastPDF 啟動時建立的 render host 也在 job 裡。1.2.0 是啟動後才設定，第一個 render host 可能不受限制。
  - `-Slowdown`：duty cycle。每 6 ms 中，app 的所有 process 只執行 1/倍數 的時間，其餘時間以 `NtSuspendProcess` 暫停（Chrome DevTools 的 CPU throttling 也是這個做法）。實測的有效倍數記錄在每次 run 的 `slowdown`。
  - `-AppEnv`：把 `FASTPDF_TEST_DXGI_ADAPTER` 傳給 app。
- **選 GPU 的測試 build**：GPUI 一律使用 `EnumAdapters` 中第一個支援 D3D11 的 adapter，也就是接著螢幕的 RTX 5090。改用其他 adapter 只能修改 Windows 的圖形設定（registry），所以另外做了一個測試 build：
  - 套用 [`low-end-adapter-override.patch`](low-end-adapter-override.patch)，依 `FASTPDF_TEST_DXGI_ADAPTER` 選 adapter（`radeon`、`warp`）。不設定時，行為和正式版相同；
  - 這個 patch **不會**放進 `vendor/gpui_windows-patches/`，正式版也不包含；
  - build：`5985ff5` 加上這個 patch，`cargo build --profile dist -p fastpdf-app --locked`，exe 16,885,248 bytes；
  - 五種設定都用同一個 exe。每次 run 的 stderr 都有 `fastpdf-test: DXGI adapter N: <名稱>`，確認選到的 adapter。

### 情境

- **3 頁**（`small-text/three-pages-platypus-times.pdf`）：啟動，idle 10 秒。
- **300 頁**（`large-text/dense-300p-times.pdf`）：
  - 啟動，idle 10 秒；
  - 20 格滾輪（50 ms 間隔）、5 次 PageDown、3 次 Ctrl+滾輪；
  - 互動後 idle 5 秒。
- **2000 頁**（`large-page-count/flat-2000p-reportlab.pdf`，只跑 A 與 D）：啟動，idle 10 秒。
- 每次啟動只量 1 次，共 4 輪。每輪內設定的順序輪換，讓負載的慢速漂移平均分到各設定。
- 每次啟動前等系統忙碌度（`Win32_Processor.LoadPercentage`，3 次取樣的平均）低於 30%，而且沒有編譯在跑，最多等 5 分鐘。這和第九、十輪相同。
- 量測前執行 5 次 `--version` 預熱。

### idle 窗口不降速

暫停 process 時，kernel 會送 suspend APC 給每條 thread，正在等待的 thread（例如 thread pool worker）會因此醒來。smoke test 實測：

- 降速期間，每條等待中的 thread 約醒來 330 次／秒；
- 10 秒 idle 多出約 100 ms 的 CPU。

這是量測方法造成的，不是 FastPDF 的行為。所以 bench-app 在 idle 窗口與互動後的 idle 窗口都會暫停降速。在慢 N 倍的 CPU 上，同樣的 idle 工作要花約 N 倍的 CPU 時間，結果表中另外換算。

## 結果

數字是 4 輪的中位數，括號內是範圍。時間從 process 建立起算；「app 自報」來自 `FASTPDF_BENCH`，「外部」是 bench-app 截圖看到的時間。

### 啟動

| 首頁完全清晰（app 自報，ms） | A | B | C | D | E |
|---|---:|---:|---:|---:|---:|
| 3 頁 | 166.0（164.1–168.8） | 192.6（178.5–202.3） | 569.7（537.0–628.3） | 1142.0（1061.1–1212.4） | 161.2（156.6–169.6） |
| 300 頁 | 168.2（167.7–180.7） | 205.6（180.6–541.4） | 569.6（550.0–592.3） | 1153.9（1114.8–1235.9） | 154.7（151.4–163.8） |
| 2000 頁 | 164.0（151.1–170.6） | — | — | 1142.1（1121.4–1169.2） | — |

| 外部：視覺完成（ms） | A | B | C | D | E |
|---|---:|---:|---:|---:|---:|
| 3 頁 | 190.2（186.0–195.2） | 220.6（203.9–221.3） | 611.2（591.7–682.8） | 1196.8（1109.8–1259.3） | 246.8（239.8–258.9） |
| 300 頁 | 193.6（185.4–203.1） | 239.3（201.3–579.0） | 609.9（592.8–639.5） | 1201.8（1171.7–1301.0） | 241.4（231.9–243.5） |
| 2000 頁 | 183.8（173.6–202.3） | — | — | 1198.5（1176.9–1235.2） | — |

- B 300 頁第 2 輪的 541 ms 是背景負載造成的：bench-app 在啟動瞬間取樣到 100% 忙碌。排除這一次，中位數是 202 ms。
- WARP 的畫面比 app 自報晚約 85 ms 才出現在螢幕上，硬體 GPU 只晚 24–56 ms。軟體繪圖的 frame 要經過另一條 composition 路徑。

**時間花在哪裡**（app 自報的時間線，3 頁，中位數，ms）：

| | `main` | `document_opened` | `window_visible` | `first_page_exact` |
|---|---:|---:|---:|---:|
| A | 11.6 | 158.7 | 164.4 | 166.0 |
| B | 11.0 | 186.5 | 192.6 | 192.6 |
| C | 19.1 | 540.6 | 565.3 | 569.7 |
| D | 37.1 | 1093.2 | 1142.0 | 1142.0 |
| E | 20.4 | 145.1 | 160.3 | 161.2 |

- 每一種設定的 `document_opened` 都緊接在 `window_visible` 前面：讀檔、開檔與 GPUI 初始化平行進行，最後由 GPUI 初始化決定時間。GPUI 初始化中最大的一段是建立 D3D11 device（ADR 0009）。
- 同樣是 1/3 速度，WARP（E）161 ms，AMD 內顯（C）570 ms，差距約 410 ms（換算成全速約 137 ms）。
  - 2026-10-07 以逐步時間點拆解：其中約 330 ms（全速約 107 ms）是跨 adapter 的 swap chain，只在本機出現；其餘主要是 AMD 建立 device；
  - NVIDIA（A 166 ms 對 E 換算全速約 54 ms）多出約 112 ms，幾乎都是建立 device。
- 這段成本在 driver 裡，FastPDF 能做的是把它移出關鍵路徑。研究與原型見 `docs/benchmarks/warp-first.md`。

### Idle

idle 窗口不降速（見〈方法〉），「換算」一列是實測的 CPU 乘上降速倍數，估計在慢 N 倍的 CPU 上的值。

| 3 頁 | A | B | C | D | E |
|---|---:|---:|---:|---:|---:|
| private working set（MB，KPI） | 27.9（27.8–27.9） | 44.6（44.5–44.6） | 44.4（44.4–44.5） | 44.4（44.4–44.5） | 20.5（20.4–22.0） |
| private bytes（MB） | 112.5（112.2–113.1） | 165.3（165.2–165.3） | 165.1（163.2–165.1） | 165.1 | 52.3（51.5–55.5） |
| CPU（% 單核，實測） | 0.00（0.00–0.16） | 0.16 | 0.00 | 0.00 | 0.00 |
| 主執行緒喚醒（次／秒） | 0.00 | 0.00（0.00–0.49） | 0.00 | 0.10（0.00–0.39） | 0.00（0.00–2.57） |
| FastPDF 的 frame（render） | 0 | 0 | 0 | 0 | 0 |

| 300 頁 | A | B | C | D | E |
|---|---:|---:|---:|---:|---:|
| private working set（MB，KPI） | 29.8（29.7–29.9） | 46.5（46.4–46.5） | 46.3（46.3–46.4） | 46.1（46.0–46.3） | 22.6（22.5–23.7） |
| private bytes（MB） | 115.1（115.0–115.2） | 167.2（167.1–167.4） | 167.2（167.1–167.2） | 167.0（166.9–167.1） | 54.4（54.4–55.6） |
| CPU（% 單核，實測） | 0.08（0.00–0.16） | 0.00（0.00–0.31） | 0.08（0.00–0.16） | 0.08（0.00–0.16） | 0.00（0.00–0.16） |
| CPU（% 單核，換算） | 0.08（0.00–0.16） | 0.00（0.00–0.31） | 0.24（0.00–0.48） | 0.48（0.00–0.96） | 0.00（0.00–0.48） |
| 主執行緒喚醒（次／秒） | 0.00 | 0.00（0.00–0.10） | 0.00（0.00–0.10） | 0.00（0.00–0.20） | 0.00 |
| FastPDF 的 frame（render） | 0 | 0 | 0 | 0 | 0 |

| 2000 頁 | A | D |
|---|---:|---:|
| private working set（MB，KPI） | 33.7（33.7–33.8） | 50.2（50.2–50.4） |
| private bytes（MB） | 119.1（119.0–119.2） | 171.1（171.0–171.2） |
| peak private bytes（MB） | 126.7（126.6–126.9） | 179.4（179.3–179.4） |

- 內顯多出的記憶體是 driver 的：同一份 3 頁文件，以 `-MemoryDetail` 拆解主 process 的 private working set：
  - A 24.2 MB，B 41.0 MB；
  - heap（FastPDF 自己的配置）A 3.23 MB、B 3.22 MB，**相同**；
  - 差異在 driver 以 `VirtualAlloc` 配置的區域（other_private）：A 15.4 MB，B 32.4 MB。
- 2000 頁比 3 頁多約 6 MB（A 與 D 都是），是文件本身的結構。D 的 50.2 MB 是 33.7 MB 加上內顯 driver 的約 16.5 MB。
- idle 時整個 tree 的喚醒（`-ThreadDetail`，3 頁）：
  - A 與 B 都是每秒約 61 次，幾乎全部來自 NVIDIA driver 那條每秒 60 次的 thread。B 的這條 thread 是跨 adapter 複製時載入的 NVIDIA driver 帶來的；
  - AMD driver 自己的 thread 每秒都不到 0.5 次；
  - WARP 整個 tree 每秒約 4 次；
  - 所以在螢幕由 AMD 內顯驅動的機器上，idle 喚醒應該會比本機少很多。

### 互動（300 頁）

| | A | B | C | D | E |
|---|---:|---:|---:|---:|---:|
| 滾輪：第一個畫面變化（ms） | 22.9（21.9–23.2） | 22.4（22.1–42.0） | 23.2（22.3–23.4） | 22.8（21.3–24.3） | 74.0（57.8–124.1） |
| 滾輪：最後一次輸入後到穩定（ms） | 139.0（139.0–156.0） | 156.0（140.0–164.0） | 156.0（155.0–172.0） | 162.0（140.0–174.0） | 314.5（290.0–322.0） |
| 滾輪期間每秒畫面變化數 | 52.0（50.0–53.0） | 49.8（30.9–52.1） | 50.0（49.0–52.0） | 49.0（47.0–50.0） | 9.0（8.0–9.0） |
| PageDown：第一個畫面變化（ms） | 22.4（21.5–39.3） | 25.5（21.0–34.6） | 22.5（22.1–23.7） | 22.0（21.0–22.2） | 56.5（55.5–57.2） |
| PageDown：最後一次輸入後到穩定（ms） | 6.0（0.0–21.0） | 24.0（18.0–27.0） | 14.0（6.0–24.0） | 22.5（0.0–40.0） | 87.0（73.0–90.0） |
| 縮放：第一個畫面變化（ms） | 39.0（22.1–40.2） | 25.0（22.2–28.9） | 22.6（22.2–22.9） | 22.2（20.5–23.1） | 65.3（56.4–73.9） |
| 縮放：最後一次輸入後到穩定（ms） | 40.0 | 58.5（39.0–91.0） | 98.5（89.0–109.0） | 229.5（172.0–239.0） | 205.5（156.0–222.0） |
| 互動後 idle 5 秒的 CPU（ms，實測） | 0.0 | 0.0 | 15.6（0.0–31.2） | 0.0（0.0–15.6） | 0.0（0.0–15.6） |
| 互動後 idle 的 frame（render） | 0 | 0 | 0 | 0 | 0 |
| peak private bytes（MB） | 284.6（283.9–285.2） | 367.0（357.1–369.9） | 356.9（353.0–360.5） | 349.4（318.8–365.1） | 218.8（217.3–219.5） |

- 截圖頻率約 55–65 Hz，所以每秒約 50 次已經接近量測上限，第一個畫面變化的解析度約 ±20 ms（bench-app README〈方法限制〉）。
- 捲動的第一個畫面變化只需要移動既有的 tile，在硬體 GPU 上和 CPU 速度無關。縮放要重新 render tile，所以隨 CPU 變慢。
- 內顯的 peak private bytes 比 A 多約 65–82 MB，也是 driver 的配置（commit，大部分不在實體記憶體中）。

## KPI 判定（低階設定）

| KPI（spec §29） | 目標 | B 內顯 | C 主流舊筆電 | D 入門筆電 | E 沒有 GPU 加速 |
|---|---|---|---|---|---|
| 小檔首頁 | < 200 ms | 193 ms ✅（300 頁約 202–206 ms）；扣除跨 adapter 估計約 97 ms | 570 ms ❌；估計約 0.27 秒 ❌ | 1142 ms ❌；估計約 0.58 秒 ❌ | 161 ms ✅（畫面約 247 ms） |
| Idle RAM（private WS） | < 50 MB | 44.6 MB ✅（上限） | 44.4 MB ✅ | 44.4 MB ✅；2000 頁 50.2 MB ❌（上限） | 20.5 MB ✅ |
| Idle CPU | 接近 0 | ✅ | ✅ | ✅（換算最多約 1%） | ✅ |
| 大型 PDF | 不需完整掃描 | — | — | ✅ 2000 頁與 3 頁同為 1142 ms | — |

## 後續

1. **實機量測**：Intel 內顯、螢幕由內顯驅動的筆電（低階最常見）。本次的內顯數字含跨 adapter 複製的成本，是上限。
2. **把 GPU driver 移出首頁的關鍵路徑**：2026-10-07 已做原型與量測（先用 WARP 畫第一個 frame，再切換到硬體 device），見 `docs/benchmarks/warp-first.md`。
3. **Idle RAM 在內顯上接近 50 MB**：多出的是 driver 的配置，FastPDF 控制不了。在實機上重新判定；如果仍然超過，`benchmarks/README.md` 的 KPI 定義要註明 GPU driver 的影響。
   - 2026-10-07：GPUI patch 0004（driver 不啟動 worker thread）在測試 build 上讓內顯的 idle private working set 少約 3 MB（44.6 → 41.5 MB，`docs/benchmarks/warp-first.md`），2000 頁的 50.2 MB 估計也會降到約 47 MB。
4. 高更新率與混合 DPI 仍未量測（R12 的另一部分）。

## 限制

- **內顯的跨 adapter 複製**：螢幕接在 RTX 5090 上，內顯畫好的每一個 frame，DXGI 都要複製到 NVIDIA。為此，FastPDF 的 process 裡也會載入 NVIDIA 的 driver（39 條 thread）。
  - 筆電的螢幕由內顯驅動，沒有這一步。所以 B、C、D 的記憶體、thread 數與 idle 喚醒是**上限**；
  - **啟動也受影響**（2026-10-07 實測）：NVIDIA driver 是在內顯第一次建立 swap chain 時載入的，這一步全速約 107 ms，在慢 CPU 上等比例放大，所以 B、C、D 的首頁時間也是上限（〈結論〉有扣除後的估計）；
  - present 也多了一次複製，這部分是偏慢的估計。
  - WARP（E）沒有這個問題：它的 process 裡沒有 NVIDIA 的 thread。
- **只有一種內顯**：Intel 內顯（低階筆電最常見）的 driver 行為與記憶體用量都不同。這裡量的是 AMD 的 driver。
- **duty cycle 不是降頻**：快取、記憶體頻寬、I/O 與 GPU 都還是本機的速度，GPU driver 與 DWM 在 app 之外的工作也不受影響。低階機器的 eMMC 與單通道記憶體會讓冷啟動更慢，這裡沒有模擬。
- **只量 warm 啟動**：第一次在內顯上啟動時，AMD driver 要編譯 GPUI 的 shader，會比較慢、用比較多記憶體。
  - smoke test 的第一次是 74 MB，之後約 45 MB；
  - 同一秒，`%LOCALAPPDATA%\AMD\DxCache` 寫入了一個新的 shader cache 檔；
  - 這是一次性的成本（driver 更新或清除 cache 之後會再發生一次），之後就使用 cache。
- **記憶體上限沒有模擬**：tile cache 固定 128 MiB，不依實體記憶體調整。300 頁文件互動後的峰值約 280 MB，4 GB 的機器也放得下；但沒有在記憶體不足、會 paging 的情況下量測。
- 單一螢幕、60 Hz、100% DPI；高更新率與混合 DPI 仍未量測（R12 的另一部分）。

## 重現方式

```powershell
# 1. 測試 build：在乾淨的 clone 上套用 patch（不要在 D:\fastPDF 套用）
git clone D:\fastPDF lowend; cd lowend
git apply docs/benchmarks/low-end-adapter-override.patch
$env:CARGO_TARGET_DIR = 'D:\fastPDF\target\agent-lowend'
cargo build --profile dist -p fastpdf-app --locked

# 2. 例：入門筆電（D）的 300 頁情境
pwsh -File tools/bench-app/bench-app.ps1 -Preset fastpdf -Exe D:\fastPDF\target\agent-lowend\dist\fastpdf.exe `
    -Pdf fixtures/generated/large-text/dense-300p-times.pdf -Runs 1 -ThreadDetail `
    -AppEnv FASTPDF_TEST_DXGI_ADAPTER=radeon -Affinity 0x5 -Slowdown 6 `
    -ScrollNotches 20 -PageDowns 5 -ZoomSteps 3 -PostIdleSeconds 5
```

`-AppEnv FASTPDF_TEST_DXGI_ADAPTER=radeon` 依 adapter 名稱比對，換成本機內顯名稱中的一段文字（例如 `intel`），或用 `warp`。
