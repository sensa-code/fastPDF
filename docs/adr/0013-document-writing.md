# ADR 0013 — 文件寫入功能（另存新檔、頁面操作、印章、文字修改、數位簽章）

- 狀態：Accepted
- 日期：2026-10-11
- 決策者：owner（`sensa-code`）
- 相關：spec §9（V0.1 禁止功能）、§48、§53（產品哲學）；ADR 0002（engine 抽象）、ADR 0008（out-of-process rendering）；`CLAUDE.md` Non-negotiables

## Context

- spec §9 把 PDF editing、Annotations、Signature、PDF merge／split／convert 列為 V0.1 禁止功能；`CLAUDE.md` 的 Non-negotiables 也寫「No ... PDF editing features」。
- 2026-10-11 owner 回報實際使用上缺少的功能：
  - 閱讀：可拖曳的捲軸、單頁／捲動模式、雙頁並排（這三項不牴觸 spec，不在本 ADR 範圍）；
  - 編輯：另存副本、分割／擷取頁面、刪除／旋轉／重排頁面、合併多個 PDF、修改文字內容；
  - 簽章：蓋電子印章（印章圖片），以及用憑證（自然人憑證、工商憑證）做數位簽章。
- owner 選擇「隔離加入閱讀器」：寫入功能放進 FastPDF，但不能拖慢閱讀。另外兩個選項是做成獨立工具程式，或維持唯讀。
- spec §53 的原則仍然成立：可能讓開檔變慢的功能必須 lazy load、isolate、defer、disable by default，或不加入。
- 網路仍然禁止（spec §9）。

## Decision

### 1. 允許的寫入功能

| 功能 | 輸出方式 |
|---|---|
| 另存副本 | 原檔位元組照樣複製 |
| 擷取／分割頁面、刪除、旋轉、重排、合併 | 重寫成新檔（§3） |
| 印章圖片 | 重寫成新檔，印章畫進頁面內容（flatten） |
| 修改文字 | 重寫成新檔：移除原本的文字運算子，以內嵌的系統字型 subset 畫上新文字 |
| 數位簽章 | 在原檔之後附加 incremental update（§3），PAdES B-B |

修改文字的範圍有限制：只處理頁面本身的 content stream，不處理 Form XObject 與註解裡的文字；做不到時明確拒絕，不靠「蓋白色方塊」假裝修改。

### 2. 不覆寫原檔，也不拖慢閱讀

- **一律另存新檔**：輸出路徑由使用者在存檔對話框選擇，不能是目前開啟的檔案。
  - 先寫到目標資料夾的暫存檔；
  - 用 engine 重新開啟驗證；
  - 驗證通過才改名成目標檔名。
- **沒用到就沒有成本**：閱讀路徑（啟動、開檔、捲動、縮放、閒置）不增加任何工作、執行緒或記憶體。寫入程式碼只在使用者執行寫入指令時才跑。
- **在子行程執行**：
  - 寫入工作由 `fastpdf --write-job` 子行程執行，放在 job object 裡，有記憶體上限，主程式結束時子行程一起結束（同 ADR 0008 的 render host）；
  - hostile PDF 造成的 crash、配置炸彈或卡住，只會結束這個子行程；
  - 執行期間閱讀器的 UI 照常回應。
- **engine 隔離不變**：寫入程式碼放在 `fastpdf-engine-hayro`，使用 hayro-syntax 讀取物件；其他 crate 只透過 `fastpdf-engine-api` 的型別描述寫入工作（`tools/check_engine_isolation.py`）。

### 3. 兩種寫法

- **重寫**：從 trailer 的 `/Root`、`/Info` 走訪所有可到達的物件，重新編號後寫成全新的檔案，套用頁面樹的修改、新的頁面內容與新增的物件。
  - 只寫入可到達的物件，舊的 revision 不會留下。所以刪除的頁面與被改掉的文字，不會留在輸出檔裡；
  - 註解、表單、書籤、metadata 隨物件圖一起保留。指向已刪除頁面的書籤與連結，目的地會被移除。
- **Incremental update**：原檔位元組完全不動，在後面附加新物件與新的 xref（`/Prev` 指向原本的 xref）。
  - 只用在數位簽章，因為簽章必須涵蓋原本的位元組；
  - 之前的簽章仍然可以驗證。

### 4. 安全與權限

- 尊重文件的權限設定：文件禁止修改或組合頁面時，拒絕對應的寫入。
- 第一版不寫入加密文件：重寫需要解密後再加密，incremental update 需要用原本的金鑰加密新物件，兩者都還沒實作。介面會說明原因。
- 寫入程式碼同樣面對 hostile input：
  - 不 unwrap；
  - 有大小與深度上限；
  - 在子行程內執行。

### 5. 數位簽章不使用網路

- 簽章等級為 PAdES B-B：
  - 不連時戳伺服器（TSA）；
  - 不下載 OCSP／CRL；
  - 簽署時間取本機時鐘，並在介面註明。
- 憑證與私鑰透過 Windows 憑證存放區與 CryptoAPI／CNG 取得：
  - 智慧卡（自然人憑證、工商憑證）由讀卡機的 minidriver 處理 PIN；
  - FastPDF 不讀取也不保存私鑰或 PIN。
- 先完成評估文件（可行性、測試方式、法律效力的限制），再決定實作範圍。

## Consequences

- spec §9 對這些功能的禁止，由本 ADR 取代；其餘 V0.1 禁止項目不變：OCR、AI、cloud、account、login、sync、telemetry、analytics、online service、compression、convert。
- `CLAUDE.md` 的 Non-negotiables 改為「不在原處修改 PDF；寫入只能透過 ADR 0013 的隔離另存功能」，`docs/DEVELOPMENT.md` 的禁止清單同步修改。
- 執行檔會變大：PDF 物件序列化、字型 subset，以及簽章相關的 Windows API 呼叫。新增的依賴照 dependency policy 審查授權與數量，並更新 `THIRD_PARTY_LICENSES.md` 與 `THIRD_PARTY_NOTICES.txt`。
- 新增測試：
  - 重寫器以 corpus 中每個未加密的檔案做往返測試：重寫後重新 render，必須與原檔逐像素相同；
  - 各項功能有自己的 fixture 與測試。
- 閱讀的 KPI 不能退步：寫入功能完成後重跑 B-1 與 B-8。

## Alternatives considered

- **獨立的工具程式**（`fastpdf-tools.exe`）：閱讀器完全不受影響，但使用者要切換程式，而且要維護第二套 UI。子行程隔離已經能達到同樣的穩定性。
- **維持唯讀**：不符合 owner 的實際需求。
- **蓋白色方塊再寫字的「文字修改」**：原文仍然留在檔案裡，可以被搜尋與複製，會誤導使用者以為已經改掉，不採用。
- **所有修改都用 incremental update**：原文會留在之前的 revision 裡，可以被還原。刪頁與改字需要真正移除內容，所以只有簽章使用 incremental update。
- **用 `lopdf` 處理物件圖**：會整份載入，又是另一套 parser，和 render 用的 hayro 可能對壞檔有不同的解讀。直接用已經在依賴樹裡的 hayro-syntax 讀取，寫出端自己序列化。

## Validation

- 每項功能都要有測試：輸出檔用 hayro 重新開啟、render，並與預期比對。
- 重寫器的往返測試涵蓋 corpus 中所有未加密、可正常開啟的檔案。
- 寫入功能完成後重跑 B-1 與 B-8，確認閱讀的 KPI 沒有退步。
