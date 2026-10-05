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
python tools/license_report.py --all-features --check
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

- [ ] `python tools/license_report.py --all-features --check`：重新產生 `THIRD_PARTY_LICENSES.md`，不可出現待審項目。commit 的清單涵蓋所有 feature（含 `engine-zpdf`），不加 `--all-features` 會改掉清單的檔頭與內容。依賴有變動時，逐一審查新增的 crate。
- [ ] 檢查 Apache-2.0 依賴是否有 `NOTICE` 檔。有的話，內容必須隨 binary 一起發佈。`package.ps1` 的 bundle 步驟（下一項）會自動收錄，並列出有 NOTICE 的 crate。
- [ ] 檢查 `THIRD_PARTY_LICENSES.md` 的 Ported source 章節，確認所有移植自 upstream 的程式碼都有登記。
- [ ] **各 crate 的授權全文（`licenses/third-party/`）**：
  - MIT／BSD／ISC／Zlib 要求隨 binary 附上**每個 crate 自己的 copyright 與授權全文**。`package.ps1` 在 staging 時執行 `python tools/license_report.py --bundle <staging>/licenses/third-party` 收錄這些檔案。
  - 範圍：從 `fastpdf-app` 經 normal 依賴可達的 crate（含 proc-macro），涵蓋 release binary 實際連結的所有 crate。build 依賴只在編譯時執行，不會連結進 exe，所以不收錄。`THIRD_PARTY_LICENSES.md` 的清單範圍比較大：包含所有 workspace member 與 build 依賴。
  - 收錄方式：
    - 各 crate 原始碼目錄的 `LICENSE*`、`LICENCE*`、`COPYING*`、`NOTICE*`、`COPYRIGHT*`、`UNLICENSE*`，原樣複製到 `<crate>-<version>/`；
    - git 依賴另外收錄 repository 根目錄的 `NOTICE*`，放在 `<crate>-<version>/repository-root/`；
    - git 依賴在 Windows 上 checkout 時，symlink 會變成只含相對路徑的文字檔（stub），例如 `../../LICENSE-APACHE`。bundle 會沿路徑複製真正的檔案。找不到檔案、或路徑超出該 crate 的原始碼範圍時，不複製，列在 `MISSING.md`；
    - 沒有附授權檔的 crate，先使用 `licenses/overrides/` 的檔案（見下一項）。仍然沒有的不會補寫 copyright 行，列在 `MISSING.md`。授權可選 Apache-2.0 的，由共用的 `Apache-2.0.txt` 涵蓋，在 `MISSING.md` 另外註記。
  - override（`licenses/overrides/<crate>-<version>/`）：
    - 只放在本機找到、來源明確的授權全文：同一個 crate 的其他版本，或同一個 repository 的其他套件，逐位元組複製。不可自行撰寫或拼湊 copyright 行；
    - 每個檔案都要在 `licenses/overrides/SOURCES.md` 登記來源與 SHA-256。沒有登記或 SHA-256 不符的資料夾，整個不會被使用；
    - 資料夾的版本必須和 `Cargo.lock` 完全相同。依賴升級後，舊版本的 override 會列為 stale 並顯示警告，該 crate 回到缺漏清單；
    - `MISSING.md` 另外列出「Filled in from licenses/overrides」與「Overrides not used」兩節。補上缺漏的步驟見 `licenses/overrides/README.md`。
  - 2026-10-04 實測（HEAD `eae58c0`，加入 override 之前，default features，`x86_64-pc-windows-msvc`）：
    - 範圍內有 362 個 crate，輸出 618 個檔案，未壓縮約 3.1 MB。其中 344 個 crate 有自己的授權全文；
    - 範圍是用 `cargo metadata` 的 resolve 計算，會包含 weak 依賴（`dep?/feature`），所以比實際連結的多。對照 `cargo tree -p fastpdf-app -e normal`：實際的 315 個 crate 全部在範圍內，另外多收 47 個（例如 `image` → `ravif` → `rav1e` 這條 AVIF 依賴）；
    - 33 個檔案是從 stub 解析而來：zed 的 17 個 crate 各 1 個（`LICENSE-APACHE`），hayro 的 8 個 crate 各 2 個（`LICENSE-APACHE`、`LICENSE-MIT`）。抽查 zed 7 個、hayro 3 個 crate（含 `gpui`、`hayro-syntax`），內容與 repository 中的原檔逐位元組相同，是完整的授權全文；
    - 有 NOTICE 的 crate 共 8 個：`hayro`、`hayro-ccitt`、`hayro-cmap`、`hayro-interpret`、`hayro-jbig2`、`hayro-jpeg2000`、`hayro-postscript`、`hayro-syntax`。內容都是 hayro repository 根目錄的 `NOTICE.md`，記載改寫自 PDFBox、pdf.js 與 png crate 的程式碼。其他 crate 都沒有 NOTICE 檔；
    - 13 個 crate 沒有附授權檔，但授權可選 Apache-2.0，由共用的 `Apache-2.0.txt` 涵蓋：accesskit 系列 3 個、lyon 系列 5 個、profiling 系列 2 個、`sval_nested`、`svg_fmt`、`zune-inflate`。其中 `sval_nested` 的 2 個 stub 指向套件以外，無法解析；
    - `package.ps1` 完整執行一次（HEAD 的乾淨匯出加上本次工具修改）：zip 共 623 個 entry（`licenses/third-party/` 佔 618 個），entry 清單檢查與 smoke test 都通過；
    - zip 為 8,415,819 bytes（8.03 MB）。同一批檔案不含 `licenses/third-party/` 時為 7,065,748 bytes，增加 1,350,071 bytes（+19.1%）。
  - 加入 override 之後的實測（HEAD `5b9f222`，條件相同，只執行 `--bundle`，沒有重新打包）：
    - 輸出 620 個檔案（多了 2 個 override 檔）。344 個 crate 有自己的授權全文，2 個由 override 補上，13 個由共用的 `Apache-2.0.txt` 涵蓋，3 個仍然缺漏；
    - `alloc-stdlib` 0.2.4（BSD-3-Clause）：使用同一個 repository 的 `alloc-no-stdlib` 2.0.4 套件內的 `LICENSE`（Copyright (c) 2016 Dropbox, Inc.）；
    - `pulp-wasm-simd-flag` 0.1.1（MIT）：使用同一個 repository、同一個 commit 的 `pulp` 0.22.3 套件內的 `LICENSE`（Copyright (c) 2021 sarah）；
    - 兩者的來源判斷（`repository` 欄位、`.cargo_vcs_info.json` 的 commit 與子目錄）和 SHA-256 記錄在 `licenses/overrides/SOURCES.md`。
  - **2026-10-05：缺漏已經全部補上。**
    - `seahash` 4.1.0、`taffy` 0.13.0、`simd_helpers` 0.1.0 在本機找不到全文，經 owner 同意後，從各自的 upstream repository 下載，並固定在特定 commit：
      - `taffy`：發佈 commit 的 `LICENSE`；
      - `seahash`、`simd_helpers`：發佈後由 upstream 補上的 `LICENSE`。
    - 網址、commit 與 SHA-256 記錄在 `licenses/overrides/SOURCES.md`。
    - `seahash` 的 upstream 授權檔沒有 copyright 行，照原樣收錄，不自行補寫。
    - `--bundle` 結果：362 個 crate，344 個有自己的授權全文，5 個由 override 補上，13 個由共用的 `Apache-2.0.txt` 涵蓋，**0 個缺漏**。
    - 仍會出現 1 行警告：`sval_nested` 有 2 個 symlink stub 指向套件外，無法跟隨，但它的授權可選 Apache-2.0，已由共用全文涵蓋。
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
| `FastPDF-X.Y.Z-win-x64/fastpdf.exe` | `--profile dist`：fat LTO、`codegen-units=1`、strip symbols；link 時加上 `/Brepro`（見下方〈可重現性〉） |
| `README.md` | 專案 README |
| `THIRD_PARTY_LICENSES.md`、`licenses/Apache-2.0.txt` | 第三方 crate 清單與授權審查結果；Apache-2.0 全文 |
| `licenses/third-party/` | 連結進 exe 的每個 crate 自己的授權檔（`<crate>-<version>/`）、共用的 `Apache-2.0.txt`、缺漏清單 `MISSING.md`。由 `license_report.py --bundle` 產生，見 §1.5 |
| `BUILDINFO.txt` | 版本、完整的 git commit hash（含 dirty 標記）、source date、rustc 版本、profile、exe 的 SHA-256。**不記錄打包時間** |

`package.ps1` 會自動檢查：
- VERSIONINFO 與 Cargo 版本一致，且沒有 `CompanyName`；
- zip 的 entry 清單完全符合預期，沒有多也沒有少。`licenses/third-party/` 底下以 bundle 步驟回報的檔案清單為準，逐一比對；
- staging 不可有空資料夾，zip 也不可有任何目錄 entry：
  - `licenses/` 只複製最上層的檔案。`licenses/overrides/` 這類子資料夾是 bundle 步驟的輸入，內容已經收進 `licenses/third-party/`；
  - 之前用 `licenses\*` 複製時，會留下一個空的 `licenses/overrides/`，在 zip 中成為多餘的目錄 entry。舊的檢查會把目錄 entry 濾掉，所以沒有發現；
- zip 的 entry 依路徑排序，時間都是 source date，沒有檔案屬性（見〈可重現性〉）；
- zip 內 exe 的 SHA-256 等於 build 產物。

**可重現性（reproducible build）**：同一個 commit、同一套工具鏈，打包出來的 exe 與 zip 都逐位元相同，SHA-256 也相同。

- **source date**：有設定 `SOURCE_DATE_EPOCH`（reproducible-builds.org 的慣例）就用它，否則用 HEAD 的 commit 時間（`git log -1 --format=%ct`）。不是 git checkout、也沒有設定時，用 1980-01-01 並顯示警告。
- **zip**（`package.ps1` 自己寫 zip，不再用 `ZipFile.CreateFromDirectory`）：
  - 只有檔案，沒有目錄 entry，依路徑的 ordinal 順序排列，不受檔案系統列舉順序影響；
  - 每個 entry 的時間都是 source date。zip 的時間欄位（DOS 格式）不含時區，這裡存 UTC 的時鐘時間，精度 2 秒。解壓工具會把它當成本地時間，所以在 UTC+8 看到的檔案時間會比 commit 時間早 8 小時；
  - 檔案屬性（external attributes）一律為 0；
  - `BUILDINFO.txt` 記錄完整的 commit hash 與 source date，不記錄打包時間，也不記錄 source date 從哪裡來。所以把 `SOURCE_DATE_EPOCH` 設成 commit 時間，結果和不設定相同；
  - deflate 由 .NET 的 zlib 執行，所以壓縮後的位元組也取決於 PowerShell／.NET 的版本。`package.ps1` 會印出版本，本次為 PowerShell 7.6.6、.NET 10.0.12。
- **exe**：`package.ps1` build 時加上 `-Clink-arg=/Brepro`，和 `--remap-path-prefix` 放在同一個 `--config` 旗標中，**不需要修改 `.cargo/config.toml` 或 profile**。
  - 沒有 `/Brepro` 時，link.exe 會把 link 的時間寫進 PE header 與 debug directory，並為 PDB 產生隨機的 GUID，所以每次 build 的 exe 都不同。dist profile 雖然 strip symbols，link 仍然會產生 `fastpdf.pdb`（不放進 zip），exe 的 CodeView 紀錄含有它的檔名與 GUID；
  - `/Brepro` 讓這些欄位改由 image 內容的雜湊決定；
  - build 路徑已經由 `--remap-path-prefix` 改寫，`CARGO_TARGET_DIR` 的路徑也不會進入 exe，所以 checkout 的位置與 target dir 不影響結果。
- **前提**：
  - 同一個 commit（含 `Cargo.lock`），working tree 是否 dirty 也要相同（`BUILDINFO.txt` 有標記）；
  - 檔案的換行要和全新的 checkout 相同。`.gitattributes` 讓 git 把 CRLF 正規化，所以 working tree 中被改成 CRLF 的檔案，`git status` 看不出來，但放進 zip 的位元組不同。`package.ps1` 用 `git ls-files --eol` 找出這類檔案（`i/lf w/crlf`），顯示警告並在 `BUILDINFO.txt` 標記。最簡單的作法是在全新的 clone 打包；
  - 同一套工具鏈：`rust-toolchain.toml` 的 rustc、MSVC 的 link.exe、Windows SDK 的 `rc.exe`，以及同一個 PowerShell／.NET；
  - release 一律跑完整流程。`-SkipBuild` 的 `BUILDINFO.txt` 會註明沒有驗證 build 方式，所以 zip 會和完整流程的不同。
- **MSIX 不能逐位元重現**：
  - makeappx 把**打包當下的時間**寫進每個 entry 的時間欄位，而且沒有選項可以指定；
  - makeappx 依檔案時間排列 entry。staging 中 license 檔的時間是複製當下的時間，所以原本每次打包的 entry 順序、`AppxBlockMap.xml` 與 `[Content_Types].xml` 都不同。`package-msix.ps1` 現在先把 layout 中所有檔案的時間設成 source date，這三者就只取決於檔案本身，與複製的時間無關；
  - 結果：同樣的 staging 打包兩次，`.msix` 只有 entry 的時間欄位不同，把這些欄位清零後就逐位元相同。`package-msix.ps1` 會印出 `AppxBlockMap.xml` 的 SHA-256，可以用來比對兩個未簽章套件的內容；
  - owner 簽章時會加入簽章與 timestamp，所以簽章後的套件本來就不會逐位元相同。要驗證簽章後的套件，用 `makeappx unpack` 解開，再比對 payload 檔案，排除 `AppxBlockMap.xml`、`[Content_Types].xml`、`AppxSignature.p7x` 與 `AppxMetadata\`。

**MSIX（未簽章，ADR 0010）**：

```powershell
pwsh -File tools/package-msix.ps1       # 與 zip 共用 build 與 staging -> makeappx pack -> unpack round trip
```

- 輸出：`dist/FastPDF-X.Y.Z.0-x64.msix` 與 `.sha256`。工作目錄 `dist/.msix-work/` 在成功後會刪除（`-KeepWork` 可保留）。
- 共用部分：`package.ps1 -StageOnly -Flavor msix` 只做 build、檢查與 staging，回傳 staging 資料夾。所以 MSIX 和 zip 的 exe、授權檔、`BUILDINFO.txt` 完全相同，`BUILDINFO.txt` 只多標示 `msix`。
- layout 由三部分組成：
  - staging 的內容；
  - `packaging/msix/Assets/*.png`（由 `tools/icon/make_icon.py` 產生）；
  - 從 `packaging/msix/AppxManifest.xml` 填好的 manifest：四段版本、Publisher 佔位值，見 `packaging/msix/README.md`。
- `makeappx pack` 使用 Windows SDK 內建的工具，並做完整的語意驗證（不加 `/nv`）。打包前，layout 中所有檔案的時間都設成 source date。
- 腳本最後印出 `.msix` 與 `AppxBlockMap.xml` 的 SHA-256：前者每次打包都不同，後者對同樣的 staging 相同（見上方〈可重現性〉）。
- `makeappx unpack` round-trip 的檢查項目：
  - 每個檔案都逐位元相同；
  - 額外檔案只能是 `AppxBlockMap.xml`（`unpack` 不會寫出 `[Content_Types].xml`）；
  - 不能有 `AppxSignature.p7x`；
  - manifest 的 identity、full-trust 進入點、`.pdf` 關聯、`fastpdf.exe` 別名、唯一的 capability `runFullTrust`。
- 腳本**不簽章、不安裝、不建立憑證**。Cargo 的 pre-release 版本必須給 `-Revision N`。

### 1.7 Smoke test

`package.ps1` 會先把 zip 解壓到 `dist/.smoke-*`，再執行以下步驟。每一次啟動，包括只走 CLI 的情況，都會把 `FASTPDF_SETTINGS_FILE` 與 `FASTPDF_RECENT_FILE` 設為空值，所以不會建立 `%APPDATA%\FastPDF\`：

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

**MSIX 沒有自動 smoke test**：未簽章的套件無法安裝，而 smoke test 需要安裝。`package-msix.ps1` 只做靜態的 round-trip 驗證。owner 簽章之後，要在乾淨的 VM 依 §4 的〈簽章後的驗證〉逐項確認。

### 1.8 SHA-256 與發佈

- [ ] release notes 附上 zip 的 SHA-256（`.sha256` 檔的格式為 `<hex>  <檔名>`）。使用者可以用 `Get-FileHash -Algorithm SHA256 <zip>` 驗證。
- [ ] release notes 附上 `BUILDINFO.txt` 的內容，以及 MSVC、Windows SDK、PowerShell／.NET 的版本。zip 可重現（§1.6〈可重現性〉），所以任何人都可以重新產生同一個 zip 來驗證：
  1. 用全新的 `git clone` checkout release 的 tag（長期使用的 working tree 可能有 `git status` 看不出來的換行差異，見 §1.6）；
  2. 用同一套工具鏈執行 `pwsh -File tools/package.ps1`；
  3. 比對 zip 的 SHA-256。不同時，先比對 `BUILDINFO.txt` 中的 exe 雜湊，判斷差異在 exe（工具鏈）還是在打包（PowerShell／.NET），再逐檔比對解壓後的內容。
- [ ] 公告之前，自己先在另一個 checkout（最好是另一台機器）重做一次，確認 SHA-256 相同。
- [ ] MSIX 無法逐位元重現（§1.6）：公告的 SHA-256 只用來確認下載完整；內容是否一致，要以解壓後的 payload 比對。
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
- **以 MSIX 安裝時**（ADR 0010）：
  - 檔案關聯改由套件的 manifest 宣告（`uap:FileTypeAssociation`），安裝與解除安裝時由 Windows 處理；
  - `file_types.rs` 以 `GetCurrentPackageFullName` 偵測 package identity。有 identity 時，`--register-file-types`、`--unregister-file-types`（含 `--dry-run`）只印出「關聯由套件的 manifest 管理」以及「預設應用程式」設定頁的位置，然後 exit 0。**不建立計畫，也不碰 registry**；
  - 單元測試確認：開發機上的測試 process 沒有 identity；有 identity 時兩個指令都回傳 0，而且在建立計畫之前就結束；
  - 在實際安裝的 MSIX 中的輸出，要等 owner 簽章後才能驗證（§4）。
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
  - MSIX 版不使用這個 crate，見上方〈以 MSIX 安裝時〉。zip 版仍然需要它。

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

## 4. 安裝程式選項（決策見 ADR 0010）

| 形態 | 優點 | 缺點／風險 | 適合 |
|---|---|---|---|
| **可攜版 zip**（現狀） | 不需要安裝、不需要 admin、預設不寫 registry；檔案關聯是可選的 per-user 註冊（§2） | 沒有開始功能表項目與解除安裝項目；更新要手動 | V0.1 beta、進階使用者 |
| **MSIX**（未簽章版已可產生，見下方） | 安裝與移除都很乾淨；per-user；不需要 admin；檔案關聯在 `AppxManifest` 宣告（`uap:FileTypeAssociation`），會自動出現在預設應用程式中；可以上 Store | **必須簽章**（或走 Store）；packaged app 對 `%APPDATA%` 的寫入會被重新導向到套件的私有位置（FastPDF 的 settings 與 recent 要驗證）；package identity 對啟動時間的影響要用 B-8 量測；`.appinstaller` 的自動更新屬於 spec §9 禁止的 auto-update，不要使用 | V1.0 一般消費者 |
| **MSI（WiX Toolset）** | 企業部署（Intune、GPO、SCCM）的標準；可以安裝到 Program Files（per-machine）；解除安裝行為一致 | WiX 的學習成本、per-user 與 per-machine 的設計複雜度、版本與 UpgradeCode 的紀律；**WiX 新版本對有營收的組織可能有維護費（Open Source Maintenance Fee）要求，需要確認**；同樣需要簽章 | 有企業客戶需求時 |
| Inno Setup／NSIS（EXE 安裝程式） | 簡單、免費、per-user 容易 | 企業環境比較不歡迎 EXE 安裝程式；NSIS 偶爾會被防毒軟體誤判 | 不建議作為主要形態 |
| winget | 只是發佈管道（manifest 指向 zip、MSI 或 MSIX 的 URL 加上 SHA-256），支援 portable 類型 | 需要公開的下載 URL，並要對 `winget-pkgs` 開 PR（屬於對外動作，由 owner 決定） | V0.1 之後的低成本管道 |

**決策（ADR 0010）**：
1. V0.1：可攜版 zip 加 SHA-256，檔案關聯由 app 提供 opt-in 的 per-user 註冊。
2. V1.0：MSIX 作為主要安裝形態，由 owner 簽章；是否上 Store 由 owner 決定。zip 版繼續提供。
3. MSI 只在企業需求明確時才做，另寫 ADR。
4. 不做 auto-update，也不使用 `.appinstaller`。
5. winget 等 owner 決定。

**MSIX 的實作現況**（未簽章；`tools/package-msix.ps1`，詳見 §1.6）：

- manifest 樣板 `packaging/msix/AppxManifest.xml`：
  - `Windows.FullTrustApplication` 加上 `rescap:runFullTrust`；
  - `.pdf` 的 `uap:FileTypeAssociation`；
  - `uap5:AppExecutionAlias` 提供 `fastpdf.exe`；
  - `TargetDeviceFamily` 為 Windows.Desktop 10.0.19041.0 以上；
  - 不宣告網路 capability。
- 版本：Cargo 的 `X.Y.Z` 對應 MSIX 的 `X.Y.Z.0`。Publisher 是明顯的佔位值，簽章時必須改成憑證 subject。
- 2026-10-04 實測（HEAD `5b9f222` 的乾淨匯出加上本次修改；SDK 10.0.26100.0 的 `makeappx`）：

| 項目 | 結果 |
|---|---|
| `makeappx pack` | `Package creation succeeded.`，627 個檔案（626 個 payload 加上 `AppxManifest.xml`），完整語意驗證，沒有警告 |
| `makeappx unpack` round-trip | 627 個檔案逐位元相同；`AppxBlockMap.xml` 列出 627 個檔案；沒有 `AppxSignature.p7x`；`Get-AuthenticodeSignature` 為 `NotSigned`；manifest 的 identity、進入點、`.pdf`、別名、capability 檢查全部通過 |
| 套件大小 | `FastPDF-0.0.1.0-x64.msix` 8,580,747 bytes（8.18 MB）。同一個 exe（16,732,672 bytes）的 zip 為 8.2 MB |
| 時間 | 從 staging 到 round-trip 完成約 7 秒（不含 dist build） |
| 加入 `licenses/overrides/` 後（授權 agent 的檔案，當時尚未 commit） | zip 為 625 個 entry（`licenses/third-party/` 佔 620 個），沒有目錄 entry；MSIX 為 629 個檔案（628 個 payload 加上 manifest），round-trip 通過，8.19 MB |

**簽章後的驗證**（owner 執行，必須在**乾淨的 VM** 上，不要在開發機上做）：
1. 以 `-Publisher '<憑證 subject>'` 重新打包，`signtool sign /fd SHA256 /tr <timestamp URL> /td SHA256`，然後 `signtool verify /pa /v`。
2. 安裝與啟動：
   - 開始功能表與工作列的圖示；
   - 從 Explorer 開啟 `.pdf`，「開啟檔案」與「預設應用程式」中有 FastPDF；
   - 命令列可以使用 `fastpdf.exe` 別名；
   - `fastpdf --register-file-types` 印出「由套件的 manifest 管理」。
3. **`%APPDATA%` 重新導向**：
   - 確認 settings 與 recent 的實際位置：套件化的 full-trust app 寫入 `%APPDATA%\FastPDF\` 時，會被導到套件的私有位置；
   - 確認和 zip 版是否共用設定；
   - 確認解除安裝後是否殘留；
   - 確認 `FASTPDF_SETTINGS_FILE`、`FASTPDF_RECENT_FILE` 的行為。
4. 解除安裝：檔案關聯、開始功能表項目、套件資料都要移除乾淨。
5. **B-8**：經由 `fastpdf.exe` 別名啟動套件版，和 zip 版比較冷啟動與熱啟動、idle RAM（spec §29）。在能安裝之前**不能做**這項比較。
6. SmartScreen 與 Defender 的行為，以及高 DPI 下的 tile 與工作列圖示（目前只有 scale-100，沒有 `resources.pri`）。

---

## 5. 本次（0.0.1）實測紀錄（2026-10-04，不是正式發佈）

> **最新一次打包（2026-10-05，HEAD `658b47a`，可重現）**：exe 16,879,616 bytes（SHA-256 `72c5186f…`），zip 628 個 entry（其中 623 個在 `licenses/third-party/`，缺漏 0），含 GPUI 本地 patch（ADR 0011）。
>
> **再前一次（HEAD `8135107`）**：exe 16,875,008 bytes（SHA-256 `78bb4ba8…`），zip 625 個 entry。
>
> **前一次打包（2026-10-05，HEAD `1dda1b4`）**：
> - `package.ps1 -NoGuiSmoke`：exe 16,823,296 bytes，zip 8,634,700 bytes。
> - zip 共 625 個 entry，其中 620 個在 `licenses/third-party/`，沒有目錄 entry。
> - CLI smoke 通過。
> - B-8 的 GUI 量測見 `docs/benchmarks/b8-app.md`〈最終版（第七輪）〉。
>
> 以下是第一次打包的紀錄。

**來源**：HEAD `6189995` 的乾淨匯出（`git archive`），加上本次發佈相關的修改。當時 working tree 中有其他工作尚未 commit 的 `fastpdf-ui` 修改，無法 build，所以沒有在 working tree build（這正是 §1.2 要求從乾淨 checkout build 的原因）。

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

**注意**（第一次打包時的情況，2026-10-05 已修正）：
- 當時 zip 的 SHA-256 **每次打包都不同**：`BUILDINFO.txt` 有 build 時間，zip entry 的時間取自檔案時間（同一個 exe 打包兩次，得到 `4674fc6d…` 與 `03cfed7d…`）。exe 每次 build 也不同（link 時間與 PDB GUID）。
- 現在 exe 與 zip 都逐位元可重現，見 §1.6〈可重現性〉與下方實測。公告的雜湊仍然要對**實際發佈的那個 zip** 計算。

**可重現性實測**（2026-10-05）：
- 來源：HEAD `9cf3b8a` 用 `git clone` 建立的兩個 checkout，加上本次的 `package.ps1`／`package-msix.ps1` 修改；
- 環境：PowerShell 7.6.6、.NET 10.0.12，其餘同上；
- 每次都用 `-NoGuiSmoke`。

| 比對 | 修改前 | 修改後 |
|---|---|---|
| zip：同一個 exe 打包兩次（`-SkipBuild`） | `fd48aad7…` 與 `995476a2…`，不同 | 兩次都是 `790ed325…`（8,635,269 bytes） |
| zip：`SOURCE_DATE_EPOCH` | — | 設成 commit 時間，結果和不設定相同（`790ed325…`）。設成 `1700000000`，`BUILDINFO.txt` 與 entry 時間都變成 2023-11-14 22:13:20 UTC，zip 為 `07ec5093…` |
| exe：兩個 checkout、兩個 target dir（`agent-pkg`、`agent-pkg2`）各做一次 fat LTO build | `8dbbd148…`（`D:\fastPDF`、03:09 build）與 `e7467447…`（checkout B）。大小都是 16,823,296 bytes，只有 5 處、共 24 bytes 不同：PE header 的 TimeDateStamp、3 個 debug directory entry 的 TimeDateStamp、CodeView 的 PDB GUID | 兩次都是 `478a5c8e…`（16,823,296 bytes）。TimeDateStamp 變成內容雜湊 `0xcfe6b053`，debug directory 多一個 `REPRO` entry |
| zip：上面兩次完整流程（含 build） | — | 兩次都是 `e067e7f3…`（8,635,283 bytes）。最終版腳本再跑一次（build 已是最新）也一樣 |
| MSIX：同一個 exe 打包兩次 | `f445635a…`（8,617,380 bytes）與 `0a9d5768…`（8,617,400 bytes）。631 個 entry 的時間與順序、`AppxBlockMap.xml`、`[Content_Types].xml` 都不同 | `5efb6b13…` 與 `5771779c…`（都是 8,617,273 bytes），只有 entry 的時間欄位不同（1,262 bytes），清零後逐位元相同。`AppxBlockMap.xml` 都是 `15a77e0a…` |

- 修改前的 exe 比對已經涵蓋不同的 checkout 路徑與 target dir：兩者除了時間與 GUID 以外完全相同，表示 `--remap-path-prefix` 有效，`CARGO_TARGET_DIR` 的路徑也沒有進入 exe。
- build 時間：修改前的 checkout B 與修改後的 checkout A 同時以低優先權 build，各約 4 分 50 秒；修改後的 checkout B 單獨 build 為 3 分 40 秒。
- **長期使用的 working tree 打包結果不同**：從 `D:\fastPDF` 本身打包同一個 exe，zip 為 `562dbfcb…`，和 clone 的結果不同。
  - 原因：有 11 個 tracked 檔案在 working tree 是 CRLF，但 index 是 LF（`git ls-files --eol` 顯示 `i/lf w/crlf`），`git status` 看不出來；
  - 其中 `licenses/Apache-2.0.txt` 會放進 zip（`licenses/` 與 `licenses/third-party/` 各一份），所以 zip 不同；
  - `package.ps1` 現在會警告，並在 `BUILDINFO.txt` 標記。發佈一律用全新的 clone（§1.8）。
  - exe 不受影響：rustc 讀取原始碼時會把 CRLF 正規化。修改前的 exe 比對中，`D:\fastPDF` 同樣含有 CRLF 的 `.rs` 檔，結果也只差時間與 GUID。
