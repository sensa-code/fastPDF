# ADR 0001 — Use GPUI (pinned to Zed main) for the UI

- 狀態：Accepted
- 日期：2026-10-04
- 相關 spec：§2、§20、§33、§34、§35；證據：`docs/audit/gpui.md`、`docs/architecture-current.md`

## Context

- Spec 要求 GPU-native、Windows 11 優先的 native Rust UI；明確排除 Electron／Chromium + PDF.js（§34），也暫不改用 Tauri（§35），除非 benchmark 或技術阻礙證明 GPUI 不適合，且必須先寫 ADR。
- M0 audit 的問題 Q3：GPUI 在 Windows 上有沒有會影響 reader 的 rendering、scrolling、texture、high-DPI 限制？
- GPUI 的發佈現況：
  - crates.io `gpui` 0.2.2（2025-10-22）已約 11.5 個月沒更新，落後 zed main 約 8,167 個 commit；Zed 成員在 #63471（2026-09-29）建議改用 repo 版本。
  - zed main 已把 gpui 拆成 `gpui`、`gpui_platform`、`gpui_windows` 等 crate，API 經常變動。
  - `gpui-pre` 是第三方（gpui-component 維護者）在 crates.io 重新發佈的 snapshot。
- 授權史：zed 的 `zlog`、`ztracing`（經由 `sum_tree` 成為 gpui 的依賴）在 2026-09-01（`ac5af8b9e1`，#63573）之前是 GPL-3.0-or-later。較舊的 zed rev 會把 GPL 帶進依賴圖（pdf-reader-gpui audit 在 `e30720a` 實際觀察到）。

## Decision

1. **繼續使用 GPUI**。audit 沒有找到 Windows blocker（Q3：rendering 中度、可繞過；scrolling 沒有阻擋；texture 高度但可控；high-DPI 沒有阻擋）。
2. **依賴策略 B**：從 zed repo 以 git 依賴 pin 一個 rev（目前 `a84689073d296dfd39987bc7dd478e43ef76d83a`，2026-10-03）。`gpui` 與 `gpui_platform` 在 `[workspace.dependencies]` 單點宣告、使用同一個 rev、`default-features = false`，`gpui` 只開 `windows-manifest`。
   - **rev 必須在 `ac5af8b9e1`（2026-09-01）之後**；每次升級都要跑 `tools/license_report.py --check`（CI 已包含）。
   - 升級節奏：每 4–8 週，或遇到需要的修正時。升級前以 GPUI probe（atlas churn／leak、idle CPU、time to window、scroll）與 `fastpdf-bench` 當門檻。
3. **不使用 gpui-component**：exe 多 6.2 MB、多 58 個 crate、clean build 時間翻倍，而且 0.6 起綁定 `gpui-pre`。需要的少量元件自己寫。
4. **GPUI 型別只出現在 `fastpdf-ui` 與 `fastpdf-app`**。viewer 的邏輯都在 toolkit 無關的 `fastpdf-core::DocumentSession`，升級 GPUI 的影響範圍只在一個 crate。
5. **Windows 建置設定**（比照 Zed）：`.cargo/config.toml` 設定 `-C target-feature=+crt-static`（不需要 VC++ redistributable）與 `--cfg windows_slim_errors`。release build 需要 Windows SDK 的 `fxc.exe`（可用 `GPUI_FXC_PATH` 指定）。
6. **Texture 生命週期由 FastPDF 管理**：
   - tile 透過 `TileCache` 的 eviction hook 一律呼叫 `drop_image`。GPUI 不會自動 evict，漏掉就會永久洩漏。
   - 每 frame 設 upload 預算，超過時先顯示 stand-in。
   - tile 尺寸固定（256／512）並對齊 device pixel。
7. **已知行為的處理**：
   - idle 時 vsync thread 仍以 refresh rate 喚醒（約 0.5–1% of 1 core）、持續按鍵時 present 會飢餓（#61469）。先在 FastPDF 層合併 notify、不做常駐動畫。量測證明需要時，再以 `[patch]` 套用小 patch（vsync park、key dispatch 後 present）。
   - 列印走 Win32。

### 例外：本地套用 gpui_windows 的 patch（2026-10-05，ADR 0011）

- `gpui_windows`（GPUI 的 Windows platform crate）改用 `vendor/gpui_windows`：pin rev 的原始碼，加上三個本地 patch：
  - 啟動時平行建立 DirectX device；
  - 啟動時不做字型 update check；
  - idle 時 park vsync thread。
- 經由 root `Cargo.toml` 的 `[patch]` 取代 upstream 的 crate；其餘 GPUI crate 仍是第 2 點的 git pin。第 7 點「量測證明需要時，再以 `[patch]` 套用小 patch」的條件，已由 B-8 配對量測滿足。
- 範圍只限這一個 crate。patch 另有 upstream 草稿；upstream 合併、FastPDF 升級 pin 之後就移除，見 ADR 0011 的退出計畫。
- 升級 GPUI 時，多一個步驟：`python tools/vendor_gpui_windows.py`。這一步會重新產生 vendored crate，並確認 patch 還能套用。

## Consequences

- 能拿到 0.2.2 之後的重要修正：atlas 空間回收（VRAM 630 → 333 MiB）、D3D11 upload 加速約 2 倍、觸控板 pinch 與像素捲動、多螢幕與 DPI 修正、IME 修正、關閉時的 deadlock 修正。啟動也快了約 2 倍（first frame 205–228 ms vs 403–489 ms）。
- 首次建置要抓 zed repo（約 351 MB）。CI 需要 cache cargo git。
- GPUI floor（最小 app）：exe 9.8 MiB（LTO + strip 6.3 MiB）、idle private working set 約 16 MiB、working set 約 68 MiB、commit 約 80 MiB。所以 spec §29 的「Idle RAM < 50 MB」必須明確定義指標（見 PROJECT_AUDIT Risks R8）。
- 每次升級都可能要改程式碼，例如 `Application::new()` 改成 `gpui_platform::application()`、`paint_image` 新增 `image_bounds` 參數。

## Alternatives considered

- **crates.io gpui 0.2.2**：不可變、最單純，但缺少上述修正，啟動慢約 2 倍、atlas 碎片化，官方也不建議。否決。
- **`gpui-pre` snapshot**：在 crates.io 上，但由第三方重新發佈，有供應鏈信任問題，而且和 gpui-component 綁在一起。保留為 CI 無法 clone zed 時的備案，前提是核對 snapshot 與 zed sha 一致。
- **Vendor GPUI 進 repo**：完全可控，但維護成本是好幾萬行。只有在需要深度修改（external texture、vsync）時才考慮。
- **Tauri／Electron／Chromium + PDF.js**：違反 spec §34、§35。

## Validation

- `tools/license_report.py --check` 在 CI 通過（確認依賴圖中沒有 GPL）。
- B-8：time to window、idle CPU／RAM 與 GPUI floor 比較。
- M5：tile churn 測試（反覆 zoom、捲動）下 VRAM 與 commit 不持續成長。
