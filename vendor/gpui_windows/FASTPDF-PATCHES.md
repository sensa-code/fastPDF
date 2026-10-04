# FastPDF 對 gpui_windows 的修改

- **來源**：[zed-industries/zed](https://github.com/zed-industries/zed) 的 `crates/gpui_windows`，rev `a84689073d296dfd39987bc7dd478e43ef76d83a`（2026-10-03），也就是 FastPDF root `Cargo.toml` pin 的 GPUI rev。
- **授權**：Apache-2.0，Copyright Zed Industries, Inc.。全文見本資料夾的 `LICENSE-APACHE`，取自 zed repository 根目錄（upstream 的 crate 裡只是指向它的 symlink）。
- **為什麼在這裡**：見 `docs/adr/0011-local-gpui-patches.md`。FastPDF 的 GPUI 其餘部分仍從 zed 的 git 取得；只有這個 crate 經由 root `Cargo.toml` 的 `[patch."https://github.com/zed-industries/zed"]` 換成這份 vendored copy。這個資料夾不是 workspace member，FastPDF 的 lints、clippy 與 rustfmt 都不套用。
- **產生方式**：除了本檔，這個資料夾的檔案都由 `tools/vendor_gpui_windows.py` 從本機的 cargo git checkout 產生，**不要直接修改**。要改就改 `vendor/gpui_windows-patches/` 的 patch，再重新產生：

  ```powershell
  python tools/vendor_gpui_windows.py            # 重新產生
  python tools/vendor_gpui_windows.py --check    # 只比對，不同就 exit 1（升級 GPUI 或 CI 用）
  python tools/vendor_gpui_windows.py --unit-tests   # 單獨編譯 src/vsync.rs，跑它的單元測試
  ```

  `--check` 另外確認 build 真的用到這份 crate：root `Cargo.toml` 有 `[patch]` 這一項，而且 `Cargo.lock` 裡沒有其他來源的 `gpui_windows`。`[patch]` 沒被用到時（例如 upstream 改了版本號），cargo 只會警告，然後照樣用 zed 的原版建置，patch 就默默失效。

  `--unit-tests` 存在的原因：這個 crate 自己的 test target 需要 gpui 的 `test-support` feature，會用到 FastPDF 沒有建置的 crate（例如 `proptest`），離線無法編譯。

## 與 upstream 的差異

| 檔案 | 差異 |
|---|---|
| `Cargo.toml` | 獨立的 manifest，開頭有修改聲明（細節見下） |
| `LICENSE-APACHE` | upstream 是 symlink（部分 checkout 裡是內容為 `../../LICENSE-APACHE` 的 stub）；這裡放 zed repository 根目錄的全文 |
| 下表 patch 改到的 `.rs` 檔 | 第一行是修改聲明：`// Modified by FastPDF: <patch 標題> (<編號>). See FASTPDF-PATCHES.md.`（Apache-2.0 §4(b)） |

其餘檔案（`build.rs`、`.hlsl` shader、其他原始檔）與 upstream 完全相同。

`Cargo.toml` 的改寫：

- zed workspace 的繼承全部展開，第三方依賴的版本需求與 features 和 zed workspace 的定義相同。
- zed 自己的 crate（`gpui`、`collections`、`gpui_util`、`scheduler`）改用和 FastPDF 相同的 git URL 與 rev，cargo 會把它們和 FastPDF 的 GPUI 統一，不會出現第二份 gpui。
- `edition`、`publish` 直接寫明；`[lints]` 留空，不套用 zed 的，也不套用 FastPDF 的。
- 結果：`Cargo.lock` 只少了 `gpui_windows` 的 `source` 一行；解析出的 473 個 package、它們的 features 與依賴關係，在預設與 `--all-features` 下都和改動前相同。

## Patches

位置：`vendor/gpui_windows-patches/`，依檔名順序以 `git apply -p3` 套用。這是本機套用的唯一來源；`docs/upstream-issues/patches/` 裡的是準備送 upstream 的草稿，彼此獨立、各自以 pin 的 rev 為基礎。

| # | 用途 | 修改的檔案 | 對應的 upstream 草稿 |
|---|---|---|---|
| 0001 | **G1**：在 `WindowsPlatform::new` 一開始，就在另一條執行緒建立 DirectX devices，第一次 attach 時 join。`D3D11CreateDevice`（約 100–120 ms）和 OLE、DirectWrite、message window 的初始化同時進行。 | `platform.rs` | `docs/upstream-issues/gpui-startup.md` §2；`patches/gpui-startup-0001-create-directx-devices-while-the-platform-starts.patch`（內容相同） |
| 0002 | **G2**：建立系統字型集合時不做 update check（`GetSystemFontCollection(…, false)`），主執行緒少約 27 ms。 | `direct_write.rs` | `docs/upstream-issues/gpui-startup.md` §3；`patches/gpui-startup-0002-skip-the-font-update-check-when-creating-the-text-system.patch`（內容相同） |
| 0003 | **vsync park**：實作 `PlatformWindow::frame_waker`；沒有視窗要求 frame 超過 1 秒，vsync thread 就 park，有要求時立刻喚醒並 invalidate。park 期間每秒醒來一次檢查 device lost。 | `direct_manipulation.rs`、`events.rs`、`platform.rs`、`vsync.rs`、`window.rs` | `docs/upstream-issues/gpui-idle.md`；`patches/gpui-idle-0001-park-vsync-thread-when-idle.patch`（rebase 過，見下） |

### 0003 的衝突處理

0003 原本以 pin 的 rev 為基礎。套在 0001 之後，`platform.rs` 中 `WindowsPlatform::new` 結構初始化那個 hunk 的 context 對不上：0001 在 `drop_target_helper,` 和 `invalidate_devices: …,` 之間加了 `early_devices: RefCell::new(early_devices),`。

處理方式：把 0003 的 `frame_demand: Arc::new(FrameDemand::new()),` 放在 `invalidate_devices: …,` 之後，也就是 `early_devices` 欄位之後，順序和 upstream 草稿相同。其他 hunk 只有行號位移。本機的 `0003-park-the-vsync-thread-while-no-window-wants-frames.patch` 就是 rebase 後的版本，和草稿相比只差這個 hunk 的 context、行號與 blob hash；新增與刪除的程式碼完全相同（+227／−10）。

兩個 patch 在語意上沒有交集：0001 改 device 的建立時機（`new`、`connect_initially`、`attach_gpu`），0003 改 vsync thread 的迴圈與視窗的 frame 要求，以及 `detach_gpu` 與 `Drop` 中喚醒 thread。

## 何時移除

- **單一 patch**：upstream 合併了它（或等效的改動），而且 FastPDF 的 GPUI pin 已升級到包含它的 rev 時，刪掉對應的 patch 檔，重新產生。
- **全部移除**：三個 patch 都不需要時，刪掉 `vendor/gpui_windows/`、`vendor/gpui_windows-patches/`、`tools/vendor_gpui_windows.py`，root `Cargo.toml` 的 `[patch]` 與 `exclude` 中的這一項，以及 `tools/ported-sources.json` 的條目；下一次 `cargo build --offline` 會在 `Cargo.lock` 補回 `gpui_windows` 的 git `source`。重新產生 `THIRD_PARTY_LICENSES.md`，並把 ADR 0011 標為 Superseded。
- **升級 GPUI pin 時**：先在 root `Cargo.toml` 改 rev，讓 cargo 取得新的 checkout，再執行 `python tools/vendor_gpui_windows.py`。
  - patch 套不上：照上面的方式 rebase，在本檔記錄；
  - 若改動太大，放棄這個 patch，依 ADR 0011 的退出計畫回到 upstream 的行為。
  - 之後重新跑 B-8 配對量測，確認效果還在。
