# ADR 0010 — 發佈形態：V0.1 可攜版 zip，V1.0 以 MSIX 為主

- 狀態：Accepted
  - V0.1 的部分已實作：`tools/package.ps1`、`fastpdf --register-file-types`。
  - V1.0 的 MSIX 已有不簽章的打包與 round-trip 驗證；**簽章、安裝驗證與上架決定**仍待 owner。
- 日期：2026-10-04
- 相關 spec：
  - §9：禁止 auto-update；
  - §29：exe 小於 30 MB；
  - §33：Windows 優先、檔案關聯、Explorer integration；
  - §36–§37：依賴與授權；
  - §52。
- 相關文件：
  - `docs/RELEASE.md`：§3 簽章選項、§4 安裝程式選項，本 ADR 的分析依據；
  - `packaging/msix/README.md`；
  - `crates/fastpdf-shell`；
  - `crates/fastpdf-app/src/file_types.rs`。

## Context

FastPDF 要開始交到使用者手上。發佈形態決定了五件事：

1. **怎麼安裝與移除**：要不要 admin、會不會留下殘留。
2. **怎麼建立檔案關聯**：Windows 10／11 不允許程式自己設定預設程式，`UserChoice` 有雜湊保護。
3. **怎麼取得信任**：SmartScreen、Authenticode 簽章。簽章需要決定法律主體，這是 owner 的決定。
4. **授權合規**：各 crate 的授權全文要隨 binary 發佈（`docs/RELEASE.md` §1.5）。
5. **怎麼更新**：spec §9 禁止 auto-update framework。

選項分析見 `docs/RELEASE.md` §4。本 ADR 的依據除了那份分析，還有這兩輪的實作結果：

- **可攜版 zip 已可重複產生，而且逐位元可重現**：`tools/package.ps1`。
  - 流程：dist build（fat LTO、去除符號、改寫 build 路徑）→ 檢查 VERSIONINFO 與 PE resource → 附上各 crate 授權全文 → zip 與 SHA-256 → 驗證 zip 內容 → 解壓後做 CLI 與 GUI smoke test。
  - exe 約 16 MB（HEAD `5b9f222` 為 16,732,672 bytes），zip 約 8.2 MB（含授權全文）。
  - 同一個 commit、同一套工具鏈，打包出來的 exe 與 zip 逐位元相同：zip entry 的時間使用 commit 時間，link 時加上 `/Brepro`（`docs/RELEASE.md` §1.6〈可重現性〉）。
- **per-user 檔案關聯已可用**：`crates/fastpdf-shell` 加上 `fastpdf --register-file-types`／`--unregister-file-types [--dry-run]`。
  - 全部寫在 `HKCU`，不需要 admin，不寫 `UserChoice`。
- **MSIX（未簽章）**：
  - `packaging/msix/AppxManifest.xml`：full trust、`.pdf` 的 `uap:FileTypeAssociation`、`fastpdf.exe` 執行別名；
  - `tools/package-msix.ps1`：與 zip 共用同一個 build 與 staging，用 Windows SDK 的 `makeappx` 打包，並以 `makeappx unpack` 做 round-trip 驗證；
  - 結果見〈Validation〉。

## Decision

1. **V0.1：可攜版 zip 加上 SHA-256**。
   - 由 `tools/package.ps1` 產生，不簽章。release notes 公告 SHA-256，並說明 SmartScreen 警告的原因。
   - zip 可逐位元重現。release notes 附上 `BUILDINFO.txt` 與工具鏈版本，任何人都可以用同一個 tag 重新產生 zip 並比對 SHA-256，未簽章的 zip 也因此可以由第三方驗證。
   - 檔案關聯為 opt-in：使用者執行 `fastpdf --register-file-types`（per-user）。
2. **V1.0：以 MSIX 為主要形態，由 owner 簽章**。
   - 憑證選項依 owner 的資格決定（`docs/RELEASE.md` §3）：Microsoft 的雲端簽章服務（若資格適用），或 OV／EV 憑證。是否上 Microsoft Store 也由 owner 決定。
   - 檔案關聯改由 manifest 宣告。以套件執行時，`--register-file-types`／`--unregister-file-types` 會偵測 package identity（`GetCurrentPackageFullName`），只印出說明並 exit 0，**不碰 registry**。
   - zip 版在 V1.0 後繼續提供，給不想安裝的使用者與企業的手動部署。
   - MSIX 無法逐位元重現：makeappx 把打包時間寫進每個 entry，簽章本身也含時間。內容的一致性以解壓後的 payload 比對；未簽章的套件也可以比對 `AppxBlockMap.xml`。
3. **MSI 只在企業需求明確時才做**，例如要用 Intune、GPO 部署，或需要 per-machine 安裝。屆時另寫 ADR，並先確認 WiX Toolset 授權與維護費的條件。
4. **不做 auto-update**（spec §9）：
   - 不使用 `.appinstaller` 的自動更新；
   - app 不檢查新版本，不連網；
   - 新版本由使用者自行下載，或經由 Store 等 owner 選定的管道取得。
5. **winget 等 owner 決定**。上架 winget 需要公開的下載 URL，並要對 `microsoft/winget-pkgs` 開 PR，屬於對外動作。技術上 zip（portable）與 MSIX 都可以登記。
6. **版本對應**：
   - Cargo 的 `X.Y.Z` 對應到 MSIX 的 `X.Y.Z.0`；
   - pre-release 版本必須明確給 revision（`package-msix.ps1 -Revision N`），因為 MSIX 沒有 pre-release 欄位，而且版本必須遞增；
   - exe 的 VERSIONINFO 維持 `X.Y.Z.0`。

## Consequences

- **兩條打包流程共用同一個 build 與檔案集合**（`package.ps1 -StageOnly`），zip 與 MSIX 不會各自漂移。manifest 樣板是新增的維護點：新增功能需要新的 capability 時（原則上不會），要同時修改樣板與 `package-msix.ps1` 的檢查。
- **MSIX 必須簽章才能安裝**。在 owner 簽章之前：
  - 無法驗證安裝與解除安裝、檔案關聯，以及 `%APPDATA%` 重新導向對 settings 與 recent 的影響。套件化的 full-trust app 寫入 `%APPDATA%\FastPDF\` 時，會被導到套件的私有位置。這會影響和 zip 版的設定共用，以及解除安裝後是否殘留；
  - 也無法做 B-8 的 zip 與 MSIX 啟動時間比較（spec §29），因為 package identity 與啟動路徑可能影響冷啟動。
- **Publisher 是硬性綁定**：manifest 的 `Publisher` 必須和簽章憑證的 subject 逐字相同。樣板目前用明顯的佔位值，打包時以 `-Publisher` 覆蓋。
- **發行主體是個人**（2026-10-06，ADR 0012）：簽章方案改以適合個人與開源專案的選項為主（`docs/RELEASE.md` §3），`Publisher` 等簽章方案確定後再填入。
- **zip 版的檔案關聯寫入 exe 的絕對路徑**：使用者搬移資料夾後，必須重新註冊。這點要寫進 README 與 UI 說明。
- **可重現性綁定工具鏈**：rustc（`rust-toolchain.toml`）、MSVC 的 link.exe、Windows SDK 的 `rc.exe`、PowerShell／.NET（zip 的 deflate）都會影響輸出的位元組。升級其中任何一項，同一個 commit 的 SHA-256 也會改變，這是預期的結果，所以 release notes 要記錄這些版本。
- **高 DPI 的 tile 與工作列圖示**：MSIX 目前只有 scale-100 的 logo，沒有 `resources.pri`。若要更清晰，需要加上 `targetsize-*`、`scale-*` 版本，並用 `makepri` 產生 PRI（SDK 內建，不需要新工具）。

## Alternatives considered

- **只發 zip，不做安裝程式**：最簡單，但沒有開始功能表、沒有一致的解除安裝，SmartScreen 信譽也只能靠下載量累積。V0.1 可以接受，V1.0 不夠。
- **MSI 優先（WiX）**：企業部署的標準，但 per-user 與 per-machine 的設計、UpgradeCode 與版本紀律都比 MSIX 複雜，同樣需要簽章，WiX 新版的授權條件也要確認。在沒有企業客戶之前不划算。
- **Inno Setup／NSIS 的 EXE 安裝程式**：免費、per-user 容易做，但企業環境比較不歡迎，NSIS 偶爾會被防毒軟體誤判，也一樣需要簽章，相較 zip 的改善有限。
- **只上 Microsoft Store**：可以免去自行管理憑證，但綁定 Store 的審核與帳號，使用者也不一定都有 Store。是否上架交給 owner 決定，不作為唯一管道。
- **MSIX 加 `.appinstaller` 自動更新**：違反 spec §9，不採用。

## Validation

**已驗證**（2026-10-04；HEAD `5b9f222` 的乾淨匯出加上本次修改）：

- `tools/package-msix.ps1`：
  - dist build 與 zip 共用 staging；
  - `makeappx pack` 在完整語意驗證下成功（沒有加 `/nv`）；
  - `makeappx unpack` round-trip：全部 payload 檔案逐位元相同，額外的檔案只有 `AppxBlockMap.xml` 與 `[Content_Types].xml`，**沒有 `AppxSignature.p7x`**（未簽章）；
  - unpack 後的 manifest：版本、Publisher、full-trust 進入點、`.pdf` 關聯、執行別名，以及唯一的 capability `runFullTrust`，都符合預期；
  - 實測數字見下方〈MSIX 實測〉。
- `fastpdf` 的 package identity 偵測：在開發機（未封裝）上的測試回報沒有 identity；以 package identity 執行時，`--register-file-types`／`--unregister-file-types`（含 `--dry-run`）一律 exit 0，而且不建立任何計畫。
- 整個 workspace 的 clippy（預設與 `--all-features`）、測試、`license_report.py --all-features --check` 全部通過，`THIRD_PARTY_LICENSES.md` 與 HEAD 無差異，沒有新增 crate。

**MSIX 實測**（SDK 10.0.26100.0 的 `makeappx`）：

| 項目 | 結果 |
|---|---|
| pack | `Package creation succeeded.`，627 個檔案（626 個 payload 加上 manifest），沒有警告 |
| round-trip | 627 個檔案逐位元相同，block map 列出 627 個，沒有簽章（`Get-AuthenticodeSignature`：`NotSigned`） |
| 大小 | `FastPDF-0.0.1.0-x64.msix` 8,580,747 bytes（8.18 MB）。exe 為 16,732,672 bytes（15.96 MB，符合 spec §29 的 < 30 MB）；同一個 exe 的 zip 為 8.2 MB |
| 時間 | staging 到 round-trip 約 7 秒（不含 dist build） |
| zip 流程回歸 | `package.ps1` 在 `-StageOnly` 重構後完整執行一次：zip 623 個 entry、CLI 與 GUI smoke 通過（`first_paint` 212 ms）。之後 `%APPDATA%\FastPDF` 不存在，沒有殘留 process |
| 加入 `licenses/overrides/` 後 | staging 只複製 `licenses/` 最上層的檔案，並拒絕空資料夾與 zip 目錄 entry。zip 為 625 個 entry（沒有目錄 entry），MSIX 為 629 個檔案，round-trip 通過 |

**可重現性實測**（2026-10-05；HEAD `9cf3b8a` 的兩個 `git clone`，分別使用 `target\agent-pkg` 與 `target\agent-pkg2`；細節見 `docs/RELEASE.md` §5）：

| 項目 | 修改前 | 修改後 |
|---|---|---|
| exe（兩次 fat LTO build） | 只差 24 bytes：link 時間（PE header 與 debug directory）與 PDB GUID | 逐位元相同（`478a5c8e…`） |
| zip（同一個 exe 打包兩次） | 不同 | 逐位元相同；兩次完整流程（含 build）也相同（`e067e7f3…`） |
| MSIX（同一個 exe 打包兩次） | entry 的時間與順序、`AppxBlockMap.xml`、`[Content_Types].xml` 都不同 | 只剩 makeappx 寫入的 entry 時間不同；`AppxBlockMap.xml` 相同 |

**尚未驗證**（owner 簽章之後，在乾淨的 VM 上做）：

1. 以正確的 Publisher 重新打包並簽章（加上 RFC 3161 timestamp），然後 `signtool verify /pa`。
2. 安裝：「開啟檔案」與「預設應用程式」出現 FastPDF；從 Explorer 開啟 `.pdf`；`fastpdf.exe` 別名可以從命令列使用。
3. settings 與 recent 的實際存放位置：確認是否被重新導向、解除安裝後是否殘留，以及 `FASTPDF_SETTINGS_FILE`／`FASTPDF_RECENT_FILE` 的行為。
4. 解除安裝後沒有殘留：檔案關聯、套件資料夾、registry。
5. B-8：經由別名啟動套件版，和 zip 版比較冷啟動與熱啟動、idle RAM（spec §29）。
6. 在 MSIX 中執行 `--register-file-types` 的輸出，確認走的是 package identity 分支。
