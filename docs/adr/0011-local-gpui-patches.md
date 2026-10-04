# ADR 0011 — 在本地套用 GPUI 的修正（vendored `gpui_windows`）

- 狀態：Accepted
- 日期：2026-10-05
- 相關 spec：§29（小檔首頁 < 200 ms、idle CPU 接近 0）、§36–§37（依賴與授權）
- 相關文件：
  - ADR 0001（GPUI 以 git pin 使用；本 ADR 是它的例外）；
  - ADR 0009（啟動時間線，G1、G2 的來源）；
  - `docs/upstream-issues/gpui-startup.md`、`docs/upstream-issues/gpui-idle.md`（upstream 草稿）；
  - `vendor/gpui_windows/FASTPDF-PATCHES.md`（patch 清單、衝突處理、移除條件）；
  - `docs/benchmarks/b8-app.md`〈第九輪〉（本 ADR 的量測）；
  - `docs/PROJECT_AUDIT.md` R3、R8、R9。

## Context

B-8 第八輪（`b7797fc`）時，spec §29 只剩兩項沒有達成，兩項都卡在 GPUI 的 Windows platform crate `gpui_windows`：

- **小檔首頁**：中位數 204.4 ms，目標 < 200 ms。ADR 0009 的時間線顯示，剩下的時間幾乎都是 GPUI 啟動：
  - 主執行緒先花約 37 ms 做 platform 初始化，其中系統字型集合的 update check 占 27 ms；
  - 之後才開始 `D3D11CreateDevice`，約 100–120 ms。
- **Idle CPU**：FastPDF 自己在 idle 時畫 0 個 frame，但 GPUI 的 `VSyncProvider` thread 每個 vblank 都 invalidate 所有視窗，主執行緒每秒被喚醒約 95 次。

三個修正都已經寫好，並在 scratch 中量過：

| 修正 | 內容 | upstream 草稿 |
|---|---|---|
| G1 | 在 `WindowsPlatform::new` 一開始就用執行緒建立 DirectX devices，與 platform 初始化平行 | `gpui-startup-0001` |
| G2 | 建立系統字型集合時不做 update check | `gpui-startup-0002` |
| vsync park | 實作 `PlatformWindow::frame_waker`，沒有視窗要求 frame 超過 1 秒就 park vsync thread | `gpui-idle-0001` |

送 upstream 要先開 discussion（由使用者決定），合併的時間無法預期。ADR 0001 選擇 git pin、不 fork；但它的第 7 點已預留「量測證明需要時，再以 `[patch]` 套用小 patch」。所以先在本地套用，upstream 草稿照原計畫保留。

## Decision

1. **只 vendor 一個 crate。**
   - 把 pin rev（`a84689073d296dfd39987bc7dd478e43ef76d83a`）的 `crates/gpui_windows` 複製到 `vendor/gpui_windows/`。
   - 用 root `Cargo.toml` 的 `[patch."https://github.com/zed-industries/zed"] gpui_windows = { path = "vendor/gpui_windows" }` 取代 upstream 的 crate。
   - GPUI 的其他部分（`gpui`、`gpui_platform` 等）仍是 ADR 0001 的 git pin。
2. **三個 patch**放在 `vendor/gpui_windows-patches/`，依檔名順序套用：0001 G1、0002 G2、0003 vsync park。
   - 這是本機套用的唯一來源。`docs/upstream-issues/patches/` 的草稿保留作為送 upstream 的版本。
   - 0001、0002 與草稿相同。0003 是 rebase 到 0001 之後的版本：一個 hunk 的 context 衝突，以手動解決，見 `FASTPDF-PATCHES.md`。
3. **可重現。**
   - `tools/vendor_gpui_windows.py` 從本機 cargo checkout 重新產生整個 vendored crate：改寫 `Cargo.toml`、套 patch、加修改聲明。
   - `--check` 逐位元組比對重新產生的結果與 repo 中的內容，並確認 build 用的是這份 crate（`[patch]` 存在，`Cargo.lock` 沒有其他來源的 `gpui_windows`）。`--unit-tests` 執行 `vsync.rs` 的單元測試。
   - 除了 `FASTPDF-PATCHES.md`，vendored 檔案一律由工具產生，不手改。
4. **`Cargo.toml` 改寫。**
   - zed workspace 的繼承全部展開；第三方依賴的版本需求與 features 和 zed workspace 相同。
   - `gpui`、`collections`、`gpui_util`、`scheduler` 用和 FastPDF 相同的 git URL 與 rev，所以只有一份 gpui。
   - `[lints]` 留空：不套用 zed 的，也不套用 FastPDF 的。
   - 結果：`Cargo.lock` 只少了 `gpui_windows` 的 `source` 一行；解析出的 473 個 package、features 與依賴關係，在預設與 `--all-features` 下都和改動前相同。
5. **不是 workspace member。**
   - root `Cargo.toml` 的 `[workspace] exclude` 加上 `vendor/gpui_windows`。
   - `--workspace` 的 clippy、`cargo fmt --all` 與 FastPDF 的 lints 都不碰它，upstream 程式碼保持原樣。
6. **授權（Apache-2.0）。**
   - 放 zed repository 根目錄 `LICENSE-APACHE` 的全文。upstream 的 crate 裡只是 symlink。
   - 每個修改過的檔案，第一行都有修改聲明（§4(b)）：`// Modified by FastPDF: <patch> (<編號>). See FASTPDF-PATCHES.md.`。
   - 登記在 `tools/ported-sources.json`。
   - `THIRD_PARTY_LICENSES.md` 仍把它列為 Apache-2.0 的第三方 crate，並標明 vendored 位置（`tools/license_report.py` 對 path 依賴的處理）。`--bundle` 會收錄它的 `LICENSE-APACHE`。

## 範圍

- 改到的 upstream 檔案只有 6 個：
  - `platform.rs`：G1、vsync park；
  - `direct_write.rs`：G2；
  - `vsync.rs`、`window.rs`、`events.rs`、`direct_manipulation.rs`：vsync park。
- patch 合計 +270／−15 行（0001：+38／−4，0002：+5／−1，0003：+227／−10），另加 `Cargo.toml` 改寫與修改聲明。
- exe 多 10,752 B（16,881,664 → 16,892,416 B）。
- FastPDF 自己的程式碼沒有改。`gpui` crate 本身也沒有改：vsync park 用的是 GPUI 既有的 `frame_waker` hook。

## 量測

B-8 配對量測：A 是 HEAD（`3f4d104`），B 是 A 加上本 ADR 的 vendored crate，FastPDF 的程式碼相同。兩者都是 dist build，A、B 交替啟動，每次啟動前等系統負載降到 30% 以下。細節與每對的數字見 `docs/benchmarks/b8-app.md`〈第九輪〉。

| 指標（中位數） | A | B |
|---|---|---|
| 3 頁 `first_page_exact`（6 對） | 196.1 ms | **170.7 ms**（164.2–179.8；6 對都比 A 快，每對差的中位數 −28.1 ms） |
| 3 頁 `window_visible` | 195.7 ms | 164.7 ms |
| 300 頁 `first_page_exact`（6 對） | 199.8 ms | 169.7 ms |
| idle CPU（3 頁，% 單核） | 0.78 | **0**（0–0.16） |
| idle 主執行緒／vsync thread 喚醒（次／秒） | 77.1／63.6 | **0／0.99** |
| idle process tree（Mcycles／秒） | 26.7 | 3.3（剩下的是 NVIDIA driver 的 thread，A 也有） |
| FastPDF 在 idle 時畫的 frame | 0 | 0 |
| idle private working set（3 頁） | 28.3 MB | 28.0 MB |
| peak private bytes（300 頁） | 284.9 MB | 283.9 MB |

- **互動（300 頁）**：滾輪、PageDown、縮放的首次反應與穩定時間，差異在截圖解析度（約 15 ms）與背景負載的範圍內，方向不一致。每次啟動做 8 輪輸入的補充量測中，B 的首次反應比較快（滾輪 24.4 → 18.8 ms，縮放 26.3 → 21.5 ms），在相近負載下捲動的 CPU cycles 也沒有增加。
- **畫面**：bench-app 的截圖在多數 run 逐位元組相同；不同的時候最多差 94 個像素、每個通道差 1，A 與 B 都會發生。
- **重畫**：vsync thread park 之後，最小化／還原、最大化／還原、滾輪、深淺色切換都立刻正確重畫，時間在 A 兩次量測的範圍內。
- **KPI**：spec §29 的小檔首頁 < 200 ms 與 Idle CPU 接近 0 都達成；Idle RAM 與 exe 大小維持達成。

## Device lost

依據閱讀 patch 後的程式碼：

- `platform.rs`：`begin_vsync_thread`、`check_device_lost`、`handle_gpu_device_lost`；
- `events.rs`：`handle_size_change`、`handle_device_lost`。

結論：

- **偵測**。vsync thread 的 `recover_if_device_lost` 會呼叫 `GetDeviceRemovedReason`，並檢查 `invalidate_devices` 旗標（視窗的 `ResizeBuffers` 失敗時設定）。
  - 原本：每個 vsync tick（約 16.7 ms）檢查一次。
  - 套用後：有 frame 需求時一樣每個 tick 檢查。park 期間每 1 秒醒來檢查一次（`IDLE_DEVICE_CHECK_INTERVAL`）。任何 frame 需求（視窗變 dirty、resize、輸入）都會立刻喚醒 thread，先檢查 device，再 invalidate 視窗。
  - 所以 idle 時的偵測延遲從最多一個 frame 變成最多 1 秒；有互動時不變。這 1 秒就是 park 期間唯一留下的週期性喚醒（vsync thread 每秒 1 次）。
- **復原流程沒有改**：
  - `handle_gpu_device_lost` 等 350 ms，重建 `DirectXDevices`；
  - 對 platform 與每個視窗送 `WM_GPUI_GPU_DEVICE_LOST`，重建 renderer、atlas 與 text system 的 GPU 狀態；
  - 200 ms 後送 `WM_GPUI_FORCE_UPDATE_WINDOW`，強制重畫；
  - 復原失敗仍是 `panic!("Device lost: …")`，和 upstream 相同。
- **G1 只改了第一次建立 device 的時機**。early thread 建立的 devices 在 `attach_gpu` 交給 platform，之後的 device lost 流程完全相同。early thread 失敗或 panic 時，`attach_gpu` 在主執行緒重建（記錄 log）。
- **resize 失敗**：`handle_size_change` 設定 `invalidate_devices` 時，resize 本身已讓視窗 dirty，vsync thread 會立刻醒來處理，不會等 1 秒。
- **停止**：`detach_gpu` 與 `Drop` 設定 stop 旗標後，會呼叫 `frame_demand.wake()`，park 中的 thread 醒來後結束。
- **沒有實測**。觸發真正的 device removal（TDR）需要改變 GPU 或系統狀態（例如 `dxcap -forcetdr`），不在這次允許的範圍內。

## Consequences

- **KPI**：spec §29 剩下的兩項都達成，數字見〈量測〉。
  - 小檔首頁：3 頁 `first_page_exact` 中位數 196 → 171 ms。
  - Idle CPU：主執行緒喚醒 77 → 0 次／秒，vsync thread 64 → 1 次／秒，CPU time 中位數 0.78% → 0% 單核。
- **其他指標沒有退步**：
  - idle private working set 少 0.35 MB，peak private bytes 相同；
  - 捲動、PageDown、縮放的反應時間在雜訊範圍內；
  - 畫面相同：截圖在多數 run 逐位元組相同，偶爾出現的像素級差異 A 與 B 都有。
- **維護成本**：
  - repo 多了約 1.36 萬行 upstream 程式碼（27 個檔案），要跟著 GPUI 升級。真正要維護的是 patch 本身（+270／−15）。
  - 每次升級 GPUI pin，都要執行 `tools/vendor_gpui_windows.py`；patch 套不上就要 rebase。
  - vendored crate 不是 workspace member，`cargo test --workspace` 與 clippy 都不涵蓋它。它自己的 test target 需要 gpui 的 `test-support`，離線無法編譯。能跑的只有 `vsync.rs` 的 5 個單元測試（`--unit-tests`）。
- **行為差異**（只在 idle 時）：
  - device lost 的偵測延遲從最多一個 frame 變成最多 1 秒，見〈Device lost〉。
  - 如果某個地方只靠 vsync 的週期性 invalidate 重畫，卻沒有向 GPUI 要求 frame，就要等下一個輸入或 OS 的 `WM_PAINT` 才會重畫。FastPDF 的動畫（平滑捲動）都經由 GPUI 要求 frame，不受影響；重畫檢查也沒有發現這種情況。
- **授權**：FastPDF 的發佈物現在包含修改過的 Apache-2.0 程式碼。義務都已處理：
  - 附上授權全文；
  - 修改過的檔案有修改聲明；
  - `FASTPDF-PATCHES.md` 說明修改內容；
  - zed repository 沒有 NOTICE 檔，§4(d) 不適用。
- **CI**：`gpui_windows` 改從 repo 內建置，其他 GPUI crate 仍從 zed 的 git 取得。建議在 CI 加上 `python tools/vendor_gpui_windows.py --check`。
- **exe**：多 10,752 B。

## 風險

| 風險 | 影響 | 可能性 | 對策 |
|---|---|---|---|
| 升級 GPUI 時 patch 套不上。`platform.rs`、`window.rs`、`events.rs` 在 upstream 經常修改 | 中：升級要多花時間 | 高 | 工具在套不上時直接失敗，並列出失敗的 hunk。patch 小（+270／−15），衝突處理記錄在 `FASTPDF-PATCHES.md`。改動太大就放棄該 patch（〈退出計畫〉） |
| `[patch]` 默默失效（例如版本號或 crate 名稱改變），build 回到 upstream 而沒人發現 | 中：KPI 退回第八輪 | 中 | `--check` 確認 `Cargo.lock` 只有 vendored 的 `gpui_windows`。B-8 的 `-ThreadDetail` 也會看到主執行緒的喚醒回來 |
| 有人直接修改 vendored 原始碼，與 patch 不一致 | 中：無法重現 | 低 | `--check` 逐位元組比對。`FASTPDF-PATCHES.md` 與檔頭的修改聲明都指向 patch |
| vsync park 漏掉某種需要 frame 的情況，畫面停在舊內容 | 高：顯示錯誤 | 低 | frame 需求來自 GPUI 自己的 `frame_waker`，任何 invalidate 或動畫 frame 都會喚醒 vsync thread。OS 的 `WM_PAINT` 不經過 vsync thread。重畫檢查涵蓋最小化／還原、最大化／還原、滾輪、深淺色切換。沒有測試的情況：系統主題、DPI 或螢幕變更、device lost，這些都需要改系統設定。出問題時拿掉 `[patch]` 就能回復 |
| idle 時 device lost 晚 1 秒才偵測到 | 低：復原多等最多 1 秒 | 低 | 復原流程不變；有互動時每個 frame 仍會檢查 |
| upstream 不接受，或改用不同的做法 | 中：要長期維護本地 patch | 中 | 以 upstream 的做法為準：升級到包含它的 rev 時改用 upstream 的版本，本地 patch 只維持到那時 |
| 本地 patch 與 upstream 草稿分歧（0003 有兩份） | 低 | 中 | 本地 patch 是唯一的套用來源。草稿在討論中修改後，重新 rebase 本地版本並執行工具 |

## 退出計畫

1. **正常退出**：upstream 合併了 patch（或等效的改動），FastPDF 也升級 pin 到包含它的 rev。
   - 逐一刪除已合併的 patch，重新產生 vendored crate。
   - 三個都合併後，照 `FASTPDF-PATCHES.md`〈何時移除〉刪掉 vendor、工具、`[patch]`、`exclude` 與 `ported-sources.json` 的條目，並重新產生 `THIRD_PARTY_LICENSES.md`。
   - 重跑 B-8 配對量測，確認 KPI 仍然達成，再把本 ADR 標為 Superseded。
2. **緊急回復**（patch 造成問題時）：
   - 刪掉 root `Cargo.toml` 的 `[patch]` 那兩行，`cargo build --offline` 就會回到 pin rev 的原版 `gpui_windows`。它的 checkout 已經在 cargo cache 裡，`Cargo.lock` 會自動補回 `source`。
   - FastPDF 本身的程式碼不依賴這些 patch，不必修改。KPI 會回到第八輪：首頁約 204 ms，idle 時主執行緒每秒約 95 次喚醒。
   - 也可以只刪掉出問題的那個 patch 檔並重新產生，保留另外兩個。
3. **定期檢查**：照 ADR 0001 每 4–8 週評估 GPUI 升級時，一併確認 upstream 的狀態。如果 rebase 的工作量明顯超過 patch 本身（例如 upstream 重寫了 vsync 或 device 的處理），就照第 2 點回到 upstream 的行為，再重新評估。

## Alternatives considered

- **等 upstream**：不改依賴，也不必維護。但送 upstream 要先開 discussion（由使用者決定），合併時間無法預期，這段期間 §29 的兩項 KPI 都達不到。否決；草稿仍保留，準備送 upstream。
- **fork zed，`[patch]` 指向 fork 的 git URL**：要在 GitHub 建立並維護整個 repo 的 fork，不在這次的權限內；每次升級都要 rebase 整個 repo；審查也不如 repo 內的 diff 直接。否決。
- **`[patch]` 指向 repo 外的本機 zed checkout**：其他人與 CI 無法重現。否決。
- **在 build 時修改 cargo cache 裡的原始碼**（用 build script 或手動）：不可重現，還會影響本機其他使用同一個 checkout 的專案。否決。
- **vendor 整個 GPUI**：ADR 0001 已否決（數萬行）。這次只需要 `gpui_windows`。
- **在 FastPDF 層處理**：做不到。FastPDF 在 idle 時已經不畫 frame，喚醒來自 GPUI 的 vsync thread。啟動成本在 `WindowsPlatform::new` 裡面，FastPDF 無法替 GPUI 提前建立 device。
- **只套用啟動的兩個 patch（G1、G2），vsync park 等 upstream**：vsync park 的改動最大（+227 行），但 idle CPU 的 KPI 只能靠它達成。三個 patch 彼此獨立、可以個別移除，所以一起套用，個別退出。

## Validation

- `python tools/vendor_gpui_windows.py --check`：vendored crate 與「pin rev 加上 3 個 patch」重新產生的結果逐位元組相同，而且 build 確實使用它。建議放進 CI。
- `python tools/vendor_gpui_windows.py --unit-tests`：`vsync.rs` 的 5 個單元測試，涵蓋 frame 需求與 park 策略。
- `cargo build`、`cargo test --workspace`、兩種 clippy（`-D warnings`）、`cargo fmt --all -- --check`、`tools/check_engine_isolation.py`、`tools/license_report.py --all-features --check`。
- B-8 配對量測（〈量測〉、`docs/benchmarks/b8-app.md`〈第九輪〉）：KPI 達成、互動沒有退步、截圖相同。
- 重畫檢查：vsync thread park 之後，最小化／還原、最大化／還原、滾輪、深淺色切換都立刻正確重畫。
- 每次升級 GPUI pin：重新產生 vendored crate，重跑以上全部，包括 B-8 配對量測。
