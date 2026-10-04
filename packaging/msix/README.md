# MSIX 打包素材（ADR 0010）

| 檔案 | 說明 |
|---|---|
| `AppxManifest.xml` | manifest 樣板。`tools/package-msix.ps1` 會填入 `{{VERSION}}`、`{{PUBLISHER}}`、`{{PUBLISHER_DISPLAY_NAME}}`、`{{ARCH}}` |
| `Assets/StoreLogo.png`（50×50）、`Assets/Square44x44Logo.png`（44×44）、`Assets/Square150x150Logo.png`（150×150） | 由 `uv run tools/icon/make_icon.py` 產生，與 exe 的 icon 是同一份原創圖形。不要手改，改圖請改腳本 |

```powershell
pwsh -File tools/package-msix.ps1          # dist build -> dist/FastPDF-X.Y.Z.0-x64.msix（未簽章）+ .sha256
```

## 內容

- **身分**：`Identity Name="FastPDF"`、`ProcessorArchitecture="x64"`。
- **版本**：取自 Cargo 的 `X.Y.Z`，轉成四段的 `X.Y.Z.R`，R 預設為 0，用 `-Revision` 指定。
  - Cargo 的 pre-release 版本（例如 `0.2.0-beta.1`）沒有對應的 MSIX 欄位，必須明確給 `-Revision`。
  - 版本必須遞增，更新才能安裝。
- **執行方式**：`Windows.FullTrustApplication` 加上 `rescap:runFullTrust`。這是一般 Win32 程式，沒有 AppContainer 沙箱。
- **檔案關聯**：`uap:FileTypeAssociation` 宣告 `.pdf`。安裝時 Windows 會把 FastPDF 加進「開啟檔案」與「預設應用程式」，解除安裝時一併移除。
  - 用套件執行時，`fastpdf --register-file-types` 與 `--unregister-file-types` 只會印出說明，不會碰 registry（`crates/fastpdf-app/src/file_types.rs`）。
- **命令列別名**：`uap5:AppExecutionAlias` 提供 `fastpdf.exe`。套件安裝的 app 不在 PATH 上，B-8 也要靠這個別名啟動套件版本。
- **刻意不宣告的項目**：網路 capability、`.appinstaller` 自動更新（spec §9）、開機啟動與背景啟用。

## 簽章前一定要換掉 Publisher

- 樣板預設的 `Publisher` 是 `CN=FASTPDF-UNSIGNED-PLACEHOLDER-REPLACE-WITH-CERT-SUBJECT`，只是佔位值。
- 簽章前，`-Publisher` 必須改成簽章憑證的 **subject，逐字相同**，例如 `CN=Example Ltd, O=Example Ltd, L=Taipei, C=TW`。不一致的套件無法簽章，也無法安裝。
- `-PublisherDisplayName` 是使用者看到的發行者名稱。

**由 owner 執行**（本 repo 的工具不會做）：
1. 取得憑證（選項見 `docs/RELEASE.md` §3）；
2. 以正確的 Publisher 重新打包；
3. 用 `signtool sign /fd SHA256 /tr <RFC 3161 timestamp URL> /td SHA256 ...` 簽章；
4. 在乾淨的 VM 安裝並驗證（`docs/RELEASE.md` §4）。

憑證與私鑰**絕不放進 repo**。

## 尚未驗證（要等能安裝之後）

- 安裝、解除安裝是否乾淨，檔案關聯是否出現在「開啟檔案」與「預設應用程式」。
- `%APPDATA%` 重新導向：套件化的 full-trust app 寫入 `%APPDATA%\FastPDF\` 的 settings 與 recent 時，會被導到套件的私有位置。是否被導向要以實際安裝結果為準。這會影響和 zip 版之間的設定共用、解除安裝後是否殘留，以及 `FASTPDF_SETTINGS_FILE` 的行為。
- B-8 的 zip 版與 MSIX 版啟動時間比較：要經由別名 `fastpdf.exe` 啟動，才會帶有 package identity。
- 高 DPI 的 logo：目前只有 scale-100 的基本圖，沒有 `resources.pri`。若要讓工作列與開始功能表在高 DPI 下更清晰，需要加上 `targetsize-*`、`scale-*` 版本，並用 `makepri` 產生 PRI。
