# 數位簽章評估（ADR 0013 §5）

- 日期：2026-10-11
- 狀態：第一版已實作（`fastpdf-sign`、`fastpdf-engine-hayro` 的 `write::sign`、簽章面板），結果見文末
- 相關：ADR 0013（文件寫入功能）、spec §9（不使用網路）

## 目標

- 用使用者自己的憑證對 PDF 做數位簽章，結果另存新檔，原檔不變。
  - 自然人憑證（MOICA）、工商憑證（MOEACA）、組織及團體憑證（XCA），以及其他放在 Windows「個人」憑證存放區、有私鑰的憑證。
- 可以把印章圖片當成簽章的外觀：頁面上看到的是印章，點開是數位簽章（「蓋章並簽署」）。
- 不使用網路，不讀取也不保存私鑰或 PIN 碼。

## 標準與等級

- PDF：ISO 32000 §12.8 數位簽章。
- 格式：PAdES（ETSI EN 319 142-1），簽章字典使用 `/Filter /Adobe.PPKLite`、`/SubFilter /ETSI.CAdES.detached`。
- `/Contents`：DER 編碼的 CMS `SignedData`，detached，涵蓋 `/ByteRange` 指定的位元組（整個檔案扣掉 `/Contents` 的值）。
- 雜湊：SHA-256。
- signed attributes：
  - `content-type`（id-data）；
  - `message-digest`；
  - ESS `signing-certificate-v2`（RFC 5035），以 SHA-256 綁定簽署憑證，並附 issuer 與序號；
  - 不放 `signing-time`：PAdES 規定簽署時間寫在簽章字典的 `/M`。
- CMS 內附簽署憑證，以及從本機快取建出的中繼憑證（不含根憑證），讓驗證端能建立憑證鏈。
- 等級：**PAdES B-B**。沒有時戳（B-T），也沒有內嵌驗證資料（B-LT、B-LTA），原因見「限制」。

## Windows 端

### 列出憑證

- 開啟目前使用者的 `MY`（個人）存放區，列出同時符合以下條件的憑證：
  - 有對應的私鑰（`CERT_KEY_PROV_INFO_PROP_ID`）；
  - key usage 允許數位簽章或不可否認（沒有 key usage 擴充欄位時視為允許）；
  - 目前在有效期間內。
- 用 FastPDF 自己的面板顯示（主旨、發行者、有效期限），不使用 Windows 的憑證選擇對話框：查看憑證詳細資料時 Windows 可能會上網抓取憑證鏈與撤銷資訊。
- 以 SHA-1 thumbprint 識別選到的憑證，寫入寫入工作。

### 自然人憑證

- 插卡後，HiCOS 卡片管理工具與 Windows 的 Certificate Propagation 服務把卡上的憑證放進「個人」存放區，私鑰留在卡上。
- 簽署時，卡片的 CSP 或 minidriver 顯示 PIN 視窗。FastPDF 以 `CryptAcquireCertificatePrivateKey` 取得金鑰並傳入主視窗的 handle，PIN 視窗會出現在 FastPDF 前面。
- 卡片沒插、PIN 錯誤或使用者取消時，簽署失敗，不留下任何檔案。

### 簽署

- crypt32.dll 的 low-level message 函式：`CryptMsgOpenToEncode`（`CMSG_DETACHED_FLAG`）、`CryptMsgUpdate`、`CryptMsgGetParam`。可以完全控制 signed attributes，舊的 CSP 金鑰與 CNG（KSP）金鑰都能使用。
- 建立憑證鏈：`CertGetCertificateChain`，加上只用快取、不更新根憑證的旗標，不連網。
- 綁定：`windows-sys`（已是依賴），只需加開 `Win32_Security_Cryptography` feature，不增加 crate。
- 程式放在新的 `fastpdf-sign` crate（只在 Windows 編譯），由 `fastpdf-app` 使用：
  - UI 行程：列出憑證；
  - `--write-job` 子行程：簽署（PDF 解析仍然只在子行程內）。

## PDF 端：增量更新

- 原檔（或先套用文字修改、印章後重寫出的新內容）的位元組完全不動，後面附加：
  - 簽章欄位：widget annotation（`/FT /Sig`，`/V` 指向簽章字典）。
    - 不可見簽章：`/Rect [0 0 0 0]`，放在第 1 頁；
    - 可見簽章：外觀（`/AP /N`）是畫出印章圖片的 Form XObject，放在使用者放置印章的位置；
  - 簽章字典：`/Type /Sig`、`/Filter`、`/SubFilter`、`/M`（本機時間）、`/Reason`、`/Location`（選填）、`/ByteRange` 與 `/Contents`；
  - 新版本的 catalog（`/AcroForm` 加上欄位與 `/SigFlags 3`）與放置欄位的頁面（`/Annots` 加上 widget）；
  - 新的交互參照區段與 trailer：`/Prev` 指向原本的區段，`/Size`、`/Root`、`/Info`、`/ID` 延續原檔。原檔使用 xref stream 時，新區段也用 xref stream。
- `/Contents` 先保留固定大小的十六進位空間（預設 32 KB，足夠放入含數張憑證的 CMS），算出 `/ByteRange` 後計算 SHA-256、簽署，再把 CMS 填入；空間不夠時回報錯誤。
- 原有的簽章不受影響：增量更新不改動原本的位元組。
- 驗證：輸出檔以 hayro 重新開啟；確認 `/ByteRange` 從檔頭涵蓋到檔尾、只跳過 `/Contents`；CMS 由 Windows 驗證簽章，並比對 `message-digest`。

## 限制

- **不使用時戳伺服器（TSA）與 OCSP／CRL**：符合「不使用網路」的原則，但：
  - 簽署時間來自本機時鐘，驗證軟體會顯示為簽署者電腦上的時間，而不是可信的時間；
  - 憑證過期或被撤銷之後，無法證明簽署當時憑證仍然有效（沒有 LTV）。需要長期保存的文件，可以日後用支援時戳的工具加上文件時戳。
- **加密文件**：第一版不支援（與 ADR 0013 §4 相同）。
- **交互參照表損壞的檔案**：增量更新必須接在有效的區段之後。偵測到損壞時，先重寫成新檔再簽署；若原檔已有簽章，重寫會使它們失效，因此拒絕並說明原因。
- **簽署後不能再修改**：之後的任何修改都會讓簽章顯示為無效。介面會把簽署當成最後一步，未儲存的文字修改與印章在同一個工作裡先寫入、再簽署。
- **重寫已簽章的文件**：頁面工具、文字修改、印章會重寫檔案，原有的簽章一定失效。執行前要提醒使用者，並移除失效的簽章值，以免驗證軟體顯示「文件在簽署後被修改」。
- **法律效力**：依電子簽章法，使用合格憑證（例如自然人憑證）做的數位簽章具有法律效力；特定機關（法院、主管機關）是否接受特定格式，需由使用者自行確認。FastPDF 不提供法律保證。

## 測試方式

- 不在任何系統憑證存放區建立或匯入憑證，也不修改系統設定。
- PDF 端：以固定位元組的假簽署函式測試增量更新的結構、`/ByteRange`、可見外觀，以及原有簽章與原始位元組保留。
- CMS 端：測試時用 CNG 建立不保存的暫時金鑰（`NCryptCreatePersistedKey` 不給名稱），以 `CertCreateSelfSignCertificate` 在記憶體中產生自簽憑證，不寫入任何存放區；簽署後用 Windows 驗證簽章，並檢查 signed attributes。
- 獨立驗證：用 pyHanko 驗證輸出檔，確認簽章完整（INTACT）、涵蓋整份文件。自簽憑證的信任狀態會是「不受信任」，這是預期結果。
- 真實憑證：由 owner 用自己的自然人憑證在 GUI 實測，並用 Adobe Acrobat Reader 確認「簽章有效、文件未被修改」，簽署時間顯示為本機時間。
- GUI 測試：開發覆蓋層開啟時（`FASTPDF_DEV_OVERLAY=1`，與開發腳本相同的開關），開發腳本可以用 `sign-test` 改用暫時金鑰簽署，不需要讀卡機就能走完整個流程。此時面板只顯示測試簽署者，不讀取也不顯示「個人」存放區的憑證。

## 介面

- 「編輯」選單新增「數位簽章…」，開啟簽章面板：
  - 憑證清單（主旨、發行者、有效期限），可重新整理（插卡後）；
  - 簽章外觀：不顯示，或使用印章（選一個印章圖片，在頁面上點一下放置，可以拖曳與縮放，和蓋章相同）；
  - 簽署原因、地點（選填）；
  - 「簽署並另存…」：選擇檔名後開始，卡片的 PIN 視窗由 Windows 顯示；
  - 說明：原檔不會被修改；簽署後再修改，簽章就會失效；簽署時間是這台電腦的時間。

## 結論

可行，且不需要新的第三方 crate：Windows 已提供憑證存放區、智慧卡與 CMS 簽署所需的一切。第一版做 PAdES B-B、不可見與以印章為外觀的可見簽章、單一簽署者；時戳與 LTV 因為不使用網路的原則不做。

## 實作結果（2026-10-11）

- `fastpdf-sign`（新 crate，只用 `windows-sys`）：
  - 列出「個人」存放區中可簽署的憑證（唯讀開啟，有私鑰、在有效期間、key usage 允許簽章）；
  - 以 `CryptMsgOpenToEncode` 產生 detached CMS：SHA-256，signed attributes 為 content-type、message-digest、signing-certificate-v2，不含 signing-time；附簽署憑證與從快取建出的中繼憑證；
  - 簽完先用 `CryptVerifyDetachedMessageSignature` 驗證再交出；
  - 讀卡機與 PIN 的錯誤碼分成「找不到憑證」「沒有卡片」「已取消」「PIN 錯誤」。
- `fastpdf-engine-hayro` 的 `write::sign`：增量更新（簽章字典、欄位與 widget、可見外觀、新版 catalog／form 與頁面、table 或 stream 形式的 xref 區段），`/Contents` 預留 32 KB。xref 表的項目沒有指向對應物件時，先重寫再簽；若原檔已有簽章則拒絕。
- 重寫（頁面工具、文字修改、印章）會移除原有簽章的值（`/V`），欄位保留為未簽署，存檔通知會說明移除了幾個。
- 測試：
  - `crates/fastpdf-sign`：DER 編碼、錯誤碼分類，以及用暫時金鑰簽署後驗證（改一個位元組就驗證失敗）；
  - `crates/fastpdf-engine-hayro/tests/sign.rs`：原始位元組不變、`/ByteRange` 只跳過簽章值、連續簽兩次第一個簽章的位元組不變、可見簽章 render 出印章、xref stream 檔案、xref 表損壞的檔案、簽署失敗或簽章太大時不留檔、先重寫再簽、重寫已簽章文件會移除簽章值，以及用暫時金鑰產生真正的 CMS。
- 獨立驗證（pyHanko 0.37）：測試產生的檔案與 GUI 實測的檔案（文字修改＋印章外觀＋簽署）都判定為 intact、涵蓋整份檔案（ENTIRE_FILE）、沒有後續修改；以該測試憑證為信任根時判定有效。signed attributes 正好是 content-type、message-digest、signing-certificate-v2。
- 尚待 owner 以自己的自然人憑證實測（Adobe Acrobat Reader 驗證）。
