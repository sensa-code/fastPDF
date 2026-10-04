# GPUI（Windows）idle 喚醒：upstream issue／PR 草稿

對象：[zed-industries/zed](https://github.com/zed-industries/zed) 的 `crates/gpui_windows`，FastPDF pin 的 rev `a84689073`（`a84689073d296dfd39987bc7dd478e43ef76d83a`，2026-10-03）。
來源：

- idle CPU 歸因（`docs/benchmarks/b8-app.md`〈Idle 歸因與 KPI（第六輪）〉）；
- GPUI audit 的 Idle Behavior（`docs/audit/gpui.md`）；
- PROJECT_AUDIT 的 R9。

Patch：`patches/gpui-idle-0001-park-vsync-thread-when-idle.patch`（`git diff` 格式，base 是上面的 rev，只改 `crates/gpui_windows`，5 個檔案，+227／−10 行）。

**本文件只是草稿：沒有在 GitHub 建立任何 issue、PR 或 discussion，沒有 fork、沒有 push，也沒有做任何寫入。要不要送出由使用者決定。** FastPDF 本身仍使用原本的 GPUI，沒有套用這個 patch。

## 送出前要做的事

以下整理自 zed 的 `CONTRIBUTING.md`（含 AI Policy）與 `.github/pull_request_template.md`，版本是 pin rev 中的檔案。內容只當作資料參考，送出前請以 GitHub 上的最新版本為準。

1. **先討論，不要直接開 PR。**
   - CONTRIBUTING 要求：沒有 staff 確認過的 issue 時，先開 GitHub discussion，不要直接開 PR 或新 issue。
   - 相關的既有討論（2026-10-04 唯讀查詢，見 `docs/audit/gpui.md`）：
     - #63184（open）：成本隨更新率線性成長，240 Hz 是 60 Hz 的 4 倍；
     - #58048（open）：螢幕關閉後風扇仍在轉；
     - #63182（2026-09-17 closed，"Make frame scheduling demand-driven"）：維護者表示內部另有重構計畫；
     - #63438（open PR）：螢幕關閉時停止 render。
   - 建議：先在 #63184 留言附上下面〈1〉的數據，問維護者是否接受只動 `gpui_windows` 的小 PR，或內部重構是否已經涵蓋。#63182 被關掉的原因要先問清楚。
2. **AI Policy。**
   - zed 不接受 autonomous agent 的貢獻。送出者必須理解並能解釋整個 patch，包括下面〈4〉列出的取捨。
   - 和維護者溝通的文字（issue、PR 說明、留言回覆）要自己寫。下面的英文只是整理好的素材，請用自己的話改寫，不要直接貼上。
   - 用 LLM 翻譯時，翻譯放在 quote block，後面附上原文。
   - 引用和 LLM 的對話時，放在 quote block、標明是 AI 產生的內容，並加上自己的說明。
3. **在本機 build 並執行 Zed 本身**（`docs/src/development/windows.md`），照〈5〉的清單手動測試。這個 patch 只在 FastPDF 與最小 GPUI 視窗上量過，沒有在 Zed 編輯器上跑過。
4. **在最新的 `main` 上重做。** pin rev 之後 `gpui_windows` 可能已經改了（例如 #63182 提到的內部重構），patch 可能需要 rebase。
5. **PR 格式。**
   - 照 PR template 寫：Summary（含 `Closes #…`）、Testing、Self-review checklist、Release Notes。
   - 非視覺的改善要附 benchmark。
   - 一個 PR 只做一件事。
   - 同時最多 3 個 open PR。
6. **簽 CLA**（https://zed.dev/cla）。
7. **Patch 作者欄。**
   - `patches/` 裡的 patch 是 `git diff` 輸出，本身沒有作者資訊。
   - 若用 `git commit`／`git format-patch` 做成正式 commit，作者欄（`From:`）會是本機 git 設定的 `user.name`／`user.email`。送出前確認那是要公開的身分，而且和簽 CLA 的帳號一致。
8. **測試。**
   - patch 新增了 3 個 unit test（`vsync.rs`：demand 只被取走一次、linger 之後才 park、`request()` 能喚醒 park 中的 thread）。
   - 在 FastPDF 這邊做過的檢查：
     - 3 個 test 抽到獨立 crate 執行通過（原文照搬）；
     - `cargo clippy -p gpui_windows`（lib）沒有警告；
     - rustfmt（1.99.0、edition 2024）沒有差異；
     - FastPDF 與最小視窗的 dist build 實際執行（見〈4〉）。
   - 沒做到的：`gpui_windows` 的 lib test 在 FastPDF 的依賴圖下編譯不過。這是既有程式的問題：`render_to_image` 需要 `gpui/test-support`，與這個 patch 無關。
   - 送出前請在 zed workspace 跑 `cargo test -p gpui_windows` 與 `cargo clippy -p gpui_windows --all-targets`。
9. **套用 patch。**
   - patch 是 LF 換行，在 LF 的 checkout 上可以直接 `git apply`（已確認可套用到 `a84689073`）。
   - 若 clone 時 `core.autocrlf=true`，檔案是 CRLF，`git apply` 會失敗。可以用 `git -c core.autocrlf=false` 重新 checkout，或改用 `git apply --ignore-whitespace`；後者新增的行會是 LF，commit 前要再檢查換行。

---

## 1. Issue／留言草稿（英文素材，送出前請自己改寫）

- **Type**：Bug／Performance（Windows）
- **Version**：zed `a84689073`；Windows 11 Pro 10.0.26200；Ryzen 9 9950X；RTX 5090（driver 32.0.16.1088）；1920×1080 @ 60 Hz、100% 縮放

~~~markdown
### Summary

On Windows, an idle GPUI window keeps two threads busy at the display refresh rate.

The `VSyncProvider` thread (`gpui_windows/src/platform.rs`, `begin_vsync_thread`) waits
for every vblank with `DwmFlush` and then calls `RedrawWindow(RDW_INVALIDATE)` for every
window, whether or not any window wants a frame. Each invalidation wakes the UI thread for
a `WM_PAINT` that goes through `draw_window` into the `on_request_frame` callback, which
finds nothing dirty and returns.

So a window that shows a static UI costs about 60 + 60 wake-ups per second at 60 Hz
(more at higher refresh rates, see #63184), which keeps laptop CPUs out of deep idle
states (#58048).

### Measurements

A minimal GPUI app (one view painting a solid background, 826×918 window, release build
with fat LTO), idle for 10 s after the first frame, per-thread numbers from
`QueryThreadCycleTime` and the context-switch counts of `NtQuerySystemInformation`:

| | UI thread | `VSyncProvider` | whole process |
|---|---|---|---|
| wake-ups/s | 109.5 | 65.3 | 236 |
| Mcycles/s | 18.0 | 10.8 | 31.2 |
| CPU time / 10 s | | | 78 ms (0.78% of one core) |

The rest of the process's wake-ups (about 60/s, 2–3 Mcycles/s) come from one thread of
the NVIDIA D3D11 user-mode driver, which is outside GPUI.

### Possible fix

GPUI already has the hook for this: `PlatformWindow::frame_waker`. `WindowInvalidator`
calls it when a window becomes dirty and when next-frame callbacks or a throttled frame
are pending, so that "platforms that stop requesting frames for idle windows" get a
wakeup. The web and test platforms implement it; Windows does not.

Implementing it on Windows lets the vsync thread park when no window has asked for a frame
for a while, and wake up (and invalidate immediately) when one does. I have a patch that
does this (about 230 lines, all in `gpui_windows`) and measured it in an app; happy to open
a PR if this direction is acceptable, or to adapt it to the refactoring mentioned in #63182.
~~~

## 2. PR 說明草稿（英文素材，送出前請自己改寫）

~~~markdown
## Summary

gpui_windows: park the vsync thread while no window wants frames.

The vsync thread used to invalidate every window on every vblank, so an idle window woke
the UI thread at the refresh rate only for `request_frame` to find nothing to draw.

This implements `PlatformWindow::frame_waker` on Windows. Windows report frame demand
through it (GPUI calls it when a window becomes dirty or has pending next-frame callbacks),
and the vsync thread:

- keeps ticking as before while frames were requested within the last second. This covers
  GPUI's one-second "keep presenting after high-rate input" window and every place that
  relies on the next vsync to re-invalidate a window;
- then parks until the next request, and invalidates right away when woken, instead of
  first waiting for another vblank;
- still wakes once per second while parked to check for a lost device, so an idle window
  recovers after a driver reset without input.

Platform work that needs vsync ticks without a dirty window also requests frames:
Direct Manipulation contacts and running gestures/inertia (they are polled from
`draw_window`), and draws deferred by the `DrawCoordinator`.

Closes #…

## Testing

- Unit tests for the demand flag and the park policy (`vsync.rs`).
- Measured in a minimal GPUI app and in an app with smooth scrolling, keyboard
  navigation and Ctrl+wheel zoom (Windows 11, RTX 5090, 60 Hz), A/B alternating runs:
  - idle (10 s after the first frame): UI thread wake-ups 110–230/s → 0/s, vsync thread
    64–100/s → 1/s, process Mcycles/s 49 → 4.9 (median of 4 pairs), CPU time 1.41% → 0.08%
    of one core;
  - in-app input-to-paint latency (p50 8.0 → 6.8 ms, max 21.0 → 20.5 ms) and frame
    intervals during smooth scrolling (p50 17.1 → 17.8 ms): no regression;
  - first input after idle to paint: 4.2 → 0.6 ms, because a woken vsync thread
    invalidates right away instead of waiting for the next vblank;
  - (to confirm before sending: externally captured zoom latency, which is quantized to
    the capture interval.)
- Manually: …

## Self-review

- [ ] I've reviewed my diff for quality, security, reliability, and performance.
- [ ] UI changes follow the checklist.
- [ ] Tests cover the new or changed behavior.

Release Notes:

- Reduced CPU wake-ups of idle windows on Windows.
~~~

## 3. Patch 說明（給審查者與 FastPDF 自己）

| 檔案 | 改動 |
|---|---|
| `vsync.rs` | 新增 `FrameDemand`（`AtomicBool` 加上 vsync thread 的 `Thread` handle，`request()` 設旗標並 `unpark`）、`IdleTracker`（最後一次 demand 後超過 `IDLE_FRAME_LINGER` = 1 s 才 park）、兩個常數，以及 3 個 unit test |
| `platform.rs` | `WindowsPlatform` 持有 `Arc<FrameDemand>`，傳給每個視窗；vsync 迴圈每個 tick 取走 demand，閒置超過 1 s 就 `park_timeout(1 s)`；醒來後不等 vblank、立刻 invalidate；park 期間每秒檢查一次 device lost；`detach_gpu` 與 `Drop` 設 stop 旗標後喚醒 thread |
| `window.rs` | `WindowsWindowState` 持有 `Arc<FrameDemand>`；實作 `PlatformWindow::frame_waker`，回傳呼叫 `request()` 的 closure |
| `events.rs` | Direct Manipulation 的 `DM_POINTERHITTEST`、手勢或慣性進行中、`DrawCoordinator` 延後的 draw，都會要求 frame |
| `direct_manipulation.rs` | `OnViewportStatusChanged` 記錄手勢是否在 RUNNING／INERTIA，提供 `is_gesture_active()` |

設計重點：

- **沿用 GPUI 既有的 hook。** `frame_waker` 已經是 `PlatformWindow` 的 API，`gpui` crate 本身不用改。
- **Linger 1 秒。** GPUI 在「造成 invalidation 的高頻輸入」後會持續 present 1 秒（`InputRateTracker`）；另外，延後的 re-entrant draw、按鍵 dispatch 時已經畫好但還沒 present 的 frame（#61469），都依賴下一個 vsync 再 invalidate 一次。保留 1 秒 linger，這些行為和原本完全相同。
- **醒來後立刻 invalidate。** 停了超過 1 秒，上一個 frame 早已完成，沒有必要先等一個 vblank。另外，`vsync.rs` 的註解提到，thread 剛從 idle 回來時第一次 `DwmFlush` 可能提早返回，這時原本的程式會改成 `sleep(interval)`，多等一整個 frame。醒來後的第一個 tick 不呼叫 `DwmFlush`，也就避開了這個情況。
- **Device lost。** park 期間每秒醒來一次，用 `GetDeviceRemovedReason` 檢查。這是 idle 時唯一剩下的週期性喚醒（1 次／秒）。若要完全歸零，可以改用 `ID3D11Device4::RegisterDeviceRemovedEvent` 等事件，但改動較大，這個 patch 沒有做。

## 4. 量測（FastPDF 與 GPUI 最小視窗）

機器：AMD Ryzen 9 9950X（16C／32T）、128 GB、RTX 5090、Windows 11 Pro 10.0.26200、1920×1080 @ 60 Hz、100% DPI。工具：`tools/bench-app` 1.1.0（`-ThreadDetail -MemoryDetail -AppProbe`），dist build（fat LTO）。量測期間使用者的 WSL VM 與其他 agent 的工作仍在執行，各 run 的背景負載見 `docs/benchmarks/b8-app.md`。

### GPUI 最小視窗（一個 view 畫純色背景，826×918），idle 10 秒

| | 原本 | 加上 patch |
|---|---|---|
| 主執行緒喚醒／秒 | 109.5 | 0 |
| `VSyncProvider` 喚醒／秒 | 65.3 | 1.0 |
| NVIDIA driver thread 喚醒／秒 | 60.2 | 60.2 |
| 整個 process Mcycles／秒 | 31.2 | 2.9 |
| CPU time／10 s | 78 ms | 0 ms |
| private working set | 15.0 MB | 14.9 MB |

各 1 次。

### FastPDF（3 頁 PDF），交替配對 4 對（A B A B …）

FastPDF 數字見 `docs/benchmarks/b8-app.md`〈Idle 歸因與 KPI（第六輪）〉的表格；摘要：

- **idle**：
  - 主執行緒喚醒 139（113–234）→ 0（0–0.3）次／秒；
  - `VSyncProvider` 68（64–101）→ 1.1（1.0–1.2）次／秒；
  - 整個 process 49.2（34.4–65.0）→ 4.9（3.1–6.2）Mcycles／秒；
  - CPU time 1.41%（0.16–1.56%）→ 0.08%（0–0.16%）單核。
- **輸入與 frame**：
  - app 內的輸入到 paint 延遲：p50 8.0 → 6.8 ms，最大 21.0 → 20.5 ms；
  - 平滑捲動期間的 frame 間隔 p50：17.1 → 17.8 ms；
  - idle 後第一個輸入到 paint：4.2 → 0.6 ms（另外 3 對，A 端的 tile 大小有 build 瑕疵，見 b8-app.md）。
- **待確認**：外部截圖量到的縮放延遲中位數 24.9 → 28.9 ms。數值集中在 23 ms 與 42 ms 兩個截圖週期，A、B 各有 1 次 42 ms；patch 在互動期間不改變程式路徑（vsync thread 仍在 linger 中）。送出前建議用 app 內逐次輸入的時間戳記再確認。

## 5. 手動測試清單（送 PR 前，在 Zed 上）

- 游標閃爍（每 500 ms notify 一次：thread 不應 park）、spinner 與 agent panel 的串流輸出等動畫。
- 觸控板捲動、pinch、慣性捲動（Direct Manipulation）；觸控板手勢剛開始、畫面還沒變化時是否有反應。
- 按住 PageDown／方向鍵（#61469）；IME 輸入。
- 多個視窗、modal dialog、視窗最小化與還原、移到另一個螢幕、DPI 改變。
- 系統睡眠後喚醒、螢幕關閉（#58048）、遠端桌面。
- GPU device lost（driver 更新或 TDR）時，閒置中的視窗能在約 1 秒內恢復畫面。
- 144／240 Hz 與 VRR 螢幕。
- `GPUI_DISABLE_DIRECT_COMPOSITION=1`。

## 6. FastPDF 端的處理

- FastPDF 自己在 idle 時不要求任何 frame：probe build 量到 idle 10 秒內 render、prepaint、paint、wake 都是 0（見 b8-app.md）。所以 idle CPU 只能靠這個 GPUI patch 降低。
- 要套用時，FastPDF 可以在 workspace `Cargo.toml` 用 `[patch."https://github.com/zed-industries/zed"]` 指向帶 patch 的 zed，或等 upstream 合併後升級 rev。
  - Cargo.lock 中來自 zed repo 的 22 個 crate 都要一起 patch（`gpui`、`gpui_platform`、`gpui_windows`、`gpui_util`、`sum_tree`、`zlog` 等），否則 path 版與 git 版的 `gpui` 會同時存在。
  - 本輪實測時只在 `git archive` 匯出的副本中這樣做，Cargo.lock 只有這 22 個 crate 的 `source` 行消失，沒有其他版本變動。
  - 這屬於 GPUI 依賴策略的變更（ADR 0001），應另開 ADR 決定。
