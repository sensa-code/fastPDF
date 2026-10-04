# GPUI on Windows：能力與限制 audit（M0，對應 spec §49 Q3／Q7）

- 日期：2026-10-04
- 範圍：GPUI 本身（不含 pdf-reader-gpui／zpdf 的應用層程式碼，那兩份另有 audit）
- 檢視的版本：
  - **crates.io `gpui` 0.2.2**（pdf-reader-gpui 使用）。原始碼位置：`%USERPROFILE%\.cargo\registry\src\index.crates.io-1949cf8c6b5b557f\gpui-0.2.2\`，以下簡寫為 `gpui-0.2.2/`
  - **zed main `a84689073`（2026-10-03）**。sparse clone 在 `D:\fastPDF\upstream\zed`，以下簡寫為 `zed/`。這個版本的 gpui 已拆成 `gpui`、`gpui_platform`、`gpui_windows` 等 crate
  - **`gpui-pre` 0.3.7**，等於 zed@1a28cff（2026-09-27），是 crates.io 上的 zed snapshot，實測用
- 量測機器：Windows 11 Pro 26200、Ryzen 9 9950X（16C/32T）、128 GB RAM、RTX 5090（接螢幕）＋AMD iGPU（沒接螢幕）、1920×1080@60 Hz 單螢幕、縮放 100%、Rust 1.99.0、MSVC 14.44
- 方法：讀原始碼（下文附 `檔案:行號`）、用 `gh` 唯讀查 issue／PR、實際 build 並執行最小 GPUI 程式（GUI 程式共啟動 5 次，每次都確認 process 已結束）
- studio-memory：fastPDF 專案目前沒有已核准的記憶，本文件內容全部以 repo 與上游原始碼為準

---

## Summary

**一句話結論：** 沒找到會讓 GPUI 不適合做 FastPDF 的 Windows blocker，可以繼續用 GPUI。但有三個條件：
1. 不要用 crates.io 的 `gpui` 0.2.2
2. texture 的生命週期必須由 FastPDF 自己管
3. 部分 Windows 行為要在 FastPDF 這一層繞過，包括 idle 時的 vsync 喚醒，以及按住按鍵時 present 會飢餓

### Q3 回答（細節見文末 Verdict）

| 類別 | 會影響 Reader 的限制 | 嚴重度 | 主要對策 |
|---|---|---|---|
| Windows rendering | **有，可繞過。**<br>• vsync thread 永遠在跑，idle 時仍以每秒 60 次喚醒<br>• 沒有 damage region，每次 present 都是整個視窗<br>• 持續鍵盤輸入時 present 會停好幾秒（#61469，open）<br>• 沒有自訂 GPU texture／surface 的 API<br>• 沒有 HDR／10-bit<br>• 沒有列印 | 中 | • 合併 worker 通知，每 frame 最多 notify 一次<br>• 不做常駐動畫<br>• 必要時 patch vsync／present<br>• 列印自己走 Win32 |
| Scrolling | **沒有阻擋性限制。**<br>• 實測 2000 頁連續捲動穩定 60 fps，每 frame CPU 0.05–1 ms<br>• 0.2.2 沒有觸控板像素捲動與 pinch；main 有（DirectManipulation）<br>• 沒有內建平滑捲動動畫 | 低 | • 用 main<br>• 自己用 canvas 做虛擬化<br>• 平滑捲動自己做 |
| Texture | **有，這是最大的風險。** 實測：<br>• `RenderImage` 不會自動從 GPU atlas 釋放；忘了 `drop_image` 就永久洩漏（400 MiB）<br>• 0.2.2 的 atlas 不會回收 texture 內已釋放的空間：150 MiB 的 tile 吃掉 630 MiB VRAM（main 是 333 MiB）<br>• upload 在 UI thread 上同步執行（64 MiB 要 8–11 ms）<br>• 沒有 mipmap，atlas 也沒有 padding（有邊緣滲色的風險，#62456 open） | 高（可控） | • 自建 `TileTextureManager`：預算、LRU、顯式 `drop_image`<br>• tile 大小 256–512<br>• 每 frame 限制 upload 量<br>• 用 main 版 |
| High-DPI | **沒有阻擋性限制。**<br>• 用 manifest 啟用 PerMonitorV2，`WM_DPICHANGED` 有處理<br>• 0.2.2 有多螢幕、混合縮放的位置 bug，main 已修多數（#62859、#40053、#48902） | 低 | • 用 main<br>• DPI 改變時用新的 scale 重新 render tile |

### Q7 證據摘要

| 項目 | 是不是瓶頸 | 證據 |
|---|---|---|
| GPU upload | 不是吞吐瓶頸，但會造成卡頓 | • 16 個 1 MiB tile 在 paint 中上傳，p50 只要 1.0 ms（main）／2.0 ms（0.2.2）<br>• 單張 64 MiB 影像要 8–11 ms，全部在 UI thread 上<br>• zoom 時一次換掉整個畫面的 tile 才會有風險，要分攤到多個 frame |
| Texture cache | 是，主要風險在記憶體 | • 沒有自動 eviction<br>• 0.2.2 有碎片化 bug<br>• driver 端 commit 大約和 GPU 配置 1:1 成長<br>• 釋放後 VRAM 要等之後再有 render 才回收，idle 時不會下降 |
| GPUI layout | 不是 | • `uniform_list` 每 frame 0.13 ms<br>• 每頁再加 200 個 div（約 600 個元素）是 0.97 ms（p50）／1.7 ms（p90）<br>• 約每元素 1.5 µs |
| Scroll | 不是 | • 2400 px/s 程式化捲動穩定 60 fps，三種做法都是 0 個 >25 ms 的 frame<br>• 捲動時整個 process CPU 3–12% of 1 core |

### 依賴策略

**建議 pin 一個 zed git rev，並設 `default-features = false`。**
- **不建議 crates.io 0.2.2**：它已經 11 個月沒更新，落後 zed main 約 8,167 個 commit。Zed 成員在 2026-09-29 明確建議改從 Zed repo 取用較新的 GPUI（#63471）
- **備案是 `gpui-pre` snapshot**：它在 crates.io 上，但是第三方發佈，使用前要核對來源
- 所有 GPUI 使用集中在 `fastpdf-ui`，讓升級時的破壞範圍可控

### GPUI floor（本機實測，release，最小 hello world）

| 項目 | gpui 0.2.2 | main 系（gpui-pre 0.3.7） |
|---|---|---|
| exe，預設 release | 10.3 MiB | 9.8 MiB |
| exe，LTO＋strip＋`panic=abort` | 4.6 MiB | 6.3 MiB |
| 從 process 建立到第一個 frame | 403–489 ms | 205–228 ms |
| Idle CPU | 0.5–1.1% of 1 core（約佔整機 32 threads 的 0.015–0.035%） | 同左 |
| Idle Private WS（工作管理員預設的「記憶體」） | 約 16 MiB | 約 16 MiB |
| Working Set | 約 67–69 MiB | 約 67–69 MiB |
| Private Bytes（commit） | 約 78–88 MiB | 約 78–88 MiB |

- 其中光是 NVIDIA 的 D3D11 device 就佔 WS 約 27 MiB、commit 約 53 MiB
- 結論：exe < 30 MB 可達成。idle RAM < 50 MB 要看用哪個指標：Private WS 可達成，WS 和 commit 已經超過

---

## Versions & Dependency Strategy

### 版本盤點（2026-10-04，crates.io API、`gh api` 查得）

| 套件 | 最新版 | 發佈日 | 發佈節奏／狀態 |
|---|---|---|---|
| `gpui`（crates.io） | 0.2.2 | 2025-10-22 | • 0.2.0（10-09）、0.2.1（10-14）、0.2.2（10-22）連發三版後就停了，已約 11.5 個月沒有新版<br>• 0.2.2 來自 zed commit `69e2130`（`.cargo_vcs_info.json` 標記 `"dirty": true`）<br>• `gh api compare` 顯示 main 已領先約 8,167 個 commit<br>• PR #39832 "Publish gpui in CI" 在 2025-11-10 未合併就關閉 |
| zed main | `a84689073` | 2026-10-03 | • 每天都有 commit<br>• `crates/gpui/Cargo.toml` 的版本號**仍是 0.2.2**，無法用版本號區分<br>• gpui 已拆成 `gpui`、`gpui_platform`、`gpui_windows`、`gpui_wgpu`、`gpui_macos`…（#49277，2026-02-19） |
| `gpui_platform`／`gpui_windows` | 不在 crates.io | — | `zed/crates/gpui/README.md` 寫 `gpui_platform = { version = "*" }`，但 crates.io 上查無此 crate |
| `gpui-pre`（還有 `gpui-pre-platform`、`gpui-pre-windows` 等 20 多個 crate） | 0.3.7 | 2026-09-28 | • 2026-09-03 起大約每週一版，0.3.0 → 0.3.7<br>• 描述是 "gpui-pre snapshot of zed@1a28cff"<br>• 發佈者 `huacnlee`，和 gpui-component 是同一人<br>• 整個家族之間都是 `=0.3.7` 精確 pin |
| `gpui-component` | 0.7.0 | 2026-09-28 | • 0.5.1（2026-02-05）是**最後一個**依賴 `gpui ^0.2.2` 的版本<br>• 0.6.0（2026-09-03）起改依賴 `gpui-pre`<br>• repo 已改名為 `longbridge/gpui-kit` |

上游使用狀況：
- pdf-reader-gpui：`gpui = "0.2.2"` 加 `gpui-component = "0.5.1"`。wasm target 另走 git（`upstream/pdf-reader-gpui/Cargo.toml`）
- zpdf-viewer-gpui：zed git rev `d989c7c5`（2026-06-09）。這個 rev 已包含 #58874 atlas 修正和 #51354 pinch（`gh api compare` 驗證）
- 生態圈參考：crates.io 上另有 `gpui-pdf` 0.6.1（MIT，hayro 加 `gpui-pre`，2026-09 發佈，下載數 41）。只是查到有這個東西，沒有評估成熟度
- Zed 成員 reflectronic 在 #63471（2026-09-29）說 "The gpui 0.2.2 crate is quite old and missing many recent improvements"。他也提到官方打算更頻繁地發佈 gpui crate，但目前建議使用者直接從 Zed repo 取用較新的 GPUI

### 0.2.2 之後只在 main 才有、且和 PDF reader 有關的修正（節錄）

| PR | 合併日 | 內容 | 對 FastPDF |
|---|---|---|---|
| #58874 | 2026-06-08 | Free atlas tile space when removing tiles | tile 換進換出時 VRAM 不再暴增（實測 630 → 333 MiB） |
| #48282 | 2026-02-03 | Optimize resource upload in D3D11 | tile upload 快約 2 倍（實測 p50 2.05 → 1.00 ms） |
| #45369 | 2025-12-19 | VRR optimization 只在高頻輸入時啟用 | 0.2.2 是任何輸入後都整窗重繪 1 秒（#32588） |
| #52970、#62628 | 2026-04、08 | 非焦點視窗限制到約 30 fps，且可設定 | 省電 |
| #64623、#55878 | 2026-09、05 | atlas texture 已釋放或 device 復原後不再 panic | 搭配 `drop_image` 時更安全 |
| #51354 | 2026-03-28 | Windows pinch（DirectManipulation） | 觸控板 pinch zoom、像素級捲動 |
| #62859、#40053、#48902、#55630 | 2025-11 ~ 2026-08 | 混合 DPI 多螢幕的視窗位置、DPI 改變時的最大化尺寸、`WM_DISPLAYCHANGE`、螢幕消失時的 panic | High-DPI／多螢幕 |
| #41259、#46079 | 2025-11、2026-01 | IME 與國際鍵盤配置、IME 候選窗位置 | 搜尋框輸入中文 |
| #55065 | 2026-04-28 | Fix process teardown deadlock on Windows | 關閉程式時不會卡住 |
| #64250 | 2026-09-15 | Windows dialog 移出 foreground thread | 檔案對話框不會擋住 UI |
| （API）#49277 等 | 2026-02 起 | gpui 拆 crate；`Application::new()` 改成 `gpui_platform::application()`；`paint_image` 多了 `image_bounds` 參數 | 升級時要改 code，API 變動是常態 |

### 選項比較

| 選項 | 優點 | 風險 |
|---|---|---|
| A. crates.io `gpui` 0.2.2 | • 不可變，build 最單純<br>• 和 pdf-reader-gpui 相同 | • 上表所有修正都沒有，包括 atlas 碎片化：實測 VRAM 630 vs 333 MiB，+89%<br>• 啟動慢約 2 倍（實測）<br>• 生態圈（gpui-component）已經離開 0.2.2<br>• Zed 官方也不推薦 |
| B. pin zed git rev（**建議**） | • 官方來源，修正最完整<br>• 可以用 `[patch]` 打自己的小修正 | • API 經常變，每次升級都要改 code<br>• 首次抓取大：本機 `~/.cargo/git/db/zed-*` 約 351 MB，GitHub repo 約 531 MB<br>• 要自己決定升級時機 |
| C. `gpui-pre` snapshot | • 在 crates.io 上，不用 clone zed<br>• 版本固定，大約每週更新 | • 第三方重新發佈（非 Zed Industries），有供應鏈信任問題，要 diff 核對 zed@sha<br>• 發佈者可能停更<br>• 和 gpui-component 綁在一起 |
| D. vendor 一份到 repo | • 完全可控，可以深度修改（vsync、atlas、external texture） | • 維護成本高，`gpui` 加 `gpui_windows` 加相依 crate 是好幾萬行 |

**建議做法：**
1. 採 **B**：在 `[workspace.dependencies]` 單點宣告 `gpui` 和 `gpui_platform`，兩者用同一個 `rev`，並設 `default-features = false`
   - Windows 只需要 gpui 的 `windows-manifest`；`gpui_platform` 在 Windows 不需要任何 feature
   - 0.2.2 預設的 `wayland`／`x11` feature 會在 Windows 上也編進 `blade-graphics`、`naga`
2. 升級節奏：每 4–8 週一次，或遇到需要的修正時才升。升級前跑 FastPDF 自己的 GPUI probe（atlas churn／leak、idle CPU、TTW、scroll），再跑 `fastpdf-bench`
3. GPUI 型別只出現在 `fastpdf-ui`，`core`／`render`／`engine` 一律不依賴 gpui（spec §4 本來就這樣要求）。這樣 rev 升級的破壞範圍只在一個 crate
4. 若 CI 不想 clone zed，C 可以當備案。條件是核對 snapshot 與 zed sha 一致
5. 參考 Zed 自己的 Windows build 設定（`zed/.cargo/config.toml`）：`-C target-feature=+crt-static`、`--cfg windows_slim_errors`
   - 實測預設 build 的 exe 會 import `VCRUNTIME140.dll`（dumpbin `/dependents`），用 `crt-static` 可以免掉 VC++ redist
6. 建置需求：release build 會用 Windows SDK 的 `fxc.exe` 編 HLSL。可用 `GPUI_FXC_PATH` 指定位置（`gpui-0.2.2/build.rs` 的 `find_fxc_compiler`）。CI image 必須裝 Windows SDK

---

## Windows Backend

下表左邊是 0.2.2 的證據，「main」欄只列出有差異的地方。

| 項目 | 實作 | 證據 | 對 FastPDF 的意義 |
|---|---|---|---|
| Renderer | **Direct3D 11**<br>• 要求 FL 11.1、11.0 或 10.1，加上 structured buffer 支援<br>• HLSL shader（quad、shadow、path、mono／poly sprite…） | `gpui-0.2.2/src/platform/windows/directx_renderer.rs`、`directx_devices.rs:146-199` | 一般 GPU、WARP 都能跑。沒有 D3D12／Vulkan |
| Swapchain | • DXGI flip-sequential、3 buffers、`B8G8R8A8_UNORM`<br>• 預設 `CreateSwapChainForComposition` 搭配 **DirectComposition** visual，視窗帶 `WS_EX_NOREDIRECTIONBITMAP`<br>• 設環境變數 `GPUI_DISABLE_DIRECT_COMPOSITION` 會改走 `CreateSwapChainForHwnd`<br>• `Present(0, 0)`，sync interval 為 0，由 vsync thread 負責節拍 | `directx_renderer.rs:29-30,204-206,1021-1075,1389`；`window.rs:410`；main 一樣（`zed/crates/gpui_windows/src/directx_renderer.rs:245-253`） | • 沒有 tearing／VRR 控制<br>• NVIDIA overlay 開著時可能讓 DComp 視窗變透明（#55654），可用上述環境變數繞過 |
| VSync／frame 節拍 | • 專屬 `VSyncProvider` thread 無限迴圈呼叫 `DwmFlush()`，每次對所有視窗呼叫 `RedrawWindow(RDW_INVALIDATE)`<br>• UI thread 收到 `WM_PAINT` 後呼叫 request_frame；只有 dirty 時才 draw | `vsync.rs:36-55`、`platform.rs:240-272`；main：`zed/crates/gpui_windows/src/platform.rs:433-490` | 高更新率跟著 DWM 合成頻率走（144／240 Hz 會等比增加喚醒次數）。見 Idle Behavior |
| 輸入後重畫 | • 0.2.2：任何輸入後 1 秒內，每個 vsync 都整窗重畫並 present<br>• main：只有「造成 invalidation 的高頻輸入」才會持續 present；非焦點視窗約 30 fps | `gpui-0.2.2/src/window.rs:1037-1041`；`zed/crates/gpui/src/window.rs:1764,1826,5612-5615` | 0.2.2 只要移動滑鼠就會耗 GPU（#32588，已在 main 修正） |
| GPU 選擇 | • 依 `EnumAdapters` 順序，取第一個能建 D3D11 device 的 adapter<br>• 沒有 `EnumAdapterByGpuPreference`，也沒有讓 app 指定的 API<br>• 0.2.2 在檢查與建立時**建了兩次 device**，main 只建一次 | `gpui-0.2.2/.../directx_devices.rs:61,123-140`；`zed/crates/gpui_windows/src/directx_devices.rs:106-140` | • 混合顯卡由 Windows「圖形設定」決定（#39264、#36798）<br>• 本機 adapter0 = RTX 5090（`d3dbase` 實測）<br>• D3D11 device 建立一次約 115 ms，所以 0.2.2 啟動較慢 |
| Device lost | • vsync thread 偵測 `GetDeviceRemovedReason`，重建 device<br>• **atlas 會整個清空**，下次 paint 再從 `Arc<RenderImage>` 重新上傳 | `platform.rs:1023`、`directx_renderer.rs:209-284`、`directx_atlas.rs:55-66` | • 顯示中的 tile 必須保留 CPU 端資料<br>• driver 更新時若復原失敗會 fail-fast（#52085） |
| DPI | • 內嵌 manifest 啟用 `PerMonitorV2`（gpui 的 feature `windows-manifest`）<br>• `WM_DPICHANGED` 時更新 `scale_factor`，再用建議的 rect 呼叫 `SetWindowPos` | `gpui-0.2.2/resources/windows/gpui.manifest.xml`、`build.rs`（`embed_resource`）、`events.rs:775-818` | • FastPDF 若要自己的 manifest（例如 longPathAware），要關掉 gpui 的 `windows-manifest` 並合併內容，否則會有兩個 `RT_MANIFEST`<br>• DPI 改變時 tile 要用新 scale 重新 render |
| 多螢幕 | `EnumDisplayMonitors`、`GetDpiForMonitor`、`WM_DISPLAYCHANGE` | `display.rs:202,252`、`events.rs:828-850` | 0.2.2 在混合縮放下有位置 bug（#59526 open、#48927）；main 修了 #62859、#48902 |
| Dark mode | • 用 `UISettings`（WinRT）讀系統外觀<br>• `WM_SETTINGCHANGE` "ImmersiveColorSet" 時觸發外觀變更<br>• 標題列用 `DWMWA_USE_IMMERSIVE_DARK_MODE`<br>• main 另有 Mica／MicaAlt（#41842） | `util.rs:159-178`、`platform.rs:1018`；`zed/crates/gpui_windows/src/events.rs:1229-1240` | 可用 `window.appearance()` 加 observer |
| Drag & drop | OLE `IDropTarget`（`RegisterDragDrop`），產生 `FileDropEvent::{Entered,Pending,Submit,Exited}`，路徑包在 `ExternalPaths` | `window.rs:881-1000,1245` | 拖放 PDF 開檔直接可用（`on_drop::<ExternalPaths>`） |
| 檔案對話框 | 用 `IFileOpenDialog`／`IFileSaveDialog`；`PathPromptOptions` 只有 files、directories、multiple、prompt | `platform.rs:902-990`、`gpui-0.2.2/src/platform.rs:1330-1339`、`zed/crates/gpui/src/platform.rs:2749` | **沒有副檔名篩選（`*.pdf`）**。要嘛自己呼叫 `IFileOpenDialog::SetFileTypes`（HWND 可從 `impl HasWindowHandle for Window` 取得：`gpui-0.2.2/src/window.rs:4845`），要嘛用 `rfd`（pdf-reader-gpui 用這個） |
| IME | IMM32：`WM_IME_STARTCOMPOSITION`／`WM_IME_COMPOSITION` 加 `ImmSetCompositionWindow`，沒有 TSF | `events.rs:629-700` | 注音、倉頡等可以在搜尋框輸入；main 另修了候選窗位置（#46079） |
| Clipboard | `CF_UNICODETEXT`、註冊格式 "PNG"、`CF_HDROP` | `clipboard.rs:12-44,96-100` | 複製文字 OK |
| 滾輪／觸控板 | • 0.2.2：`WM_MOUSEWHEEL`／`WM_MOUSEHWHEEL`，轉成 `ScrollDelta::Lines`，依系統「每次捲動行數」，沒有 phase<br>• main：加上 **DirectManipulation**，觸控板會給 `ScrollDelta::Pixels`（含 phase、慣性）與 `PinchEvent` | `events.rs:520-617`；`zed/crates/gpui_windows/src/direct_manipulation.rs:40-50,175-343` | Ctrl＋滾輪由 app 依 `modifiers.control` 自己處理 zoom |
| 觸控螢幕 | 沒有 `WM_POINTER` 處理（PR #64205 open） | grep 結果 | 觸控螢幕只有滑鼠模擬 |
| 無障礙 | main 有 accesskit（UIA，import `uiautomationcore.dll`）；0.2.2 沒有 | dumpbin `/dependents` | — |
| 列印 | **沒有** | 見 Printing | 要自己寫 |
| 執行緒模型 | • 背景 executor 用 Windows thread pool：0.2.2 是 WinRT `ThreadPool::RunWithPriorityAsync`，main 是 `TrySubmitThreadpoolCallback` 加優先度<br>• foreground 用 posted message | `gpui-0.2.2/src/platform/windows/dispatcher.rs:55-66`；`zed/crates/gpui_windows/src/dispatcher.rs:54-113` | thread 數不受 app 控制（idle 實測 113 個，含 NVIDIA driver threads）。**PDF rasterization 不要丟給 `background_executor`**，用 FastPDF 自己的有限 worker pool（spec §18） |

---

## Texture & Image Pipeline

### 路徑

```text
FastPDF worker（RGBA 或 BGRA pixmap）
  → RGBA→BGRA（必要時）
  → RenderImage::new(SmallVec<[image::Frame;1]>)       每次建構都配一個新的 ImageId
  → Arc<RenderImage>
  → img(ImageSource::Render) 或 canvas 裡的 window.paint_image(...)
  → sprite_atlas.get_or_insert_with(RenderImageParams{image_id, frame})
       ├─ 命中：重用 AtlasTile
       └─ 沒命中：etagere 配置 → ID3D11DeviceContext::UpdateSubresource（同步、UI thread、在 paint 階段）
  → PolychromeSprite → instanced draw（bilinear sampler）
```

- `RenderImage` 的資料必須是 **BGRA、straight alpha**（`assets.rs:41` 註解寫明 BGRA；blend 用 `SRC_ALPHA`／`INV_SRC_ALPHA`，見 `directx_renderer.rs:1235-1252`）
- `ImageSource::Image`／`Resource` 由 GPUI 自己解碼並轉 BGRA（`elements/img.rs:640-702`）。PDF tile 走 `Render`，沒有解碼成本
- `paint_image`：
  - 0.2.2 簽章是 `(bounds, corner_radii, data, frame_index, grayscale)`，會把 origin floor、size ceil 到 device pixel（`gpui-0.2.2/src/window.rs:3129-3175`）
  - main 多了 `image_bounds`，可以只畫影像的一部分（`zed/crates/gpui/src/window.rs:4919-5000`）

### Atlas 規格（Windows）

| 項目 | 值 | 證據 |
|---|---|---|
| texture 種類 | `D3D11_USAGE_DEFAULT`、`B8G8R8A8_UNORM`（polychrome）、`MipLevels: 1` | `gpui-0.2.2/.../directx_atlas.rs:153-199` |
| texture 大小 | `max(1024², 影像大小)`，上限寫死 16384²（FL10.1 硬體實際上限 8192，GPUI 沒有依 feature level 調整） | 同上 `:153-163`；main `zed/crates/gpui_windows/src/directx_atlas.rs:164-175` |
| 配置器 | `etagere::BucketedAtlasAllocator`。依 etagere 文件，bucket 裡全部 item 都釋放後才會回收整個 bucket | `directx_atlas.rs:222` |
| padding／取樣 | `padding: 0`；sampler `MIN_MAG_MIP_LINEAR` 加 `WRAP`；shader 的 UV 沒有內縮半個 texel | `directx_atlas.rs:255`（main `:273`）、`directx_renderer.rs:829-835`（main `:1044-1049`）、`shaders.hlsl:246-250` |
| 範圍 | 每個視窗各有一個 atlas。`App::drop_image` 會對所有視窗都 remove | `directx_renderer.rs:138`、`gpui-0.2.2/src/app.rs:2071-2083` |
| upload | `UpdateSubresource`，同步、在呼叫 `paint_image` 的那個 frame 裡執行 | `directx_atlas.rs:262-282`（main `:304`） |
| 釋放 | 只有 `Window::drop_image`、`App::drop_image` 會釋放。`RenderImage` 沒有 `Drop` hook，`RetainAllImageCache` 只管 `Resource`／`Image` 來源 | `gpui-0.2.2/src/window.rs:3198-3210`；`zed/crates/gpui/src/elements/image_cache.rs:222-280` |
| **0.2.2 的 bug** | `remove()` 只遞減 texture 的參考計數，**不會呼叫 `allocator.deallocate`**。同一張 texture 只要還有任何一個 tile 活著，已釋放的空間就永遠不會被重用 | `gpui-0.2.2/.../directx_atlas.rs:95-121`；main 修正在 `zed/crates/gpui_windows/src/directx_atlas.rs:122`（#58874） |
| 自訂 texture | **沒有。** `paint_surface` 只在 macOS／iOS 有（CVPixelBuffer），Windows 的 `draw_surfaces` 是空函式，也沒有公開 D3D11 device | `gpui-0.2.2/.../directx_renderer.rs:572-577`；`zed/crates/gpui_windows/src/directx_renderer.rs:816-821`；`zed/crates/gpui/src/window.rs:5020`；gpui-kit #3168（open）、zed PR #64061（updatable textures，open） |

### 實測：atlas 的生命週期與記憶體

測試程式是自寫的 `probe`：1024×768 視窗，tile 512×512 BGRA（每張 1 MiB），單次啟動依序跑各個 phase。外部每 200 ms 取樣 WS／Private，每約 1.1 s 取樣一次 GPU 與 Private WS 的效能計數器。

| Phase | 內容 | gpui 0.2.2 | main 系（gpui-pre 0.3.7） |
|---|---|---|---|
| Hello | idle 基準 | Private WS 16.9 MiB、GPU dedicated 26 MiB、Private 85 MiB | 15.5、23、88 MiB |
| BigImage | 一張 4096² RGBA（64 MiB） | 建立 10.6 ms，**首次 paint（含 upload）8.0 ms**；Private WS +128 MiB、GPU dedicated +64、GPU shared +64、Private +193 MiB | 建立 13.5 ms、**首次 paint 11.1 ms**；增量相同 |
| BigDropped | `drop_image` 之後丟掉 `Arc` | Private WS −64（CPU 那份釋放了）。**GPU dedicated／shared 在 idle 時沒有下降** | 一樣 |
| ChurnDrop | 每 frame 新增 16 個 tile，上一 frame 的 16 個做 `drop_image`，持續 5 s | 60 fps（300 frames），paint p50／p90／max = **2.05／3.35／5.67 ms**；GPU dedicated 穩定在 74 MiB，沒有洩漏 | 60 fps，**1.00／1.40／3.09 ms**，77 MiB |
| ChurnPartial | 每 frame 新增 4 個，留 1 個、`drop_image` 3 個，150 frames 後共留 150 MiB | **GPU dedicated 630 MiB（+556）**、Private 879 MiB | **333 MiB（+256）**、Private 582 MiB |
| PartialReleased | 留下的 150 個全部 `drop_image` | Private WS 回到 46 MiB；GPU dedicated **要等下一次 render 才釋放**（實測 idle 6 s 都沒降） | 一樣 |
| ChurnLeak | 新增 400 個 tile，**不呼叫 `drop_image`**，直接丟 `Arc` | GPU dedicated 停在 430 MiB，**永久洩漏**；Private WS 只有 27 MiB，工作管理員的「記憶體」欄看不出來；Private（commit）約 500 MiB | 433 MiB，結果相同 |

解讀：
1. **沒有自動 eviction。** 一旦遺失 `Arc` 又沒先 `drop_image`，那張 tile 就無法再釋放，直到視窗關閉或 device lost。#56667、#35894、#39914 都是同類問題
   - 上游實例：zpdf-viewer-gpui 用 `page_cache.retain`／`remove` 淘汰頁面，但沒有任何地方呼叫 `drop_image`（`upstream/zpdf/crates/zpdf-viewer-gpui/src/viewer.rs:996-1008,1179`；全專案 grep `drop_image` 結果為 0），翻過的頁會一直累積在 VRAM
   - pdf-reader-gpui 有在 `strong_count == 1` 時呼叫 `drop_image`（`upstream/pdf-reader-gpui/src/lib.rs:538-550`），是正確的模式
2. **0.2.2 有碎片化洩漏。** 存活時間不一的 tile 混在同一張 1024² texture 時，150 MiB 的存活資料吃掉 556 MiB VRAM（約 3.7 倍）
   - main 修正後是 256 MiB（約 1.7 倍）。剩下的倍數來自 bucketed allocator 的粒度
   - 結論：tile 大小要固定，最好剛好整除 1024，例如 256 或 512
3. **host 記憶體會被算兩到三次：**
   - CPU 端的 `RenderImage`（必須保留，device lost 時要重傳）
   - driver 的 staging（GPU shared）。大小約等於近期最大的一次 upload，idle 時不會釋放：上傳一張 64 MiB 影像後 shared 停在 64 MiB，之後小 tile 持續 churn 才降到約 33 MiB
   - NVIDIA 下的 Private Bytes（commit）大約和 VRAM 配置 1:1 成長，而且回落很慢。這是 driver 行為，不是 GPUI 本身；Private WS 不受影響
   - 參考 #56349（open）：Windows glyph atlas 逐 glyph 呼叫 `UpdateSubresource` 也造成 Private Bytes 大幅成長
4. **VRAM 是延遲回收的。** D3D11 的物件要等 context 再送出工作後才真正銷毀，所以「evict 完就 idle」時工作管理員看到的 GPU 記憶體會停在高點

### Upload 與轉換成本

| 項目 | 實測 | 說明 |
|---|---|---|
| 16 × 1 MiB tile 上傳（含配置） | p50 1.00 ms（main）、2.05 ms（0.2.2） | 每 MiB 約 0.06–0.13 ms；CPU 端 memcpy 到 driver |
| 64 MiB 單張影像 | 8.0–11.1 ms，在 UI thread 上 | 超過半個 60 Hz frame |
| RGBA→BGRA，1 MiB tile | 0.038 ms（u32 shuffle）／0.051 ms（`chunks.swap`） | `swapbench`，單執行緒，19–26 GiB/s；64 MiB 要 3.1–3.9 ms；可以在 worker 上做 |
| 配置並填滿 1 MiB buffer | 0.16 ms | `RenderImage` 包裝本身幾乎零成本，因為 `RgbaImage::from_raw` 不複製 |

### 建議的 FastPDF texture 設計（給 M5／M6）

1. **`TileTextureManager`（放在 `fastpdf-ui`）** 持有所有 `Arc<RenderImage>`
   - 用 byte 預算管理（GPU，例如 128–256 MiB）與 LRU
   - 淘汰一律呼叫 `window.drop_image(arc)`，而且是在「該 tile 已經不在最近一次 scene 裡」之後
   - 0.2.2 在 present 舊 scene 時若碰到已釋放的 texture 會 panic，main #64623 已修
2. **tile 大小固定為 256 或 512 device px**，避免單張巨圖。這樣能限制 staging 大小與單次 upload 卡頓，也能減少碎片
3. **每 frame 設 upload 預算**（例如 ≤ 8–16 MiB，約 ≤ 1–2 ms）。zoom 改變時先放大顯示舊的低解析 texture（spec §13），新的 tile 分幾個 frame 上傳
4. **邊緣滲色**：
   - tile 在整數 device pixel 1:1 顯示時不會發生（UV 落在 texel 中心）
   - 暫時縮放（progressive zoom）時，atlas 沒有 padding、又是線性取樣，鄰接 tile 會出現細線（#62456 open，修正 PR #62557 open）
   - 對策：render tile 時多加 1–2 px 的 gutter 並裁切，或者暫時縮放時改畫單張頁面級的低解析影像
5. **沒有 mipmap**：縮圖要直接用目標大小 render，不要把大 tile 縮小顯示（會 aliasing；放大則會模糊，#63729）
6. 若未來要讓 zpdf 的 GPU backend 直接產生 texture，GPUI 沒有掛勾點，只有三條路：
   1. 讀回 CPU 再走 atlas，有 copy 成本
   2. fork `gpui_windows`，新增 external texture primitive（共用 D3D11 device 或 shared handle），中等工作量
   3. 另開子 HWND 或 DComp visual，但會有 airspace 問題，選單和 popup 蓋不上去，**不建議**

---

## Scrolling & Layout

### 虛擬化選項

| 方式 | 特性 | 證據 | 適合 PDF 嗎 |
|---|---|---|---|
| `uniform_list` | 只量測第一個 item，其餘依算術排列；只 render 可見的 item | `gpui-0.2.2/src/elements/uniform_list.rs:1-40` | 頁面尺寸都相同時可用 |
| `list` + `ListState` | 可變高度，用 `SumTree` 記錄高度；高度是 render 到時才量（捲軸會不準），除非 `measure_all()`（會 render 全部 item） | `elements/list.rs:1-8,216-244` | 混合頁面尺寸時捲軸會跳動，不理想 |
| 自訂 `Element`／`canvas` | PDF 的頁面尺寸已知：用 prefix sum 加二分搜尋算出可見頁，再直接 `paint_quad`＋`paint_image` | `canvas()`（`zed/crates/gpui/src/elements/canvas.rs:10-19`） | **建議**：最省、最精確，也能自己決定 tile 和 overlay |

### 實測：2000 頁程式化捲動（`scrollbench`，main 系，1024×768，60 Hz）

設定：每頁 816×1056（Letter @96 dpi），每頁 6 個 512² tile（從 24 張的池子取，沒有 upload 成本），捲動速度 2400 px/s。每個做法跑 6 秒。

| 做法 | frames | 每 frame CPU：render → paint 結束（p50／p90／max） | 間隔 p50／max | >25 ms 的 frame | 該 phase 的 process CPU |
|---|---|---|---|---|---|
| `uniform_list`，每頁是 div 加 6 個 `img()` | 361 | 0.130／0.163／1.84 ms | 16.65／20.6 ms | 0 | 3.8% of 1 core |
| 單一 `canvas`（`paint_quad` 加 `paint_image`） | 361 | 0.054／0.098／0.19 ms | 16.67／18.2 ms | 0 | 3.2% |
| `uniform_list`，每頁再加 200 個 absolute div（約 600 個元素） | 360 | 0.974／1.713／3.58 ms | 16.67／19.1 ms | 0 | 11.8% |

說明：「render → paint 結束」量的是 view `render()` 開始到最後一個元素 paint 完成，不含 renderer 的 GPU 送出與 `Present`。process CPU 是外部量到的，有包含這些。

結論：
- GPUI 的 layout 與 paint 在 PDF reader 的元素數量下不是瓶頸
- 但成本約每元素 1.5 µs，會隨元素數量線性成長。**搜尋 highlight、文字選取、連結區域要在 canvas 裡用 `paint_quad` 一次畫完**，不要每個字或每個 rect 都建一個 div。數千個 div 就會吃掉好幾 ms
- 另外 #50392（open）指出：任何動畫元素（例如 spinner）都會讓整個視窗每 frame 重跑 layout 與 repaint。載入指示要用靜態圖示，或把動畫限縮在短時間內

### 輸入與平滑捲動

- **滾輪**：送出的是 `ScrollDelta::Lines`，沒有平滑動畫（`events.rs:520-575`）。FastPDF 要自己做 easing：用 `request_animation_frame`，而且只在動畫期間才要求
- 已知問題：系統「一次捲動一個畫面」（`WHEEL_PAGESCROLL`）時會跳到頂端或底端（#39513，被 stale 關閉，未修）
- **Ctrl＋滾輪**：`ScrollWheelEvent.modifiers.control` 交給 app 自己處理
- **精密觸控板**：
  - 0.2.2 只能拿到 driver 模擬的小量 `WM_MOUSEWHEEL`，沒有 phase、沒有 pinch
  - main 透過 DirectManipulation 提供像素 delta、phase 與慣性，以及 `PinchEvent`（#51354）
  - 觸控板慣性捲動卡頓（#39170）被 stale 關閉，未修
- **鍵盤（PageDown、方向鍵連發）**：**#61469（open，S3）**
  - 機制：`WM_PAINT` 是最低優先的訊息。持續輸入、再加上背景 notify 時，message queue 永遠不會空下來，present 可能停 5–15 秒；frame 有在 render，只是沒有 present
  - FastPDF 的高風險情境：按住 PageDown，同時 worker 持續回傳 tile
  - 對策：
    - worker 的完成通知要合併：只設一個原子 dirty flag，每個 frame 最多 notify 一次，不要每完成一個 tile 就 post 一個 foreground task
    - 鍵盤連發要節流
    - 必要時 patch GPUI，讓 key dispatch 之後也 present（相關 PR #61632／#63489 未合併）

---

## Idle Behavior

### 機制

視窗存在期間，`VSyncProvider` thread 會無限迴圈。每個 vblank 都 `DwmFlush()`，然後對所有視窗 `RedrawWindow(RDW_INVALIDATE)`。UI thread 處理 `WM_PAINT` 時呼叫 request_frame，沒有 dirty 就不 draw。因此 idle 時每秒仍有 refresh-rate 次（本機 60 次）的雙 thread 喚醒。
- 證據：`gpui-0.2.2/src/platform/windows/platform.rs:240-272`；main `zed/crates/gpui_windows/src/platform.rs:449-490`
- PR #63182（"Make frame scheduling demand-driven"）的說明也確認這點：即使沒有任何視窗要求 frame，vsync thread 每次都會把所有視窗標記為需要重畫。該 PR 在 2026-09-17 被關閉，維護者表示內部有另一套重構計畫

### 實測（Windows 的 CPU time 計數粒度是 15.6 ms，10 秒窗口的解析度約 ±0.16%）

| 狀態 | gpui 0.2.2 | main 系 |
|---|---|---|
| 可見、idle（hello，10 s） | 78 ms → 0.79% of 1 core | 78 ms → 0.78% |
| 可見、idle（probe 的 Hello phase，10.3 s） | 109 ms → 1.06% | 47 ms → 0.46% |
| 最小化 idle（5.3 s） | 2.09% | 1.16% |
| 大量操作之後回到 idle（多個 phase） | 0.7–1.1% | 0.3–1.4% |

### 相關 issue

| Issue | 狀態 | 說明 |
|---|---|---|
| #63184 | open | 成本隨更新率線性成長：240 Hz 是 60 Hz 的 4 倍 |
| #58048 | open | 螢幕關閉後仍持續重畫，筆電風扇變吵 |
| #63438 | open PR | 螢幕關閉時停止 render |
| #15166 | open | 沒有 damage region，每次 present 都是整個視窗 |

### 對 spec KPI「Idle CPU 接近 0%」的意義

- GPUI 的下限約是 0.5–1% of 1 core（32 threads 機器上約整機的 0.015–0.035%，工作管理員會顯示 0%）。高更新率螢幕會等比放大（這是推論，本機只有 60 Hz，沒有實測）
- 要真正歸零，需要 patch vsync thread，讓它在沒有視窗要求 frame 時 park。#63182 的做法可以參考，改動約百行內
- FastPDF 這一層應做到：
  - 不留任何常駐動畫或 timer
  - 背景工作完成後不要 notify 沒有變動的 view
  - 視窗最小化或被遮住時暫停 prefetch（main 有 `Window::is_visible`／`on_visibility_changed`，#64107）

---

## Printing

GPUI 完全沒有列印 API。在 0.2.2 與 main 的 `platform.rs` 和 `gpui_windows` 裡 grep `print`、`StartDoc`、`PrintDlg`，結果都是 0。FastPDF 要自己走 Win32。需要的 HWND 從 `raw_window_handle::HasWindowHandle for Window` 取得（`gpui-0.2.2/src/window.rs:4845`、`zed/crates/gpui/src/window.rs:7434`）。

| 做法 | 內容 | 優點 | 缺點 |
|---|---|---|---|
| **GDI raster（V0.1 建議）** | `PrintDlgExW` → `CreateDC` → `StartDoc`／`StartPage` → 依印表機 DPI 分 band 呼叫 `PdfEngine::render`（每條例如 ≤ 16 MiB）→ `StretchDIBits` → `EndPage`／`EndDoc` | • 所有印表機都支援<br>• 直接重用 PdfEngine 的 raster API 與 tile 機制<br>• 記憶體可控（band），可以在 worker thread 上執行（對話框在 UI thread） | • 只能輸出點陣，spool 檔大（A4 600 dpi 未壓縮約 100 MB／頁，要靠 banding）<br>• 文字不是向量 |
| Direct2D + XPS Print Document Package | `IPrintDocumentPackageTargetFactory` 加 `ID2D1PrintControl`，每頁一個 D2D command list | 向量輸出，品質最好 | • engine 要能輸出 D2D 繪圖指令（或 display list → D2D），工作量大<br>• 適合 M7 之後 |
| WinRT `PrintManager`（`IPrintManagerInterop::ShowPrintUIForWindowAsync`） | Windows 11 的新式列印 UI，含預覽 | 體驗最好 | • COM 介面多，要實作 `IPrintDocumentSource`<br>• 和 GDI raster 一樣需要 raster 或 D2D 內容 |

建議：
- V0.1 用 GDI raster，加上 banding、背景 thread、可取消。spec §8 允許延後列印，架構上只要保留「依指定 DPI render 頁面並分 band」的 PdfEngine 能力就好
- 之後若 engine 提供 display list，再評估 D2D／XPS 的向量路徑

---

## Known Issues

以下是 2026-10-04 用 `gh` 唯讀查詢 zed-industries/zed 與 longbridge/gpui-kit（原 gpui-component）整理出來的清單。
- 標題、狀態有 ✔ 的列，已由本文件作者另外用 `gh issue view`／`gh pr view` 逐筆核對
- 「GPUI 層」指問題在框架本身，不在 Zed editor 的功能
- zed repo 有 1,144 個 `platform:windows` issue（102 個 open），其中同時標 `area:gpui` 的只有 39 個（8 個 open）

| Repo | # | 標題（原文） | 狀態 | 類別 | GPUI 層 | 對 PDF reader 的影響 |
|---|---|---|---|---|---|---|
| zed | 61469 ✔ | Window stops presenting for seconds under sustained keyboard input (WM_PAINT starvation; dispatch_key_event draws without presenting) | open（S3） | rendering | 是 | 按住 PageDown 加上 tile 陸續回傳時，畫面可能凍結好幾秒 |
| zed | 56667 ✔ | (gpui) Image::remove_asset does not release rendered image atlas resources on Windows | closed 2026-09-11（stale，未修） | texture-memory | 是 | asset 型影像不會釋放 atlas；tile 要用 `RenderImage` 加 `drop_image` |
| zed | 54659 | MetalAtlas::bytes_per_pixel "not implemented" on macOS after long session with many images | open（部分由 #58874 修正） | texture-memory | 是（共用 atlas 邏輯） | 證實 0.2.2 的 atlas 長時間 churn 會出問題 |
| zed | 56349 ✔ | (gpui) Windows DirectX glyph atlas performs per-glyph UpdateSubresource calls causing large Private Bytes growth | open | texture-memory | 是 | 小量、頻繁的 upload 會讓 driver commit 膨脹（ARM64／Adreno 上 +150 MB） |
| zed | 62456 ✔ | GPUI: RenderImage leaves visible frame/border on same-color background | open（PR #62557 open） | rendering | 是 | atlas 邊緣滲色，tile 接縫會出現細線 |
| zed | 63729 ✔ | Low resolution images are blurry at high zoom levels | open | rendering | 是 | 只有線性取樣；放大一律要重新 rasterize |
| zed | 63747 | gpui: `img` with relative size ignores explicit `aspect_ratio`, filling the `Auto` axis from intrinsic size | open（PR #64006） | rendering | 是 | 頁框要給絕對尺寸 |
| zed | 35894 | Image viewer memory leak with BMP files. | closed 2026-07-30（#58803） | texture-memory | 部分 | 換圖時不 `drop_image` 就會累積 |
| zed | 39914 | Windows: Using the Zed Preview command 'markdown: open preview' consumes 2GB of memory | closed 2026-01-05（#46039） | texture-memory | 是 | 0.2.2 時代大量影像時記憶體爆量；只在 main 修 |
| zed | 56466 | GPUI allocation failure in img on huge Mermaid diagrams | closed 2026-07-07（#56468） | texture-memory | 是 | 單張巨圖會配置失敗，tile 尺寸要有上限 |
| zed | 39435 | Windows: Low fps in many case | closed 2026-02-06（#48282） | rendering | 是 | 0.2.2 的 instance buffer 上傳會讓 driver stall，primitive 多時掉 frame |
| zed | 32588 | Unnecessary GPU load presenting unchanged window contents (not per-element) | closed 2026-01-01（#45369） | CPU-GPU-power | 是 | 0.2.2 任何輸入都會重畫 1 秒 |
| zed | 37727 ✔ | Windows Alpha: Text typing loads GPU as FullHD video playing | open | CPU-GPU-power | 是 | 整窗重畫很貴；Intel iGPU 上更明顯 |
| zed | 58048 ✔ | win11 laptop fans become noisy & hot after screen off (not sleep) only when zed is not closed | open | CPU-GPU-power | 很可能（見 PR #63438） | 螢幕關閉後 vsync 迴圈仍在耗電 |
| zed | 63184 ✔ | Agent panel pins ~5 cores rendering a static thread at 240 Hz (0.28 cores at 60 Hz) | open | CPU-GPU-power | 部分 | 持續要求 frame 時，成本和更新率成正比 |
| zed | 50392 | Presence of animated elements generate layout recalculation and repaint | open | CPU-GPU-power | 是 | 任何動畫都會讓整窗每 frame 重新 layout |
| zed | 15166 | Excessive display server repaints from missing damage/present regions | open | CPU-GPU-power | 是 | 沒有 dirty rect；大的高 DPI 視窗比較耗電 |
| zed | 49588 | Overall UI rendering issues on Windows (ARM64) | closed 2026-08-03（#62069） | rendering | 是 | ARM64／Adreno 繪製錯誤；main 已修 |
| zed | 59192 | Minor multi-spot UI flickering/blinking in areas with Chinese characters (Windows/AMD iGPU) | closed 2026-08-03（#62069） | rendering | 是 | AMD iGPU 快速捲動時局部閃爍；main 已修 |
| zed | 52085 | Zed "exited" (crashed?) during graphics driver installation. | closed（stale；PR #63500 未合併） | rendering | 是 | device lost 復原失敗會 fail-fast（0xC0000409） |
| zed | 59962 | AMD 8060S no longer recognised as valid graphics card. | open | rendering | 是 | 建 device 失敗時會默默退到 WARP，效能大跌 |
| zed | 42632 | Windows: Unsupported GPU in Remote VM / VDI | closed（not planned） | CPU-GPU-power | 部分 | 沒有 GPU 時走 WARP，可用但慢；真正的軟體 renderer 只有 draft PR #63936 |
| zed | 26692 | Zed does not work in Remote Desktop session on windows | closed 2025-07-30（#34374） | rendering | 是 | DX11 版本可以在 RDP 下運作（0.2.2 已包含） |
| zed | 39263 | Windows Beta: First GPU is selected, rather than the best / correct GPU | closed 2025-10-10（#39264 ✔） | CPU-GPU-power | 是 | 現在採用 DXGI 預設順序；沒有讓 app 選 GPU 的 API |
| zed | 55654 | Zed becomes transparent when NVIDIA overlay is active | closed（upstream） | rendering | 是 | 可用 `GPUI_DISABLE_DIRECT_COMPOSITION=1` 繞過 |
| zed | 63471 ✔ | gpui (Windows): WM_DISPLAYCHANGE re-shows hidden windows via unconditional ShowWindow(SW_SHOWNORMAL) | closed 2026-09-29（main 由 #48902 修正） | DPI／多螢幕 | 是 | 0.2.2 插拔螢幕會把隱藏的視窗叫出來；**維護者在這裡表示 0.2.2 已過時** |
| zed | 48927 | New window opens off the screen limits when using a second display | closed 2026-08-26（#62859） | DPI | 是 | 副螢幕縮放不同時，新視窗會開在螢幕外 |
| zed | 59526 | GPUI: primary display initiates the window to display on the second display, Coordinate calculation error | open | DPI | 是 | 0.2.2 在 Win11 混合縮放（1.75／1.25）下 `Bounds::centered` 算錯位置 |
| zed | 61914 | Settings page header is clipped under certain DPI scaling values on Windows | closed 2026-09-17（#62073） | DPI | 是 | 200–250% 縮放時視窗頂端被切掉 |
| zed | 58201 | Zed tries to go to a monitor that doesn't exist | open | DPI | 可能 | 全螢幕中換螢幕再退出，視窗會跑到不存在的螢幕 |
| zed | 39513 ✔ | Windows Beta: Scroll jumps to top/bottom and touch selects text instead of scrolling | closed 2026-09-11（stale，未修） | scroll | 是 | `WHEEL_PAGESCROLL` 設定下捲動異常；觸控螢幕無法捲動 |
| zed | 39170 ✔ | Windows Beta: Scroll is choppy when using scroll momentum on a touchpad | closed 2026-04-24（stale，未修） | scroll | 是 | 觸控板慣性捲動卡頓 |
| zed | 51312 | GPUI: Add pinch event support for X11 and Windows | closed 2026-03-28（#51354 ✔） | input | 是 | pinch 只有 main 有 |
| zed | 61704 | Mouse wheel zoom setting does not support trackpads | open | input | 可能 | 觸控板 zoom 可能收不到事件 |
| zed | 62404 | Windows: phantom Alt key events injected on every window activation (opening a project) | open | input | 是 | 視窗啟用時合成 Alt，可能干擾 IME |
| gpui-kit | 1666 ✔ | Efficiently rendering raw rgba buffers or wgpu textures in GPUI | closed 2026-08-24（not planned） | texture-memory | 是 | 沒有 zero-copy 路徑，只能走 `RenderImage` 加 atlas |
| gpui-kit | 3168 ✔ | Expose a stable extension seam for externally owned native GPU surfaces | open（轉到 zed discussion #64849） | texture-memory | 是 | 不能直接畫自己的 D3D11 SRV |
| gpui-kit | 1689 | Feature Request: RenderImage Add Rgba< u32 > format feature | closed（轉 upstream） | texture-memory | 是 | `RenderImage` 只吃 8-bit |
| gpui-kit | 2005 | [Feature Request] Support for High Bit Depth (10/12/14/16-bit) Integer Color API via u16 | open | rendering | 是 | 沒有高位元深度 |
| gpui-kit | 2532 | Multiple versions of gpui crate cause type incompatibility when using git dependencies | open | other | 否（相依管理） | 用 git 依賴時，gpui 必須和元件庫同一個 rev |
| gpui-kit | 2488 | In the dock example, the window resizing that occurs when dragging the splitter lags behind the mouse movement. | open | CPU-GPU-power | 不明 | Win11 拖拉 splitter 時 resize 延遲 |
| gpui-kit | 2018 | Broken TabBar in TitleBar on Windows | open | input | 是（zed #48330 引起） | main 的 hit-test 變更後，標題列裡的按鈕失效 |

已合併的修正 PR（✔ 為已核對）：
- #58874 ✔、#48282 ✔、#45369 ✔、#64623 ✔、#52970 ✔、#51354 ✔、#39264 ✔、#62069、#62859、#48902、#55065、#64250

仍未合併的 PR：
- #63438 ✔ 螢幕關閉時停止 render
- #64205 ✔ 觸控
- #64061 ✔ updatable dynamic textures
- #62557 ✔ atlas bleeding
- #63936 ✔ 軟體 render
- #65002 目標螢幕 DPI

查無相關 issue 的主題：
- Windows 120／144 Hz frame pacing
- `uniform_list` 效能
- 切換 per-monitor DPI 後影像模糊
- Windows 上 GPUI 層的 drag & drop、檔案對話框、dark mode 問題

---

## Measurements（GPUI floor）

### 方法

- 專案放在 session scratchpad 的 `agent-gpui\` 底下，不在 repo 裡：
  - `floor\`：gpui 0.2.2，bin `hello`／`probe`
  - `floor-pre\`：gpui-pre 0.3.7，bin `hello`／`probe`／`scrollbench`
  - `floor-gc\`：gpui 0.2.2 加 gpui-component 0.5.1
  - `d3dbase\`：純 D3D11／DComp／DWrite 的 console 程式
  - `swapbench\`：RGBA→BGRA 轉換
- 量測腳本：`measure.ps1`，用 `Process.Start` 啟動
  - 每 200 ms 取樣 WS、Private、CPU time（user／kernel）、threads、handles
  - 另一個 job 用 `Get-Counter` 每約 1.1 s 取樣 `GPU Process Memory`（Dedicated／Shared）與 `Process\Working Set - Private`
  - 程式自己把 `main`、`app_run`、`first_render`、`next_frame_after_first_render` 的 wall clock 寫進 log，和 process 建立時間（`Process.StartTime`）相減
- **TTW** 的定義：process 建立到 `on_next_frame` callback 被呼叫。這時第一個 frame 已經 present，誤差約 1 個 vsync
- GUI 共啟動 5 次：0.2.2 的 hello、0.2.2 的 probe、main 系的 hello、main 系的 probe、main 系的 scrollbench
  - 3 次在量測結束後由腳本 kill，2 次自行退出；全部結束後用 `Get-Process` 確認沒有殘留
  - 0.2.2 的 probe 沒有自行退出，是測試程式本身的問題：`cx.quit()` 放在 `render()` 裡，而視窗最小化後不會呼叫 `render()`。和 GPUI 無關，main 系的版本已改由 timer 呼叫 quit
- 限制：
  - 單一機器，NVIDIA，60 Hz，100% 縮放
  - 每個數字 n = 1–3
  - 第一次 0.2.2 build 時，其他 agent 同時有 32 個 rustc 在跑，所以另外在低負載時重測

### Build 與 exe

| 組態 | clean release build | exe（預設 release） | exe（`release-small`：fat LTO、cgu=1、strip、`panic=abort`） | 相依 crate 數（Windows target，normal+build） |
|---|---|---|---|---|
| gpui 0.2.2，預設 features | **1m11s**（低負載）／2m33s（32 個 rustc 並行） | 10,836,992 B（10.3 MiB；`.text` 7.8 MiB、`.rdata` 2.1 MiB） | **4,787,712 B（4.6 MiB）**，build 2m12s | 441（`default-features=false` 時 414） |
| gpui-pre 0.3.7（main 系），`default-features=false` 加 `windows-manifest` | 1m35s（低負載，2 個 bin） | 10,225,664 B（9.8 MiB；`.text` 6.8 MiB、`.rdata` 2.4 MiB） | **6,620,160 B（6.3 MiB）**，build 1m58s。main 在 LTO 後反而比 0.2.2 大，多出 accesskit、gestures 等 | 309 |
| gpui 0.2.2 ＋ gpui-component 0.5.1 | 2m23s（低負載） | 17,004,544 B（16.2 MiB；`.text` 11.7 MiB、`.rdata` 3.8 MiB） | — | 499 |
| scrollbench（main 系，用到 `uniform_list` 和 `img`） | 增量 build | 12,029,952 B | — | — |

- exe import 的 DLL：`d3d11`、`dxgi`、`dcomp`、`dwrite`、`dwmapi`、`imm32`、`icuuc`、`combase`／WinRT、`uiautomationcore`（只有 main），以及 **`VCRUNTIME140.dll`**（動態 CRT，見依賴策略第 5 點）

### 啟動與 idle（hello，800×600；probe 和 scrollbench 是 1024×768）

| 指標 | gpui 0.2.2 | main 系（gpui-pre 0.3.7） | 純 D3D11 console（無視窗） |
|---|---|---|---|
| process 建立 → `main()` | 24／21 ms | 17／16／15 ms | — |
| `main()` → `app_run`（平台初始化） | **325／404 ms** | **161／152／138 ms** | `D3D11CreateDevice` 約 115 ms |
| → 第一次 `render()` | 394／474 ms | 212／201／185 ms | — |
| 看到 HWND（外部輪詢） | 379／465 ms | 203／203／183 ms | — |
| **TTW**（第一個 frame 已 present） | **403／489 ms** | **228／226／205 ms** | — |
| idle CPU（10 s） | 0.79%／1.06% of 1 core | 0.78%／0.46% | — |
| Working Set | 69.1 MiB | 67.2 MiB | 37.6 MiB |
| Private Bytes（commit） | 78.2–84.7 MiB | 81.7–87.8 MiB | 55.4 MiB |
| Private Working Set | 16.9 MiB | 15.5–15.6 MiB | — |
| threads／handles | 113／745–751 | 114／767 | — |
| GPU dedicated／shared | 23–26／0.9 MiB | 22–23／0.9 MiB | — |

解讀：
- 0.2.2 比 main 慢約 170–260 ms。大部分是 0.2.2 建立了兩次 D3D11 device（`directx_devices.rs:61,134`），單次約 115 ms
- 在 RTX 5090 上，D3D11 device 本身就佔約 27 MiB WS 和 53 MiB commit。GPUI 自身再加約 30 MiB WS、約 25–30 MiB commit
- 對照 spec §29 KPI：

| KPI | GPUI floor | 判斷 |
|---|---|---|
| exe < 30 MB | 10 MiB；LTO 後 4.6 MiB（0.2.2）／6.3 MiB（main） | ✔ 還有約 20 MB 可以留給 engine 和字型 |
| idle RAM < 50 MB | Private WS 約 16 MiB | ✔ |
| 同上 | WS 約 68 MiB、commit 約 80 MiB | ✘ 光是 GPUI 加 driver 就已超標，**KPI 必須先定義用哪個指標** |
| idle CPU 接近 0 | 0.5–1% of 1 core | 約略可接受；真正歸零需要 patch |
| 首頁 < 200 ms | 光 GPUI 開窗就要 205–228 ms（main）、403–489 ms（0.2.2），其中 D3D11 device 建立約 115 ms | **✘** 從 exe 啟動算起的話，GPUI 本身已經用完預算。建議：KPI 改從「視窗可見」或「開檔指令」起算；同時在 `main()` 一開始就用背景 thread 讀檔並解析 page 1，和 GPUI 平台初始化（150–400 ms）重疊 |

### 影像、atlas 與捲動

詳見 Texture & Image Pipeline 與 Scrolling & Layout 兩節的表格。

---

## gpui-component Assessment

| 面向 | 事實 |
|---|---|
| License | Apache-2.0（crates.io metadata 與 `LICENSE-APACHE`）。GitHub 偵測為 NOASSERTION |
| 維護 | • repo 已改名為 `longbridge/gpui-kit`：15.8k stars、98 個 open issue，最後 push 在 2026-10-03<br>• 2025-10 起大約 1–2 週發一版 |
| 大小 | 0.5.1 有約 57.9k 行 Rust、2.6 MB 原始碼，包含 dock、editor／highlighter、chart、table、webview、markdown 等 |
| 相依 | • 0.5.1 的**非 optional** 相依：`tree-sitter`、`tree-sitter-json`、`html5ever`、`markup5ever_rcdom`、`markdown`、`lsp-types`、`notify`（檔案監看）、`ropey`、`rust-i18n`、`schemars`、`chrono`、`uuid`、`regex`、`aho-corasick` 等<br>• 0.7.0 另外還有 `gpui-base`、`gpui-kit-assets`、`resvg`（Windows）、`windows 0.58`（和 gpui 用的 `windows` 0.61／0.62 重複） |
| 成本（實測） | 和只用 gpui 的 hello 相比：<br>• exe **+6.2 MB（+57%）**<br>• clean build **+72 s（約 2 倍）**<br>• 相依 crate **+58 個**（441 → 499） |
| 版本耦合 | • 0.6 起只能搭配 `gpui-pre`（`=0.3.7` 精確 pin），**不能和官方 zed git rev 混用**（gpui-kit #2532：型別不相容）<br>• 用它等於把 GPUI 版本決定權交給 Longbridge 的 snapshot 節奏 |
| FastPDF 實際需要的元件 | 工具列按鈕、icon、tooltip、搜尋輸入框（IME 與選取）、側欄縮圖清單（虛擬化）、捲軸、右鍵選單、簡單對話框 |

**建議：不要依賴 gpui-component。** 理由是 spec §36：只為了少數元件就引入 58 個 crate 和 6 MB，而且會鎖死 GPUI 版本。

替代做法：
- 在 `fastpdf-ui` 自寫約 6–8 個最小元件。大部分是 GPUI 的 `div` 加 `on_click`，加起來數百行
- 最難的是文字輸入框，可以參考 GPUI 官方的 `examples/input.rs`（`EntityInputHandler`）
- 真的需要某個元件時，可依 Apache-2.0 **複製該元件原始碼**，並保留 NOTICE 和出處，比整包依賴好
- pdf-reader-gpui 目前依賴它（0.5.1）。若 fork 的話，這是要拆除的耦合之一，細節見 pdf-reader-gpui 的 audit

---

## Risks

| # | 風險 | 可能性 | 影響 | 對策 |
|---|---|---|---|---|
| R1 | atlas 洩漏：淘汰 tile 時忘了 `drop_image`，或 `Arc` 被提前丟掉 | 高 | VRAM 和 commit 無上限成長（實測 400 MiB），工作管理員的「記憶體」欄看不出來 | `TileTextureManager` 集中管理，禁止在別處建立 `RenderImage`；debug overlay 顯示 atlas bytes；CI 跑 churn 測試 |
| R2 | 停在 crates.io 0.2.2 | 中（pdf-reader-gpui 現況就是這樣） | 碎片化（實測 VRAM +89%）、啟動慢 2 倍、缺 pinch、多螢幕 bug | 改用 git rev pin（B） |
| R3 | zed main API 經常變動，升級要改 code | 高 | 每次升級花費數小時到數天 | GPUI 只出現在 `fastpdf-ui`；用 probe 和 bench 當升級 gate；固定節奏升級 |
| R4 | 按住按鍵時 present 飢餓（#61469） | 中 | PageDown 連發時畫面凍結數秒 | worker 通知合併成每 frame 一次；鍵盤節流；必要時 patch |
| R5 | idle 時 vsync 喚醒，無法真正 0% | 確定 | 0.5–1% of 1 core，高更新率會放大 | 不做常駐動畫；必要時 patch vsync park（參考 #63182） |
| R6 | 一次大量 upload 造成卡頓（zoom、跳頁） | 中 | 單 frame > 16 ms | 每 frame 設 upload 預算；先顯示低解析；tile ≤ 512 |
| R7 | tile 接縫與滲色（#62456） | 中 | 縮放過渡期間出現細線 | tile 加 gutter；過渡時改用頁面級影像；追蹤 PR #62557 |
| R8 | GPU／driver 相容性：WARP 回退、混合顯卡、NVIDIA overlay、device lost fail-fast | 低–中 | 慢、透明、當掉 | 文件說明 `GPUI_DISABLE_DIRECT_COMPOSITION`；啟動時記錄 `gpu_specs()`；device lost 後清空 FastPDF 端快取的對應關係 |
| R9 | 沒有自訂 texture，限制 zpdf GPU backend 的整合 | 中（取決於 Q5） | GPU raster 的結果要先讀回 CPU 再上傳 | 先走 CPU 路徑；需要時 fork `gpui_windows` 加 external texture |
| R10 | 第三方 snapshot（`gpui-pre`）的供應鏈風險 | 低（只有選 C 時才有） | — | 核對 zed sha；優先用官方 git |
| R11 | spec KPI 定義與 GPUI floor 衝突：首頁 < 200 ms、idle RAM < 50 MB | 確定 | 「看起來沒達標」 | 先定義量測點：視窗可見起算、Private WS 或 commit；如實記錄 GPUI floor（spec §29「不要作弊」） |

---

## Verdict

**Q3：GPUI 是否存在會影響 Reader 的限制？** 信心度：中高。原始碼證據充分，實測只在一台 NVIDIA、60 Hz、100% 縮放的機器上做，沒有測 iGPU、高更新率、混合 DPI。

1. **Windows rendering limitation：存在，中度，可繞過，不是 blocker**
   - 問題：
     - D3D11 加 DComp 本身穩定，RDP 和 WARP 也能跑
     - vsync thread 永不停，idle 時仍以 refresh rate 喚醒（實測 0.5–1% of 1 core）
     - 沒有 damage region，每次都整窗 present
     - 持續輸入時 present 可能飢餓（#61469，open）
     - 沒有自訂 texture 和 HDR，沒有列印
     - 0.2.2 另外有「輸入後整窗重畫 1 秒」以及多處已在 main 修正的 bug
   - 對策：
     - 用 main
     - 合併 worker 通知，不做常駐動畫
     - 列印自己走 Win32 GDI
     - 準備兩個小 patch（vsync park、key dispatch 後 present），只在量測證明需要時才套用
2. **Scrolling limitation：不存在阻擋性限制，低度**
   - 實測 2000 頁連續捲動穩定 60 fps
   - 每 frame CPU 0.05 ms（canvas）／0.13 ms（`uniform_list`）／0.97 ms（加 600 個 overlay 元素），沒有掉 frame
   - 缺的是體驗層的功能：滾輪平滑動畫、0.2.2 沒有觸控板像素捲動和 pinch（main 有）、`WHEEL_PAGESCROLL` 的 bug
   - 對策：自訂 canvas 虛擬化（prefix sum），平滑捲動和 zoom 手勢由 FastPDF 實作
3. **Texture limitation：存在，高度，但可控。這是 GPUI 對 FastPDF 最重要的限制**
   - 實測問題：
     - 沒有自動 eviction，忘了 `drop_image` 就永久洩漏
     - 0.2.2 的 atlas 碎片化（150 MiB 存活資料吃掉 630 MiB VRAM；main 是 333 MiB）
     - upload 在 UI thread 同步執行（64 MiB 要 8–11 ms）
     - host 端會有 CPU 那份、driver staging、commit 多重佔用
     - VRAM 延遲回收
     - 沒有 mipmap、沒有 padding（#62456）
     - 單張影像上限寫死 16384²
     - 沒有 external texture
   - 對策：
     - 用 main
     - `TileTextureManager` 負責預算、LRU、顯式 `drop_image`
     - tile 固定 256 或 512
     - 每 frame 限制 upload 量
     - tile 加 gutter
     - 縮圖直接用目標大小 render
4. **High-DPI limitation：不存在阻擋性限制，低度**
   - 有 PerMonitorV2 manifest、`WM_DPICHANGED`、`scale_factor`
   - 0.2.2 有混合縮放多螢幕的位置 bug，main 已修多數（#62859、#40053、#48902、#55630），仍有 #59526、#58201 open
   - 對策：
     - 用 main
     - DPI 改變時讓 tile cache 依新 scale 失效並重新 render
     - 自訂 manifest 時要關掉 gpui 的 `windows-manifest` 並合併內容

**Q7 中和 GPUI 相關的部分：**
- 最可能的瓶頸是 **texture cache 的記憶體管理**，其次是 **zoom 或跳頁時集中 upload 造成的單 frame 卡頓**
- GPUI layout 和 scroll 在實測下都不是瓶頸
- PDF 本身的瓶頸（parse、font、image decode、rasterization）不在本文範圍，見 engine 的 audit

**建議的下一步：**
1. ADR `0001-use-gpui.md`：維持使用 GPUI，依賴策略 B，`default-features = false` 加 `crt-static`
2. M1 就把本文的 `probe` 和 `scrollbench` 收進 `benchmarks/gpui/`，當作 GPUI 升級的 regression gate
3. M5 實作 `TileTextureManager` 時，以本文的實測數字（每 MiB upload 成本、atlas 放大倍數）作為預算設計的起點
4. 冷啟動：在 `main()` 進入 GPUI 之前就在背景開始讀檔與解析，把 GPUI 平台初始化的時間（實測 138–404 ms）拿來重疊 PDF 開檔
