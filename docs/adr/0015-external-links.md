# ADR 0015 — 文件中的連結與外部連結（http、https、mailto）

- 狀態：Proposed（round 1；PR 合併時改為 Accepted）
- 日期：2026-10-25
- 相關：spec §9（不編輯 annotation）、§15–§16（cache 預算）、§20（極簡介面）、§24／§25（惡意輸入）、§27（介面語言）、§31（log）、§46（開發工具）；CLAUDE.md non-negotiables「No network」；ADR 0002（engine 抽象）、ADR 0004（memory budget）、ADR 0008（out-of-process rendering）、ADR 0014（提示卡）

## Context

- PDF 的 link annotation 有很多種動作：文件內跳頁（`GoTo`、具名目的地）、外部網址（`URI`），以及會啟動程式或開啟其他檔案的 `Launch`、`GoToR`、`GoToE`、`JavaScript`、`SubmitForm` 等。
- Engine 早就讀得到連結（hayro adapter、render host 的 `Links` 指令），但 UI 從來沒有用；連結點了沒有反應。
- 網址來自文件，是不可信的輸入。Windows 的 `ShellExecute` 會啟動任何為該 scheme 註冊的程式：`file:`、`ms-msdt:`（Follina）、`search-ms:`、UNC 路徑，或在處理程式命令列的 `"%1"` 裡夾帶參數。GPUI 的 `cx.open_url` 直接呼叫 `ShellExecuteW`，不檢查 scheme，失敗也只寫 log。
- FastPDF 的原則是「不連網」：FastPDF 本身不做任何網路連線、不送 telemetry。
- Owner 的立場（round 1 功能分析時）：http、https、mailto 連結在**每次點擊都確認**、並顯示完整網址後，才交給預設的瀏覽器或郵件程式；`file:`、`javascript:`、Launch、GoToR 與其他所有動作一律不執行；FastPDF 本身不連線。

## Decision

### 1. 文件內的連結

- 點一下（按下與放開在同一個連結上，移動不超過 4 px，即 Windows 的拖曳門檻）就跳到目的地，經由 `ReaderView::jump_to_destination`，「上一頁」可以回去。拖曳超過門檻（即使拖回原處）仍是選取文字；雙擊的第二下不會再跟隨連結；按住左鍵時放開其他按鍵不算點擊。
- 跳頁保留目前的縮放：`XYZ` 的 zoom 與 left 和 outline 一樣不套用。
- 滑鼠停在連結上時游標變成手形，左下角的小提示說明會發生什麼：「前往第 3 頁」、「開啟網頁：host」、「寄送郵件給：收件人」（query 也指定收件人或其他非文字欄位時顯示完整網址，`mailto:a@x?bcc=b@y` 不能看起來只寄給 a@x），或「FastPDF 不會開啟這個連結：…」與原因。

### 2. 讀取連結的成本與上限

- 開檔時不讀任何連結。滑鼠第一次移到某一頁上時才在背景讀那一頁（經過 `GuardedDocument`，預設由 render host 讀），同時最多兩頁，另有一頁排隊；捲動時不讀，畫面在不動的游標下移動時只用 cache 重新判斷。
- `links` cache 有 2 MiB 預算、retention 15（在 thumbnails 與 text 之間），登記在 `MemoryBudgetManager`，dev overlay 看得到。
- 惡意文件的上限：
  - hayro adapter（`crates/fastpdf-engine-hayro/src/links.rs`、`dest.rs`、`nametree.rs`）：`/F` 為 Hidden 或 NoView 的連結略過；每頁最多看 20,000 個 `/Annots` 項目、回傳 10,000 個連結，同一個 annotation 重複列出只讀一次；同一次呼叫中，參照到的物件每個角色只讀一次、具名目的地會記住、名稱樹每個節點只讀一次（`/Limits` 不包含的子樹不讀內容），讀取的項目數、位元組數與走訪次數都有預算；超過 8 KiB 的網址不傳出，一次呼叫最多傳出 1 MiB 的網址。
  - core（`crates/fastpdf-core/src/links.rs`）再檢查一次：目的頁必須存在、矩形必須有限且有面積，每頁最多 1,024 個連結、64 KiB 的網址。

### 3. 允許的外部網址

`fastpdf_shell::check_external_uri`（`crates/fastpdf-shell/src/uri.rs`）是唯一能做出 `ExternalUri` 的地方，也是唯一能交給 Windows 的型別：

- 只接受 `http`、`https`、`mailto`（大小寫不拘）；其他 scheme 一律拒絕並說明是哪個 scheme。
- 原始文字超過 2048 bytes、含控制字元、bidi 控制字元、零寬字元、soft hyphen 或 BOM 的拒絕（URL parser 會默默刪掉 tab 與換行，`java\nscript:` 會變成 `javascript:`）。
- 以 WHATWG URL parser（`url` crate，本來就在 GPUI 的依賴樹中）正規化：國際化網域轉成 punycode（`bücher.example` → `xn--bcher-kva.example`），不安全的字元 percent-encode。不是絕對網址的（`www.example.com`、檔案路徑、UNC）拒絕。
- 網址含帳號或密碼的拒絕（`https://bank.example@evil.example/` 看起來像銀行）。
- 正規化後必須仍不超過 2048 bytes，而且只含可列印的 ASCII、沒有 `"`：無法在處理程式的命令列夾帶參數。
- `mailto:` 的收件人可以在 path 或只在 query（RFC 6068）；空的 `mailto:` 拒絕。query 只允許 `to`、`cc`、`bcc`、`subject`、`body`、`in-reply-to`（大小寫不拘），其他欄位一律拒絕並說明是哪個欄位：有些郵件程式會依 `attach`／`attachment` 夾帶本機檔案，指向 UNC 路徑時還可能送出使用者的 NTLM 認證。郵件連結也不能有 fragment（`#`）或第二個 `?`：郵件程式自己拆解網址，有些只按 `?` 與 `&` 切開，藏在 fragment 裡的欄位會躲過檢查卻仍被套用。

### 4. 每次點擊都要確認

- 點擊允許的外部連結時，文件區顯示阻擋式的提示卡（與密碼共用 `crates/fastpdf-ui/src/prompt.rs`）：
  - 標題：「要在瀏覽器開啟這個連結嗎？」或「要用郵件程式開啟這個連結嗎？」；
  - 說明：「這個連結來自 PDF 文件，請確認網址可信再開啟。」；
  - 網頁連結另以粗體顯示「網站：host」，host 太長時省略開頭、保留結尾（host 要從結尾讀：`login.example.com.evil.example` 屬於 `evil.example`）；
  - 完整網址以等寬字型顯示，就是要交給 Windows 的那一串，至少顯示兩行，太長時可捲動；
  - 按鈕：「複製網址」、「取消」、「開啟」。
- 視窗很小時，先讓說明讓出空間；仍放不下時整張卡片可以捲動，順序是標題、網站、網址、按鈕，所以「開啟」出現在畫面上時，網站與網址一定在它上方。滑鼠停在外部連結上時的提示同樣保留 host 的結尾。
- 只有在「開啟」按鈕上的單一次滑鼠點擊才會開啟：Enter 不會（`ConfirmPrompt` 對連結刻意不做事），雙擊的第二下不算，鍵盤也不算。提示卡放在文件區中與點擊位置相反的一半（上方或下方），點擊的位置不會剛好落在按鈕上。Esc 與「取消」關閉提示卡。
- 提示卡顯示時鍵盤留在卡上（按工具列等使 reader 取得焦點時會交還給卡），只有回答或離開提示卡的指令有作用（`ReaderCommand::allowed_during_prompt`）。
- 沒有「不再詢問」，也沒有設定可以關掉確認。

### 5. 交給 Windows 的方式

- `fastpdf_shell::open_external`（`crates/fastpdf-shell/src/launch.rs`，unsafe 只在這個 crate）：在短暫的 `fastpdf-open-link` thread 上以 `COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE` 初始化 COM（Microsoft 對 `ShellExecuteEx` 的建議），呼叫 `ShellExecuteExW`（verb `open`、`SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI`、`SW_SHOWNORMAL`），不設 `SEE_MASK_NOZONECHECKS`。UI 在 background executor 上呼叫它。
- 失敗時（例如沒有註冊處理程式）左下角顯示錯誤提示「無法開啟連結：沒有可以開啟這種連結的程式」，直到使用者關閉或按 Esc；使用者在 shell 的對話框中取消則只寫 log。

### 6. 「不連網」的界線

FastPDF 本身仍然不做任何網路連線：沒有 DNS 查詢、沒有預先抓取、沒有 favicon 或預覽。網址只有在使用者看過完整網址並按下「開啟」後，才交給使用者自己的瀏覽器或郵件程式，由它們連線；這和使用者自己把網址複製到瀏覽器相同。滑鼠停留、開檔與捲動都不會開啟任何東西。這不違反「No network」。若之後要提供完全關閉外部連結的設定（`external_links = ask | never`），另行決定。

### 7. 永不執行的動作

`Launch`、`GoToR`、`GoToE`、`JavaScript`、`Named`、`SubmitForm`、`ResetForm`、`ImportData` 等所有 `GoTo` 與 `URI` 以外的動作、找不到的具名目的地，以及 `file:`、`javascript:`、`data:`、`vbscript:`、`ms-*`、`search-ms:` 等所有非白名單 scheme，FastPDF 一律不執行，只在提示裡說明原因。這些動作目前在 engine API 都是 `LinkTarget::Unsupported`；若要顯示更精確的原因（「這個連結會啟動程式」），需要 `LinkTarget::Blocked` 與 render host protocol 的變更，暫不做。

### 8. Log 與隱私

info 等級的 log 只寫頁碼，以及外部網址的 scheme 與 host；完整網址只在 debug 等級。

### 9. 開發腳本

`link-hover`、`link-click`、`link-drag`、`link-prompt=open|copy|cancel` 與 `wait-prompt=link`。開發腳本一律 dry run：「開啟」只寫 log「dev script: would open …」，「複製網址」也只寫 log、不碰剪貼簿，絕不啟動瀏覽器或郵件程式。

## Consequences

- 閱讀路徑：開檔、第一頁與 idle 不變（不讀連結、沒有計時器）；滑鼠第一次移到某頁時多一次 render host 的 `Links` 請求（Work lane，與 render 分開）。
- 依賴：`url` 2.5.8 原本就在依賴樹（授權報告與 notices 不變）；windows-sys 多開 `Win32_System_Com` feature，沒有新 crate。
- 行為改變：hidden／NoView 的連結不再被視為連結（之前也從未被跟隨）。
- 沒有新的快捷鍵；`PromptKind` 多了 `OpenLink`。

## Alternatives considered

- **GPUI 的 `cx.open_url`**：沒有 scheme 檢查，失敗只寫 log，使用者看不到。
- **Windows TaskDialog（`window.prompt`）**：標題只有英文、第一個按鈕是預設（Enter 就會開啟），開發腳本也無法操作。
- **Enter 確認開啟**：容易在連續按鍵時誤開；只接受滑鼠點擊。
- **「這份文件不再詢問」**：一份惡意文件就能讓之後的點擊直接開啟；不提供。
- **把 `www.` 開頭的文字當成 http**：Acrobat 會這樣做；round 1 視為格式錯誤，比較保守。
- **每頁開檔時就讀連結**：違反「不處理使用者看不到的東西」，也拖慢開檔。

## Validation

- 單元與整合測試：`crates/fastpdf-shell/src/uri.rs` 的網址表（接受、正規化結果、每種拒絕原因）；`crates/fastpdf-core/src/links.rs`（檢查、上限、最上層連結、點擊門檻、cache）；`crates/fastpdf-engine-hayro/tests/links.rs`（flags、不執行的動作、反向矩形、網址上限、具名目的地、名稱樹預算、連結洪水、無法解析的陣列項目、fixtures）；i18n 與開發腳本的解析。
- Fixtures：`small-text/links-actions.pdf`（每種連結一個標示框，座標見 `fixtures/README.md`）、`malformed/links-flood.pdf`。
- GUI 檢查與待 owner 手動確認的項目見 round 1 的 PR 說明；owner 需手動確認一次：實際點擊網頁連結並按「開啟」，預設瀏覽器會開啟該網址。
