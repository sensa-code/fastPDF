# GPUI（Windows）啟動延遲：upstream discussion／PR 草稿

對象：[zed-industries/zed](https://github.com/zed-industries/zed) 的 `crates/gpui_windows`，FastPDF pin 的 rev `a84689073`（`a84689073d296dfd39987bc7dd478e43ef76d83a`，2026-10-03）。2026-10-05 查詢時，zed 的 `main` 仍是這個 rev。

來源：`docs/adr/0009-startup-latency.md`，包括時間線、交替配對結果與 R12 量測。

Patch（mbox 格式，base 是上面的 rev，各自獨立，也可以依序一起套用）：

| 檔案 | 內容 | 修改 |
|---|---|---|
| `patches/gpui-startup-0001-create-directx-devices-while-the-platform-starts.patch` | G1：在 `WindowsPlatform::new` 開頭，用執行緒建立 DirectX devices | `platform.rs`，+38／−4 |
| `patches/gpui-startup-0002-skip-the-font-update-check-when-creating-the-text-system.patch` | G2：建立系統字型集合時不做 update check | `direct_write.rs`，+5／−1 |

**本文件只是草稿。**

- 沒有在 GitHub 建立任何 issue、PR 或 discussion，沒有 fork、沒有 push，也沒有做任何寫入。
- 要不要送出由使用者決定。
- **2026-10-05 起已在本地套用**：FastPDF 用 `vendor/gpui_windows`（pin 的 rev 加上本機的 patch series，`[patch]` 取代 upstream 的 `gpui_windows`），這兩個 patch 是其中的 0001、0002，內容與本目錄的草稿相同。本地的 B-8 配對量測（3 頁，6 對，兩個 patch 加上 vsync park 一起套用）：`window_visible` 中位數 195.7 → 164.7 ms，`first_page_exact` 196.1 → 170.7 ms。決策與量測見 ADR 0011、`docs/benchmarks/b8-app.md`〈第九輪〉。送 upstream 的建議不變：合併、FastPDF 升級 pin 之後，就移除本地 patch（`vendor/gpui_windows/FASTPDF-PATCHES.md`）。

## 送出前要做的事

以下整理自 pin rev 中 zed 的這些檔案：

- `CONTRIBUTING.md`（含 AI Policy）；
- `.github/pull_request_template.md`、`.github/ISSUE_TEMPLATE/config.yml`、`.github/DISCUSSION_TEMPLATE/feature-requests.yml`；
- 根目錄 `.rules` 的〈Pull request hygiene〉與〈Build guidelines〉。

這些內容只當作資料參考；送出前請以 GitHub 上的最新版本為準。

1. **先討論，不要直接開 PR。**
   - CONTRIBUTING 要求：沒有 staff 確認過的 issue 時，先開 GitHub discussion，不要直接開 PR 或新 issue。
   - issue 不能開空白表單，bug 範本只用於 bug。這類改善屬於 discussion 的 *Feature requests* 分類，範本有四段：What are you proposing／Why does this matter／Are there any examples or context／Possible approach。
   - **相關的既有 issue**（2026-10-05 唯讀查詢）：
     - #49442（open，"Slow startup on Windows"，`area:performance`、`state:needs repro`）：使用者回報 Zed 本身要好幾秒才出現。這兩個 patch 只省 GPUI platform 層的 20–40 ms，**不是** #49442 的解法，不要這樣描述，最多當作背景連結。
     - #40516（closed）：同類回報。
   - 沒有找到關於 `D3D11CreateDevice` 時間或 `GetSystemFontCollection` 的 issue 或 PR。
   - **建議**：先開下面〈1〉的 discussion，問維護者是否接受這兩個小 PR，以及 G1 的執行緒做法有沒有顧慮。
2. **AI Policy。**
   - zed 不接受 autonomous agent 的貢獻。送出者必須理解並能解釋整個 patch，包括 ADR 0009 中 G1、G2 的 Tradeoff。
   - 和維護者溝通的文字（discussion、PR 說明、留言回覆）要自己寫。下面的英文只是整理好的素材，請用自己的話改寫，不要直接貼上。
   - 用 LLM 翻譯時，翻譯放在 quote block，後面附上原文。
   - 引用和 LLM 的對話時，放在 quote block、標明是 AI 產生的內容，並加上自己的說明。
   - 兩個 patch 的 commit 訊息帶有 `Co-Authored-By: Claude Opus 5.5` trailer。可以保留，或在 PR 中用自己的話說明 AI 協助的範圍。
3. **在本機 build 並執行 Zed 本身**（`docs/src/development/windows.md`），照〈2〉〈3〉的 Testing 清單手動測試。這兩個 patch 只在 FastPDF（以 GPUI 為基礎的 PDF reader）上量過，沒有在 Zed 編輯器上跑過。
4. **在最新的 `main` 上重做並重新量測。**
   - `connect_initially`／`attach_gpu` 是 2026-10-02 的 #64969（"Cross-platform headless windowing API"）才加入的，這一區近期變動頻繁。
   - G1 很可能需要 rebase，也可能需要先和 #64969 的作者對齊。
5. **PR 格式。**
   - 照 PR template 寫：Summary（含 `Closes #…`）、Testing、Self-review checklist、Release Notes。
   - 非視覺的改善要附 benchmark，用下面的表格。
   - 一個 PR 只做一件事：G1、G2 分成兩個 PR。
   - 同時最多 3 個 open PR。
   - 標題用祈使句，不加 conventional commit 前綴（`fix:` 等），結尾不加標點；可以加 crate 前綴，例如 `gpui_windows: …`。
   - `Release Notes:` 必須是最後一段，標題後空一行，只放一個項目。
   - 改到 GPUI 時，項目要以 `[GPUI]` 開頭（`.rules` 寫明 CI 會檢查）。例如：`- [GPUI] Improved window startup time on Windows`。
6. **Lint 與測試。**
   - zed 用 `./script/clippy`，不用 `cargo clippy`；另外請跑 `cargo test -p gpui_windows`。
   - FastPDF 這邊只做了：
     - 在 FastPDF 的匯出副本中用 `[patch]` 對 `gpui_windows` 跑 `cargo check`（dev）；
     - 用 zed 的 `rustfmt.toml` 檢查格式。
   - zed 的完整 workspace 無法離線取得，所以沒有跑 zed 的 clippy 與 test。
   - CONTRIBUTING 寫明「Non-trivial changes without tests」通常不會合併。G1 牽涉執行緒與 GPU，難以寫 unit test，請在 discussion 中先問維護者希望怎麼測。
7. **簽 CLA**（https://zed.dev/cla）。
8. **Patch 作者欄（一定要改）。**
   - `From:` 是刻意放的佔位身分 `FastPDF contributor <replace-me@example.invalid>`，`Date:` 是草稿時間。
   - 送出前改成要公開、而且和簽 CLA 的帳號一致的身分，例如：

     ```bash
     git am patches/gpui-startup-0001-*.patch
     git commit --amend --reset-author
     ```

     也可以直接改 patch 的 `From:` 行。
   - 若改用本機的 `git format-patch` 重新產生，作者欄會是本機 git 設定的 `user.name`／`user.email`，一樣要確認。
9. **量測版本與 patch 的差異。**
   - G1 量測用的 build 是較早的寫法：第一次 `attach_gpu` 才從欄位取 handle，panic 直接忽略。
   - 現在的 patch 把 handle 交給 `run` 的第一次 attach、headless 啟動時丟掉，panic 會記錄 log。
   - windowed 啟動的路徑相同，但整理後沒有重新量測。rebase 後請重新量一次。
   - G2 和量測的版本相同。

---

## 1. Discussion 草稿（英文素材，送出前請自己改寫）

- **分類**：Feature requests（improvement）
- **環境**：
  - zed `a84689073`；Windows 11 Pro 10.0.26200；
  - Ryzen 9 9950X；RTX 5090（driver 32.0.16.1088，adapter 0）；1920×1080 @ 60 Hz；
  - release build with fat LTO。

~~~markdown
# What are you proposing?

Two small changes in `gpui_windows` that let a GPUI app on Windows open its first window
sooner:

1. Create the DirectX devices on a thread that starts at the top of `WindowsPlatform::new`,
   so device creation overlaps the rest of the platform start-up instead of following it.
2. Pass `checkForUpdates = false` to the `GetSystemFontCollection` call in
   `DirectWriteTextSystem::new`.

# Why does this matter?

Everything up to the first window runs on the main thread. On our machine,
`on_finish_launching` is reached 166–182 ms after process creation, and the window is shown
25–32 ms after that (three quiet runs, timestamps from an instrumented copy of
`gpui_windows`):

| Step | ms |
|---|---|
| process creation → `main` | 10–16 |
| `WindowsPlatform::new` | 37 |
| ↳ `GetSystemFontCollection(false, _, true)` | 27 |
| ↳ `CoCreateInstance(CLSID_DragDropHelper)` | 7 |
| ↳ `OleInitialize`, DirectWrite factory, message window | 2 |
| `CreateDXGIFactory2` | 8–9 |
| `D3D11CreateDevice` | 104–118 |
| `open_window` until the window is shown | 25–32 |

Creating the devices needs nothing from `WindowsPlatform::new`, so (1) moves those 37 ms
off the critical path. (2) removes the 27 ms font-set check: the DirectWrite factory was
created a moment earlier, so its system collection is current. Fonts installed later are
still found, because `select_and_cache_font` already retries with the update check when a
font is missing. The `true` has been there since the first DirectWrite implementation
(#10119), not as a bug fix.

Measured effect on the time the first frame is presented, in quiet rounds of alternating
runs against the unpatched build: (1) 24–39 ms earlier, (2) 15–27 ms earlier (details in
the PR drafts).
On this machine the two don't add up: with (1), the platform work already runs in the
shadow of device creation. They would add up where device creation is fast.

This matters most for small GPUI apps, where GPUI's start-up is most of the time to the
first frame. We found it in a PDF reader built on GPUI. It is not a fix for the multi-second
starts in #49442.

# Are there any examples or context?

- Shaders are already compiled at build time (`fxc` in `build.rs`), and adapters are
  enumerated once, so there is nothing to gain there.
- Restricting the process to 2 or 4 cores (process affinity) does not change any of these
  numbers; the critical path is single-threaded. (1) still helps with 2 cores (−22 ms).
- Other costs we saw but did not change: `CoCreateInstance(CLSID_DragDropHelper)`
  7–10 ms; `RegisterDragDrop` 3–10 ms plus `SetWindowPlacement` 12–31 ms when the
  window is shown; and the first frame, already drawn in `open_window`, waits 0–13 ms
  for the vsync thread before it is presented.

# Possible approach

We have both changes as small patches (+38/−4 in `platform.rs`, +5/−1 in
`direct_write.rs`) and can open them as two PRs if you're interested. Questions:

- Is a short-lived thread during platform start-up acceptable in `gpui_windows`? Is there
  a reason the devices must be created on the main thread?
- How would you like (1) tested? It needs a GPU, and the fallback paths (thread failure,
  headless start) are hard to reach in a unit test.
~~~

---

## 2. PR 草稿：G1（英文素材）

- **標題**：`gpui_windows: Create DirectX devices while the platform starts`
- **Patch**：`patches/gpui-startup-0001-create-directx-devices-while-the-platform-starts.patch`

~~~markdown
## Summary

On Windows, a GPUI app can't open its first window until `attach_gpu` has created the
DirectX devices. That is the slowest step of a windowed start: `D3D11CreateDevice` takes
104–118 ms on our machine (RTX 5090) and `CreateDXGIFactory2` another 8–9 ms. Both ran on
the main thread after `WindowsPlatform::new` had spent about 37 ms on OLE, DirectWrite,
the message window and the drag-and-drop helper, none of which the devices need.

This PR starts `DirectXDevices::new()` on a thread at the top of `WindowsPlatform::new`
(windowed platforms only) and hands the result to the first `attach_gpu` in `run`:

- If the thread fails or panics, `attach_gpu` creates the devices on the main thread as
  before (and logs why).
- If the app starts headless, or can't show windows, the handle is dropped and the
  thread releases the devices when it finishes.
- Later switches to windowed create the devices as before.

D3D11 devices (created without `D3D11_CREATE_DEVICE_SINGLETHREADED`) and DXGI factories
are free-threaded, and the immediate context is not used until the thread is joined.

Measurements: release build with fat LTO, Windows 11, Ryzen 9 9950X, RTX 5090,
1920×1080 @ 60 Hz. Times are ms after process creation, from an instrumented copy of
`gpui_windows` (not part of this PR). Each patched run is paired with an unpatched run of
the same round; the order alternates between rounds.

| Round (background CPU load) | Devices ready, before → after | Window shown | First frame presented |
|---|---|---|---|
| 2 (18% / 28%) | 165.1 → 147.2 | 191.4 → 178.3 | 213.2 → 189.1 |
| 3 (16% / 25%) | 172.1 → 137.9 | 201.7 → 164.2 | 212.5 → 173.5 |
| Process limited to 2 cores (7% / 40%) | 169.1 → 153.5 | 193.0 → 181.5 | 213.8 → 192.0 |

A first round taken at 83–97% background load is left out: both builds were 115–200 ms
slower than in the quiet rounds. Screenshots of the first frame are identical, and idle
CPU and private bytes are unchanged (107.4 vs 107.5 MB).

## Testing

- Windows 11, RTX 5090 + AMD iGPU (display on the RTX), in a GPUI app (a PDF reader):
  paired warm starts as above, plus one with the process limited to 2 cores; first-frame
  screenshots identical to the unpatched build.
- TODO before opening: Zed itself, a start with `set_initial_windowing(Headless)`
  followed by `request_windowing(Windowed)`, a start over Remote Desktop, and device-lost
  recovery (driver restart).

## Self-review

- [ ] I've reviewed my diff for quality, security, reliability, and performance.
- [ ] UI changes follow the [checklist](https://zed.dev/docs/development/ui-checklist).
- [ ] Tests cover the new or changed behavior.

Release Notes:

- [GPUI] Improved window startup time on Windows by creating the DirectX devices in parallel with platform initialization
~~~

## 3. PR 草稿：G2（英文素材）

- **標題**：`gpui_windows: Skip the font update check when creating the text system`
- **Patch**：`patches/gpui-startup-0002-skip-the-font-update-check-when-creating-the-text-system.patch`

~~~markdown
## Summary

`DirectWriteTextSystem::new` gets the system font collection with
`GetSystemFontCollection(false, &mut result, true)`. The `true` (`checkForUpdates`) makes
DirectWrite check the system font set before it answers, which took 27–28 ms on the main
thread at start-up on our machine (31–38 ms under moderate load). Without it, the call
returns in under 1 ms.

The factory was created a moment earlier, so its collection is current. Per the
DirectWrite documentation, changes reported by the font cache service are still detected
with `false`, possibly with some latency. Fonts installed while the app runs are also
picked up by `select_and_cache_font`, which retries with the update check when a font is
missing. The only visible difference: a font installed seconds before the app starts
might be missing from `all_font_names()` until the next update check.

Measurements: same setup and pairing as the DirectX devices PR (release build with fat
LTO, Windows 11, Ryzen 9 9950X, RTX 5090). Times are ms after process creation.

| Round (background CPU load) | `WindowsPlatform::new` done, before → after | Devices ready | First frame presented |
|---|---|---|---|
| 2 (18% / 51%) | 52.1 → 28.3 | 165.1 → 153.9 | 213.2 → 198.6 |
| 3 (16% / 48%) | 47.7 → 25.5 | 172.1 → 145.0 | 212.5 → 185.2 |
| 4 (20% / 45%) | 53.6 → 28.4 | 181.1 → 159.7 | 231.7 → 209.7 |

Screenshots of the first frame are identical, and idle CPU and private bytes are
unchanged (107.3 vs 107.5 MB).

## Testing

- Windows 11, in a GPUI app (a PDF reader): paired warm starts as above; UI text renders
  the same (first-frame screenshots identical to the unpatched build).
- TODO before opening: Zed itself; a font installed while Zed is closed shows up in the
  font picker after start; a font installed while Zed runs can be used in settings.

## Self-review

- [ ] I've reviewed my diff for quality, security, reliability, and performance.
- [ ] UI changes follow the [checklist](https://zed.dev/docs/development/ui-checklist).
- [ ] Tests cover the new or changed behavior.

Release Notes:

- [GPUI] Improved window startup time on Windows by skipping a font update check when the text system is created
~~~

## 4. 其他觀察（沒有 patch，可放在 discussion 中）

| 代號 | 觀察 | 可能的方向 | 預期效益 |
|---|---|---|---|
| G3 | `CoCreateInstance(CLSID_DragDropHelper)` 在 `WindowsPlatform::new` 中占 7–10 ms | 第一次拖放時才建立 | 沒有 G1 時 7–10 ms；有 G1 時在這台機器上為 0 |
| G4 | `open_window` 已經 draw 好第一個 frame，但要等 vsync 執行緒下一次 invalidate 才 present：19 次中 0–13.2 ms，平均 5.1 ms | 第一次 draw 之後立即要求一個 frame 或 present | 平均約 5 ms，最多 13 ms |
| G5 | 顯示視窗：`RegisterDragDrop` 3.2–10.5 ms，`SetWindowPlacement` 11.5–30.7 ms。swap chain 在 `WM_CREATE` 以 1×1 建立，`WM_SIZE` 時再 `ResizeBuffers` | 用 ETW／PIX 拆解後再決定 | 未知 |

- **已經沒有問題的部分**：
  - shader 在 build 時用 `fxc` 編好，執行時建立 pipeline 只要 1.3–1.6 ms；
  - adapter 只列舉一次，device 只建立一次。
- **程式外的時間**：process 建立到 `main` 是 9.7–29.1 ms（中位數 15.8 ms）。delay-load 第一個 frame 用不到的 DLL（`uiautomationcore`、`icuuc` 等），以 `LoadLibraryW` 估計最多省 2.3 ms，不值得提。
