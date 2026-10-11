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
| 修改文字 | 重寫成新檔：從第一個改動的字起移除原本的字形，以內嵌的系統字型 subset 畫上新文字（§6） |
| 數位簽章 | 在原檔之後附加 incremental update（§3），PAdES B-B |

修改文字的範圍有限制：只處理頁面本身的 content stream，不處理 Form XObject 與註解裡的文字，也不處理掃描檔上看不見的辨識文字、直書、Type3 字型與讀不懂的編碼；做不到時明確拒絕並說明原因，不靠「蓋白色方塊」假裝修改。

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

### 6. 修改文字的做法

- **選取一行**：使用者點的是文字層（`TextLayer`）的一個 span，也就是選取文字用的同一份資料。寫入工作帶著 span 的位置與文字；寫入端重新解讀頁面，取出中心點落在這個位置的字形，而且這些字形的文字必須和 span 相同（同一行畫兩次做粗體效果時，允許重複）。對不上就拒絕，絕不刪到別的文字。
- **找出每個字形來自哪個運算子**：
  - 運算子的位元組範圍來自 hayro 自己的 content parser（運算子名稱與 inline image 資料在 content buffer 裡的位置），所以和 render 用的解讀一致；
  - 在每個顯示文字的運算子外面加上 marked-content 標記後交給 hayro 解讀，記錄字形的 Unicode、位置、寬度與填色；
  - 字串切成字碼的方式照 hayro 的規則（simple font 一個位元組；CID font 依 CMap 取最短可對應的前綴）。
- **刪字**：一行保留到第一個改動的字為止（`kept_characters`）。被刪的字從原本的運算子裡拿掉，換成等於其前進寬度的 `TJ` 位移（字寬、字距 Tc，以及單位元組字碼 32 的詞距 Tw），所以頁面上其他字形的位置完全不變。之後沒有文字會接著目前位置畫時，結尾的位移省略。原本的 content stream 不再被輸出檔引用，刪掉的字不會留在檔案裡。
- **新字**：
  - 字型是使用者選的系統字型：標楷體（預設，台灣公文常用）、新細明體、微軟正黑體。三者的 OS/2 fsType 都是 8（可編輯嵌入），寫入端仍會檢查授權，不允許嵌入或子集化的字型一律拒絕；
  - 以 typst 的 `subsetter` 只嵌入用到的字形，寫成 Type0／CIDFontType2（Identity-H、`/CIDToGIDMap /Identity`），附 ToUnicode，新字可以選取、搜尋與複製；
  - 從被刪的第一個字（或保留的最後一個字的結尾）開始畫，大小、方向與填色（含「填色加描邊」的假粗體）沿用被取代的字；
  - 新字放在該行所在文字物件（`BT` ... `ET`）之後，以 `q` / `Q` 包住、依當時的 CTM 換算，因此 content 裡的文字順序就是閱讀順序，之後畫的內容（印章、框線）也照樣蓋在上面。文字物件內改了 CTM（規格不允許，但有檔案這樣做）或沒有結束時，新字改放在 content 最後，content 以 `q` / `Q` 包住；此時若 content 的 `Q` 比 `q` 多，無法確定繪圖狀態，拒絕修改。
- **驗證**：輸出檔寫好後，重新解讀每個修改過的頁面：必須畫出原本的字形（減去刪掉的）、位置不變（誤差 0.05 pt 內），新字在預期的位置、讀得出預期的文字。保留的字形對不上表示這一行無法可靠地修改，回報為拒絕；新字對不上則是驗證失敗。兩者都不會留下輸出檔。
- **介面**：工具列「編輯 > 修改文字…」開啟面板；在頁面上點一行，就在原位出現可輸入（支援注音輸入法）的欄位，Enter 套用、Esc 取消。預覽以紙張色蓋住該行要改的部分，再用所選字型畫出新字；畫面旋轉時只標示位置。「另存新檔…」把文字修改與印章一起寫入。

## Consequences

- spec §9 對這些功能的禁止，由本 ADR 取代；其餘 V0.1 禁止項目不變：OCR、AI、cloud、account、login、sync、telemetry、analytics、online service、compression、convert。
- `CLAUDE.md` 的 Non-negotiables 改為「不在原處修改 PDF；寫入只能透過 ADR 0013 的隔離另存功能」，`docs/DEVELOPMENT.md` 的禁止清單同步修改。
- 執行檔會變大：PDF 物件序列化、字型 subset，以及簽章相關的 Windows API 呼叫。新增的依賴照 dependency policy 審查授權與數量，並更新 `THIRD_PARTY_LICENSES.md` 與 `THIRD_PARTY_NOTICES.txt`。
  - 修改文字新增 `subsetter`（typst，MIT OR Apache-2.0；關閉 variable-fonts feature 後只依賴已在依賴樹裡的 `rustc-hash`），並直接使用已在依賴樹裡的 `skrifa`。
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

結果（2026-10-11，重寫器、另存副本、頁面工具）：

- 重寫器的 corpus 往返測試（`cargo test --release -p fastpdf-engine-hayro --test write -- --ignored`）：每個未加密的檔案逐頁重寫後重新 render，6,864 頁全部逐像素相同。涵蓋 object stream、xref stream、損壞的 xref、2000 頁的檔案、CJK 與透明度。
- 整合測試（`crates/fastpdf-engine-hayro/tests/write.rs`）：
  - 擷取後，刪掉的頁面內容不在輸出檔裡；指向刪除頁的書籤目的地變成 `null`；
  - 重排、旋轉、重複頁、巢狀頁面樹的繼承屬性、合併；
  - 印章的位置、方向與透明度（render 後檢查像素）；
  - 副本逐位元組相同；加密檔被拒絕；任何一個輸出失敗時不留下檔案；不覆寫來源。
- GUI 實測（dev script）：另存副本、擷取、刪除、旋轉、倒序重排、分割、合併。輸出另以 pypdf 讀取，確認頁數、`/Rotate`、頁序與可擷取的文字。

結果（2026-10-11，印章）：

- 透明背景的 PNG 印章放在兩頁上，其中一頁是在畫面轉 90 度時放的。另存後以 hayro render：
  - 第一個印章是正的，透明處看得到底下的文字；
  - 第二個印章朝左轉 90 度，和放置當時畫面上看到的方向一致。
- 預覽與輸出一致：預覽依「目前畫面方向 + 印章在頁面中的方向」選擇四個方向之一。

結果（2026-10-11，修改文字）：

- 整合測試（`crates/fastpdf-engine-hayro/tests/text_edits.rs`）：
  - 保留一行的開頭、從第一個改動的字重畫：文字層再讀成完整的一行，保留的字與其他行位置不變，舊字不在檔案裡，其他區域 render 逐像素相同；
  - 刪掉的字之後還有同一個 `TJ` 或下一個 `Tj` 接著畫（含字距、詞距）時，後面的字位置不變；刪除整行時不嵌入字型；
  - 以 CTM 放大、填色加描邊的文字：新字大小相同，顏色與描邊寬度依當時的 CTM 換算；
  - 多筆修改共用一個字型物件；以修改產生的 CID 文字（Identity-H）可以再修改；
  - 拒絕：Form XObject 裡的文字、看不見的辨識文字、狀態無法確定的 content、字型沒有的字、找不到的字型檔、文字不符或同一行改兩次。
- corpus 測試（`cargo test --release -p fastpdf-engine-hayro --test text_edits -- --ignored`）：27 份含文字的文件（繁中公文與檢驗報告的內嵌 TrueType 子集、Type0、未內嵌 MSung，日文，標準 14 字型、Type1、Type3、大量文字），每份最多 3 頁、每頁 6 行，把最後一個字改成「改」。除了直書文字（拒絕，Unsupported）與一行看不見的文字（拒絕，Invisible）之外全部寫入成功，並通過寫入端的重新解讀驗證。
- GUI 實測（dev script，虛構公文樣本）：修改兩行（日期、承辦人），面板列出修改、頁面預覽新字、在原位編輯的欄位、另存後重新開啟。輸出另以 pypdf 檢查：舊文字不在任何 stream 裡，新文字可以擷取（layout 模式讀成完整的一行）。

結果（2026-10-11，數位簽章）：依 `docs/digital-signatures.md` 實作 PAdES B-B 的增量更新簽章，新增 `fastpdf-sign` crate（Windows 憑證存放區與 CMS，只依賴已有的 `windows-sys`）。測試與 pyHanko 的獨立驗證結果記在該文件的「實作結果」。重寫已簽章的文件時，原有簽章的值會被移除並在存檔通知中說明。
