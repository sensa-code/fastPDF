# 發佈流程（Release）

本文件是 FastPDF 發佈的檢查表與發佈形態的決策參考。

- **目前的發佈形態**：Windows x64 **可攜版 zip**，附 SHA-256，**沒有簽章、也沒有安裝程式**。
- **相關工具與程式**：
  - `tools/package.ps1`：打包與 smoke test；
  - `crates/fastpdf-app/build.rs`：exe 的 icon 與 VERSIONINFO；
  - `crates/fastpdf-shell`：per-user 檔案關聯的註冊與反註冊計畫。

> 原則（spec §9、§29、§52）：發佈流程不加入 auto-update、telemetry、網路功能。效能數字必須是實測值，達不到就照實寫。

---

## 1. 發佈檢查表

依序完成，每一步的輸出都記錄在 release notes 草稿中。

### 1.1 版本號

- [ ] 決定版本號（SemVer）。0.x 期間的 minor 可以包含不相容變更；`-beta.N` 之類的 pre-release 會讓 exe 的 `FILEFLAGS` 帶上 `VS_FF_PRERELEASE`。
- [ ] 修改 root `Cargo.toml` 的 `[workspace.package] version`（所有 crate 共用這個版本），然後執行 `cargo check --workspace` 更新 `Cargo.lock`。
- [ ] exe 的 VERSIONINFO 由 `build.rs` 從 Cargo 版本自動產生：
  - `FILEVERSION` 是 `major.minor.patch.0`，每個欄位必須 ≤ 65535，超過時 build 會失敗；
  - `ProductName`、`FileDescription` 為 `FastPDF`；
  - 不寫 `CompanyName`、`LegalCopyright`。在決定法律主體與授權之前不要加上（spec §37）。
- [ ] commit：`chore(release): vX.Y.Z`。

### 1.2 Toolchain 與乾淨的 build 環境

- [ ] Rust 以 `rust-toolchain.toml` 為準（目前 1.99.0）。如果這次 release 要升級 toolchain，必須先重跑 baseline（§1.4）。
- [ ] 需要 MSVC（VS 2022 Build Tools）與 Windows SDK：嵌入 icon 與 VERSIONINFO 會用到 SDK 的 `rc.exe`（經由 embed-resource）。
- [ ] 從**乾淨的 checkout** build：`git status` 必須沒有任何變更，不能混入他人尚未 commit 的修改。`package.ps1` 會在 `BUILDINFO.txt` 中標記 working tree 是否 dirty。
- [ ] 一律加上 `--locked`（`package.ps1` 已內建），確保使用的就是已 commit 的 `Cargo.lock`。

### 1.3 品質關卡

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
python tools/check_engine_isolation.py
python tools/license_report.py --check
```

### 1.4 Baseline 重跑與 compare（spec §30、§41）

```powershell
uv run tools/fixtures/generate.py --profile quick
cargo run --release -p fastpdf-bench -- corpus fixtures/generated/manifest.json --repeat 3 --timeout 120 --out benchmarks/runs/release-vX.Y.Z.json
cargo run --release -p fastpdf-bench -- compare benchmarks/baseline.json benchmarks/runs/release-vX.Y.Z.json --threshold 10
# App 層 KPI（B-8）：小檔與 300 頁文件
pwsh -File tools/bench-app/bench-app.ps1 -Preset fastpdf -Pdf fixtures/generated/small-text/three-pages-platypus-times.pdf -Runs 5
pwsh -File tools/bench-app/bench-app.ps1 -Preset fastpdf -Pdf fixtures/generated/large-text/dense-300p-times.pdf -Runs 5 -ScrollNotches 20 -PageDowns 5 -ZoomSteps 3
```

- [ ] `compare` 沒有超過 10% 的 regression。有的話要說明 Before／After／Why／Tradeoff，或修正後再發佈。
- [ ] 量測條件依 `benchmarks/README.md`：同一台機器、關閉重度負載、插電、warm cache 要註明。
- [ ] 把 KPI 表（spec §29：exe 大小、idle RAM、小 PDF 首頁、idle CPU）寫進 release notes。

### 1.5 授權（spec §36–§37）

- [ ] `python tools/license_report.py`：重新產生 `THIRD_PARTY_LICENSES.md`，`--check` 不可出現待審項目。依賴有變動時，逐一審查新增的 crate。
- [ ] 檢查 Apache-2.0 依賴是否有 `NOTICE` 檔。有的話，內容必須隨 binary 一起發佈。
- [ ] 檢查 `THIRD_PARTY_LICENSES.md` 的 Ported source 章節，確認所有移植自 upstream 的程式碼都有登記。
- [ ] **已知缺口（公開發佈 binary 前必須補上）**：
  - MIT／BSD／ISC／Zlib 要求隨 binary 附上**每個 crate 自己的 copyright 與授權全文**。目前 `licenses/` 只有 Apache-2.0 全文，`THIRD_PARTY_LICENSES.md` 只是清單。
  - 建議擴充 `tools/license_report.py`：從 cargo registry 的原始碼目錄收集各 crate 的 `LICENSE*`、`COPYING*`、`NOTICE*`，輸出到 `licenses/third-party/<crate>-<version>/`，再由 `package.ps1` 一併打包。這不需要新增任何工具。
- [ ] 確認 exe 中的資產授權：
  - app icon 是 `tools/icon/make_icon.py` 原創繪製，沒有使用第三方素材；
  - 內嵌字型（hayro 的 `embed-fonts` 標準字型）要確認授權已列在 THIRD_PARTY 中。

### 1.6 打包

```powershell
pwsh -File tools/package.ps1            # build + stage + zip + sha256 + 驗證 + smoke test
```

輸出：`dist/FastPDF-X.Y.Z-win-x64.zip` 與 `dist/FastPDF-X.Y.Z-win-x64.zip.sha256`。`dist/` 已被 git ignore。zip 的內容：

| 檔案 | 說明 |
|---|---|
| `FastPDF-X.Y.Z-win-x64/fastpdf.exe` | `--profile dist`：fat LTO、`codegen-units=1`、strip symbols |
| `README.md` | 專案 README |
| `THIRD_PARTY_LICENSES.md`、`licenses/` | 授權（見 §1.5 的已知缺口） |
| `BUILDINFO.txt` | 版本、git commit（含 dirty 標記）、rustc 版本、profile、build 時間、exe 的 SHA-256 |

`package.ps1` 會自動檢查：
- VERSIONINFO 與 Cargo 版本一致，且沒有 `CompanyName`；
- zip 的 entry 清單完全符合預期，沒有多也沒有少；
- zip 內 exe 的 SHA-256 等於 build 產物。

### 1.7 Smoke test

`package.ps1` 會先把 zip 解壓到 `dist/.smoke-*`，再執行：

1. `fastpdf --version`：輸出必須是 `fastpdf X.Y.Z`；
2. `fastpdf --help`：第一行必須是 `usage: fastpdf ...`；
3. **1 次 GUI 啟動**：
   - 環境變數：`FASTPDF_BENCH=1`，`FASTPDF_SETTINGS_FILE` 與 `FASTPDF_RECENT_FILE` 設為空值，不讀寫使用者的設定與最近開啟清單；
   - 開啟 fixture，等到 `first_paint`，最好等到 `first_page_exact`，然後結束整個 process tree；
   - 只要 `--version`、`--help` 失敗，或沒有收到 `first_paint`，打包就會失敗。

自動 smoke test 之外，發佈前還要在**乾淨的機器或 VM／Windows Sandbox** 手動確認：
- [ ] zip 從瀏覽器下載後（帶 Mark-of-the-Web）解壓執行：記錄 SmartScreen 的提示行為（未簽章時會出現「Windows 已保護您的電腦」）。
- [ ] 解壓到路徑含空白與中文的目錄，例如 `C:\工具\FastPDF 測試\`，可以正常啟動並開檔。
- [ ] 開啟 small-text、large-page-count、traditional-chinese、encrypted、malformed 類 fixture，不能 crash。
- [ ] 列印對話框、深色與夜間模式、設定的保存與還原、關閉後沒有殘留 process。
- [ ] Explorer 顯示的 exe 圖示，以及「內容 → 詳細資料」中的產品名稱與版本。
- [ ] Windows Defender 掃描 zip 與 exe 沒有警告。

### 1.8 SHA-256 與發佈

- [ ] release notes 附上 zip 的 SHA-256（`.sha256` 檔的格式為 `<hex>  <檔名>`）。使用者可以用 `Get-FileHash -Algorithm SHA256 <zip>` 驗證。
- [ ] `git tag -a vX.Y.Z -m "FastPDF X.Y.Z"`，由 owner 決定何時 push。
- [ ] release notes 內容：變更摘要、KPI 實測值、已知問題（未簽章、檔案關聯只有命令列、沒有 UI 等）、SHA-256。
- [ ] **不提供** auto-update（spec §9）。新版本由使用者自行下載。

### 1.9 隱私：binary 內的 build 路徑

- 依賴 crate 的 panic 位置字串會把 build 機器的路徑嵌進 exe，例如 `C:\Users\<帳號>\.cargo\registry\src\...`，也就是**洩漏 build 帳號的使用者名稱**。
  - 2026-10-04 實測：沒有改寫路徑的 dist exe 含有 651 處 `C:\Users\`（516 處 `.cargo\registry`、135 處 `.cargo\git`）。
  - workspace 自己的 crate 用的是相對路徑，沒有外洩。
- `tools/package.ps1` **預設**以 `--config "target.x86_64-pc-windows-msvc.rustflags=[...]"` 加上 `--remap-path-prefix=<CARGO_HOME>=cargo-home` 與 `--remap-path-prefix=<repo>=fastpdf`：
  - 這組旗標會和 `.cargo/config.toml` 的 `windows_slim_errors`、`+crt-static` 串接在一起。不能改用 `RUSTFLAGS` 環境變數，因為它會**取代** config 中的旗標。
  - build 完成後會檢查 exe 中不再出現 `CARGO_HOME`、`USERPROFILE` 與 repo 路徑，找到就讓打包失敗。
  - 不需要這項處理時，可以用 `-NoRemapPaths` 關閉，`BUILDINFO.txt` 會註明。
- Cargo 1.99 的 `trim-paths` profile 選項**仍然不是 stable**（實測會出現 `feature trim-paths is required`）。等它 stable 之後，可以改成在 `[profile.dist]` 設定。

---

## 2. 檔案關聯（`crates/fastpdf-shell`）

- **作法**：`Registration::new(exe)` 產生純資料的 `Plan`，`apply(&plan)` 才實際寫入 registry（只在 Windows 上編譯）。單元測試只檢查計畫內容，**從不執行 apply**。
- **CLI**（`crates/fastpdf-app/src/file_types.rs`）：
  - `fastpdf --register-file-types`、`fastpdf --unregister-file-types`：對目前執行的 exe（`std::env::current_exe()`）套用計畫，不開視窗。成功時印出變更數量與「預設應用程式」設定頁的位置，失敗時 exit code 為 1；
  - 加上 `--dry-run` 只印出 `Plan::to_reg_file()` 的 `.reg` 內容供檢視，不寫入 registry。輸出是 UTF-8；含非 ASCII 字元的 `.reg` 要給 regedit 匯入時，必須另存成 UTF-16 LE（含 BOM）；
  - 兩者不能和檔案或 `--engine` 一起使用。release build 是 GUI subsystem，在 PowerShell 中要等它結束並看到輸出，請用 `fastpdf --register-file-types | Out-Host`；
  - 開發機上只執行過 `--dry-run`，實際寫入請依下方〈驗證方式〉在 Sandbox 或 VM 中確認。
- **範圍**：全部寫在 `HKEY_CURRENT_USER`，不需要系統管理員權限，也不影響其他使用者。

| Key（在 `HKCU` 之下） | 作用 |
|---|---|
| `Software\Classes\FastPDF.Document` | ProgID：類型名稱、`DefaultIcon`、`shell\open\command`、`Application` |
| `Software\Classes\.pdf\OpenWithProgids` 的值 `FastPDF.Document`（REG_NONE） | 出現在 `.pdf` 的「開啟檔案」清單，**不搶預設** |
| `Software\Classes\Applications\fastpdf.exe` | `FriendlyAppName`、`SupportedTypes\.pdf`、`DefaultIcon`、`shell\open\command` |
| `Software\FastPDF\Capabilities` | `ApplicationName`、`ApplicationDescription`、`ApplicationIcon`、`FileAssociations\.pdf` |
| `Software\RegisteredApplications` 的值 `FastPDF` | 讓 FastPDF 出現在「設定 → 應用程式 → 預設應用程式」 |

- **命令列格式**：`"C:\路徑 含空白\fastpdf.exe" "%1"`，程式路徑與 `%1` 都加上引號。
- **路徑限制**：
  - 含 `%` 的路徑會被拒絕，因為 shell 的命令範本會展開它；
  - 含 `"` 或控制字元的路徑也會被拒絕；
  - `\\?\` 前綴與正斜線會先正規化。
- **預設程式**：Windows 10／11 不允許程式自行設定預設程式（`UserChoice` 有雜湊保護），計畫也**從不寫入它**。改由 app 開啟 `ms-settings:defaultapps?registeredAppUser=FastPDF`（Windows 11 會直接進入 FastPDF 的頁面；較舊的版本可用 `ms-settings:defaultapps`），讓使用者自己選擇。
- **反註冊**：反向刪除上述內容。共用的 key（`.pdf`、`OpenWithProgids`、`RegisteredApplications`）只刪除 FastPDF 的值。`Software\FastPDF` 只在已經空了的時候才刪除。
- **可攜版的注意事項**：註冊時寫入的是 exe 的絕對路徑。使用者搬移或刪除資料夾前，應該先反註冊，或在新位置重新註冊。這點要寫進 README 與 UI 說明。
- **驗證方式**：
  - 先在 Windows Sandbox 或拋棄式 VM 執行註冊，確認「開啟檔案」與「預設應用程式」中都看得到 FastPDF，再執行反註冊，確認相關 key 已清乾淨；
  - **不要在開發機上直接測試**；
  - 如果改用 MSIX 發佈（§4），檔案關聯改由 manifest 宣告，不需要這個 crate。

---

## 3. 程式碼簽章（分析，尚未建立任何憑證）

> 費用、資格與政策會變動。以下是撰寫時的一般認知，**決策前要以 Microsoft 與 CA 當時的官方文件重新確認**。

| 選項 | 成本與需求 | SmartScreen 與信任 | 備註 |
|---|---|---|---|
| A. 不簽章（現狀） | 0 | 下載的 zip 帶有 Mark-of-the-Web，執行時會出現「未知的發行者」警告，信譽累積慢；部分企業政策會直接封鎖 | 公開 beta 可以接受，但要提供 SHA-256 並說明這個警告 |
| B. OV code signing 憑證 | 每年數百美元；依 2023 年起的 CA/B Forum 規定，私鑰必須存放在硬體（token 或 HSM，或 CA 的雲端簽章服務） | 有具名的發行者，但信譽仍然要逐步累積 | 需要法律主體驗證；CI 要接 HSM 或雲端簽章 |
| C. EV 憑證 | 比 OV 貴，同樣需要硬體金鑰 | 過去可以立即取得 SmartScreen 信譽，**Microsoft 已調整這項待遇**，需要重新確認 | 主要差異在身分驗證更嚴格 |
| D. Microsoft 的雲端簽章服務（Trusted Signing，或其後續名稱） | 月費制，價格較低；憑證有效期很短，由 Microsoft 管理 | 信譽累積在驗證過的身分上 | **開放的國家與組織資格有限制，需要確認台灣法人是否適用**；SignTool 加上 dlib 即可接 CI |
| E. Microsoft Store（MSIX） | 開發者帳號；由 Store 簽章 | Store 安裝的 app 沒有 SmartScreen 問題 | 必須走 MSIX 與審核流程，見 §4 |

**簽章的技術要點**（任何選項都適用）：
- 對 `fastpdf.exe` 和安裝程式（MSI 或 MSIX）做 Authenticode SHA-256 簽章，**加上 RFC 3161 timestamp**，憑證到期後簽章仍然有效；
- 用 `signtool verify /pa /v` 驗證；
- 私鑰不能出現在 repo 或一般的 CI secret 中。

**建議**：
- V0.1 公開 beta 使用 A：未簽章的 zip，並公告 SHA-256；
- V1.0 前由 owner 先決定發行主體（公司或個人），再依資格選 D（若適用）或 B，並與 §4 的安裝程式一起導入；
- 要做決策時寫一份 ADR。

---

## 4. 安裝程式選項分析（尚未建立）

| 形態 | 優點 | 缺點／風險 | 適合 |
|---|---|---|---|
| **可攜版 zip**（現狀） | 不需要安裝、不需要 admin、預設不寫 registry；檔案關聯是可選的 per-user 註冊（§2） | 沒有開始功能表項目與解除安裝項目；更新要手動 | V0.1 beta、進階使用者 |
| **MSIX** | 安裝與移除都很乾淨；per-user；不需要 admin；檔案關聯在 `AppxManifest` 宣告（`uap:FileTypeAssociation`），會自動出現在預設應用程式中；可以上 Store | **必須簽章**（或走 Store）；packaged app 對 `%APPDATA%` 的寫入會被重新導向到套件的私有位置（FastPDF 的 settings 與 recent 要驗證）；package identity 對啟動時間的影響要用 B-8 量測；`.appinstaller` 的自動更新屬於 spec §9 禁止的 auto-update，不要使用 | V1.0 一般消費者 |
| **MSI（WiX Toolset）** | 企業部署（Intune、GPO、SCCM）的標準；可以安裝到 Program Files（per-machine）；解除安裝行為一致 | WiX 的學習成本、per-user 與 per-machine 的設計複雜度、版本與 UpgradeCode 的紀律；**WiX 新版本對有營收的組織可能有維護費（Open Source Maintenance Fee）要求，需要確認**；同樣需要簽章 | 有企業客戶需求時 |
| Inno Setup／NSIS（EXE 安裝程式） | 簡單、免費、per-user 容易 | 企業環境比較不歡迎 EXE 安裝程式；NSIS 偶爾會被防毒軟體誤判 | 不建議作為主要形態 |
| winget | 只是發佈管道（manifest 指向 zip、MSI 或 MSIX 的 URL 加上 SHA-256），支援 portable 類型 | 需要公開的下載 URL，並要對 `winget-pkgs` 開 PR（屬於對外動作，由 owner 決定） | V0.1 之後的低成本管道 |

本機的 Windows SDK 已經包含 `makeappx.exe` 與 `signtool.exe`，但本次**沒有建立任何套件或憑證**。

**建議**：
1. V0.1：可攜版 zip 加 SHA-256，檔案關聯由 app 提供 opt-in 的 per-user 註冊。
2. V1.0：MSIX（簽章後 sideload 或上 Store）作為主要安裝形態；MSI 只在企業需求明確時才做。
3. 導入前寫 ADR（發佈形態、簽章、更新政策），並用 B-8 比較「zip 版 vs MSIX 版」的 cold 與 warm 啟動時間。

---

## 5. 本次（0.0.1）實測紀錄（2026-10-04，不是正式發佈）

**來源**：HEAD `2f162e0` 的乾淨匯出（`git archive`），加上本次發佈相關的修改。當時 working tree 中有其他工作尚未 commit 的 `fastpdf-ui` 修改，無法 build，所以沒有在 working tree build（這正是 §1.2 要求從乾淨 checkout build 的原因）。

**環境**：
- Windows 11 Pro 10.0.26200；
- rustc 1.99.0、MSVC 14.44、Windows SDK 10.0.26100；
- `CARGO_TARGET_DIR=target\agent-pkg`。

**品質關卡**：
- 以下全部通過：
  - `cargo fmt --all -- --check`；
  - `cargo clippy --workspace --all-targets -- -D warnings`；
  - `cargo test --workspace`（其中 `fastpdf-shell` 11 個測試）；
  - `check_engine_isolation.py`；
  - `license_report.py --all-features --check`：0 個待審，產生的 `THIRD_PARTY_LICENSES.md` 與 HEAD **完全相同**，表示沒有新增第三方 crate。

**`package.ps1` 的結果**：

| 項目 | 結果 |
|---|---|
| dist build | 3 分 20 秒。路徑改寫讓 rustflags 改變，依賴全部重新編譯；依賴已編譯過時約 1.5 分鐘 |
| `fastpdf.exe` | 16,291,840 bytes（15.54 MB），符合 spec §29「< 30 MB」 |
| zip | 7,065,729 bytes（6.74 MB），5 個檔案 |
| PE resource | `RT_GROUP_ICON #1`、`RT_ICON` ×6（16／24／32／48／64／256）、`RT_VERSION #1`、**`RT_MANIFEST` 只有 #1**（GPUI 的那一份，沒有重複） |
| VERSIONINFO | ProductName／FileDescription 為 `FastPDF`；ProductVersion 為 `0.0.1`；FILEVERSION 為 `0.0.1.0`；沒有 CompanyName 與 LegalCopyright；OriginalFilename 為 `fastpdf.exe` |
| build 路徑 | 未改寫時有 651 處 `C:\Users\`；改寫後為 **0**（變成 651 處 `cargo-home\`），repo 與 scratch 路徑都是 0 |
| smoke：CLI | `--version` 輸出 `fastpdf 0.0.1`；`--help` 第一行是 `usage: fastpdf [--engine NAME] [file.pdf]` |
| smoke：GUI | 開 `three-pages-platypus-times.pdf`，從 process 建立起算：`window_visible` 240 ms、`first_paint` 241 ms、`first_page_exact` 266 ms。只量 1 次，warm cache，機器上同時有其他負載（未改寫路徑的那次為 214／242 ms）；結束後沒有殘留 process |
| SHA-256（示範用） | exe 為 `eef10b170fad9a6574b7ee20f49efa2b83675eeb84fb8aaee24c3068cf82b3fc` |

**注意**：
- zip 的 SHA-256 **每次打包都會不同**，因為 `BUILDINFO.txt` 的 build 時間和 zip entry 的時間戳都會變（同一個 exe 打包兩次，得到 `4674fc6d…` 與 `03cfed7d…`）。公告的雜湊必須對**實際發佈的那個 zip** 計算。
- 如果需要 byte-for-byte 可重現的 zip，要固定 entry 時間戳並移除 build 時間，列為待辦。
