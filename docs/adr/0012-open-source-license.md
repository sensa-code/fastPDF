# ADR 0012 — 開源授權與個人發行

- 狀態：Accepted
- 日期：2026-10-06
- 決策者：owner（`sensa-code`）
- 相關：spec §37（License）、ADR 0010（發佈形態）、ADR 0011（vendored GPUI）、`docs/RELEASE.md` §3（簽章）

## Context

- spec §37 原本寫「長期希望保留商業化選項」，所以 FastPDF 一直沒有宣告授權：Cargo manifest 沒有 `license`，README 寫「保留所有權利」。
- 2026-10-06 owner 決定：
  - 不商業化，走開源模式；
  - 以個人身分發行；
  - 公開 repository 為 https://github.com/sensa-code/fastPDF 。
- 依賴的授權都是寬鬆授權（`THIRD_PARTY_LICENSES.md`）：MIT、Apache-2.0、BSD、ISC、Zlib 等。vendored 的 `gpui_windows` 是 Apache-2.0（ADR 0011）。

## Decision

1. **FastPDF 以 `MIT OR Apache-2.0` 雙授權**：使用者可以任選其一，這是 Rust 生態系的慣例。
   - repository 根目錄有 `LICENSE-MIT` 與 `LICENSE-APACHE`；
   - workspace 的 `license = "MIT OR Apache-2.0"`，每個 crate 都繼承這個設定；
   - 這兩個授權檔也會隨 zip 與 MSIX 一起發佈。
2. **貢獻採 inbound = outbound**：除非貢獻者另外聲明，提交的貢獻同樣以 `MIT OR Apache-2.0` 授權，寫在 README。
3. **著作權人**：`sensa-code and FastPDF contributors`，寫在 `LICENSE-MIT` 與 exe 的 VERSIONINFO（`LegalCopyright`）。
   - VERSIONINFO 不寫 `CompanyName`，因為是個人發行，沒有公司。
4. **個人發行對簽章的影響**（`docs/RELEASE.md` §3）：
   - V0.1 維持不簽章的 zip，並公告 SHA-256；
   - 正式簽章優先考慮開源專案適用的方案。條件會變動，申請前要以官方文件重新確認：
     - 開源專案的免費簽章服務，例如 SignPath Foundation；
     - 發給個人的開源 code signing 憑證，例如 Certum；
     - 以個人開發者帳號上架 Microsoft Store，由 Store 簽章 MSIX。
   - MSIX 的 `Publisher` 等簽章方案確定後，依憑證或 Store 指定的值填入（ADR 0010）。

## Consequences

- 任何人都可以使用、修改、再散布 FastPDF，包括商業用途；owner 自己不商業化，並不限制他人。
- 雙授權和所有依賴相容。Apache-2.0 選項附帶明確的專利授權。
- Apache-2.0 第 4 條要求保留修改聲明；`vendor/gpui_windows` 已經在每個修改過的檔案加上聲明（ADR 0011）。
- spec §37「保留商業化選項」被本 ADR 取代。spec §37 其餘的依賴授權審查原則不變：避免 GPL、AGPL 依賴，以及 `license_report.py --check`。

## Alternatives considered

- **只用 MIT**：最簡短，但沒有明文的專利授權。
- **只用 Apache-2.0**：有專利授權，但和只接受 MIT 的下游不相容。雙授權可以兩者兼顧。
- **GPL-3.0-or-later**：可以要求衍生作品開源，但 owner 的目標是開源分享，不是限制他人；Rust 生態系也以寬鬆授權為主。
- **非商業授權**（例如 PolyForm Noncommercial）：不符合 OSI 對開源的定義，和「走開源模式」不一致。
