# ADR 0014 — 加密文件（開啟密碼）

- 狀態：Accepted（round 1，隨閱讀功能的 PR 一起合併）
- 日期：2026-10-24
- 相關：spec §24（錯誤處理）、§25（安全）、§27（介面語言）；ADR 0002（engine 抽象）、ADR 0008（out-of-process rendering）、ADR 0013（文件寫入）；`docs/upstream-issues/hayro.md` 草稿 #2

## Context

- engine 端早就能處理密碼：hayro 支援 Standard security handler（RC4 40–128 bit、AES-128、AES-256 R5／R6），render host 的 `Open` 指令帶著密碼，host 當掉重啟時也會重送。但 UI 一直用 `OpenOptions::default()` 開檔，需要 user password 的文件只會顯示「FastPDF 目前還不支援開啟加密文件」。
- 只設 owner password（user password 為空）的文件不需要密碼就能開啟，現在也照常開啟。
- 密碼是使用者的秘密，不能出現在 log、`Debug` 輸出、剪貼簿、命令列參數、環境變數、檔案或開發腳本的 log。
- 寫入功能在 `--write-job` 子行程執行：job 檔是 `%TEMP%` 裡的明文，路徑放在命令列上（ADR 0013）。

## Decision

### 1. 在視窗內詢問密碼

- 開檔得到 `PasswordRequired` 或 `InvalidPassword` 時，已載入的檔案（同一份 bytes 或檔案 handle，`LockedSource`）保留下來，文件區顯示阻擋式的提示卡（prompt card）：標題「需要密碼」、檔名、說明、密碼欄、「取消」與「開啟」。
- 每次嘗試都把密碼從欄位**移出**（不複製）交給一次背景開檔。密碼錯誤時欄位清空並重新聚焦，顯示「密碼不正確，請再試一次。」；次數不限。使用 render host 時，失敗的 host 會關閉，下一次用新的 host，維持 ADR 0008「一份文件一個 host」。
- 取消（Esc、「取消」、關閉文件）回到空白視窗，不加入最近開啟的檔案；再開同一個檔案要重新輸入。
- 提示卡顯示時，只有回答或離開它的指令有作用：取消、確認、結束、開啟其他檔案、關閉文件、全螢幕、dev overlay（`ReaderCommand::allowed_during_prompt`）。之後外部連結的確認也沿用同一張提示卡（`crates/fastpdf-ui/src/prompt.rs`）。
- 介面文字有繁體中文與英文（`crates/fastpdf-ui/src/i18n/password.rs`）；不用 Windows TaskDialog，因為它的標題只有英文、放不了密碼欄，開發腳本也無法操作。

### 2. 密碼的通道與在記憶體中的時間

- 型別：`fastpdf_engine_api::Password`。`Debug` 只顯示 `Password(<redacted>)`，沒有 `Display`，drop 時以 0 覆寫整個 buffer（含未使用的容量；只用 std：`fill(0)` 加 `std::hint::black_box`）。`OpenOptions.password`、render host 的 `Open` 指令與開發腳本的 step 都用這個型別，所以它們自動產生的 `Debug` 也不會洩漏。
- 唯一的通道：密碼欄 → `OpenOptions` → reader 行程的 `RemoteEngine`（保留一份，供 host 重啟時重開）→ named pipe 上的 `Open` frame（隨機名稱、first instance、拒絕遠端連線、檢查 client PID）→ host 的 `OpenOptions` → hayro（保留一份，供記憶體壓力時重開）。送出與收到的 `Open` frame 用完即清除；encoder 先檢查密碼之後的欄位並預留空間，buffer 不會在寫入密碼後被拒絕丟棄或搬移而留下副本。`Open` 的 wire format 沒有因為密碼而改變（之後 page labels 把 `PROTOCOL_VERSION` 升為 2，與本 ADR 無關）。
- 不經過：命令列參數、環境變數、檔案、剪貼簿、設定檔、最近開啟的檔案、log、開發腳本的 log、write job。
- 密碼欄（`TextInput` 的 secret 模式）：
  - 每個字顯示一個 •，GPUI 的文字排版與 line layout cache 只看得到 •；
  - 複製、剪下沒有作用；貼上會去掉換行；
  - 文字變更事件不帶內容；`Debug` 不顯示內容；
  - 上限 1024 bytes，buffer 事先保留，打字時不會搬移；欄位被丟棄時清除。
- 輸入法維持開啟，才能輸入中文密碼（例如 fixture 的 `測試密碼2026`）；組字中的文字也顯示成 •。代價：有些輸入法會學習輸入過的詞。
- 文件關閉後，reader 與 host 的兩份副本隨文件一起丟棄並清除。密碼不保存、不重用。
- 清除只是 best effort，以下不在範圍內：hayro 由密碼推導出的金鑰、host 讀取指令用的 `BufReader`（之後的指令不一定會覆寫到 `Open` frame 所在的位置）、kernel 的 pipe buffer、輸入法與 GPUI 的事件傳遞、GPUI 讀取剪貼簿時的暫存（FastPDF 自己拿到的副本會清除；系統剪貼簿本身仍保有使用者複製的內容）、WER crash dump。同一個使用者執行的其他程式本來就能讀取這些記憶體。

### 3. 用密碼開啟的文件不寫入

- 「編輯」選單的五個項目（另存副本、頁面工具、蓋章、修改文字、數位簽章）停用；從前一份文件留下來還開著的寫入面板在解鎖時關閉；快捷鍵（Ctrl+Shift+S）與開發腳本的寫入步驟在存檔對話框出現之前就被擋下，顯示「用密碼開啟的文件無法另存或編輯。」，不會啟動 writer。另一份文件的寫入還在進行時，它的提示（與取消按鈕）保持不變。
- 原因：writer 子行程要能開啟來源檔（寫完後也要重新開啟驗證），密碼就得放進 job 檔或命令列，違反 §2 的通道規則。
- 只有 owner password 的加密文件維持 ADR 0013 §4：由 writer 拒絕。

### 4. 權限（`/P`）不強制

hayro 不提供權限資訊。列印與複製和現在的 owner-only 文件一樣不受限制。若要依權限限制列印或複製，需要另一份 ADR。

### 5. hayro 的限制

- R2–R4（RC4、AES-128）只接受 user password：缺少 Algorithm 7，owner password 會被拒絕（upstream issue 草稿 #2）。`crates/fastpdf-engine-hayro/tests/adapter.rs` 的 `owner_passwords_by_revision` 把這個行為釘住：hayro 修好後測試會失敗，提醒更新本節。
- R2–R4 的密碼以 UTF-8 位元組比對，不是 PDFDocEncoding，非 ASCII 密碼在這些版本可能打不開；R5／R6 沒有做 SASLprep，並截斷到 127 bytes。
- 只支援 Standard security handler；憑證或公開金鑰加密的文件顯示「不支援的功能」。

### 6. 開發腳本

`password=TEXT`（輸入並確認）、`password-text=TEXT`（只輸入，用來截圖遮罩後的欄位）與 `password-compose=TEXT`（留下未完成的輸入法組字）只能用 fixture 的密碼。開發腳本只在 debug build 或開啟 dev overlay 時執行；每個 step 的 log 都顯示 `Password(<redacted>)`。

`FASTPDF_DEV_SCRIPT` 是環境變數：render host 與 `--write-job` 子行程會繼承 reader 的環境（建立子行程時沒有另給環境區塊），所以腳本裡的 fixture 密碼也在它們的環境裡。子行程不讀這個變數；這也是只能用 fixture 密碼的原因之一。

## Consequences

- 閱讀路徑：未加密文件每次開檔多一次 `DocumentSource` 的 Arc clone；啟動時不建立任何提示卡的物件（密碼欄在需要時才建立）；沒有計時器或動畫，閒置 CPU 不變。
- 加密文件每次嘗試多啟動一次 render host（約 10–15 ms，ADR 0008）再加上金鑰推導。實測（`fastpdf-bench full`，in-process，3 頁 fixture）：AES-256 R6 的 open 約 9–10 ms（user password 與只有 owner password 的檔案相同），AES-128 約 0.3 ms。
- `ReaderCommand` 新增 `ConfirmPrompt`（`Prompt` context 的 Enter）、`writes_document()` 與 `allowed_during_prompt()`；新的寫入功能必須列入 `writes_document()`。

## Alternatives considered

- **`Option<String>` 加手寫的 `Debug`**：每個帶著密碼的新結構都要記得手寫，容易漏；改用型別。
- **`zeroize` crate**：不在 Windows 的預設依賴樹裡，加入會改變授權清單；std 已能做到同樣的 best effort。
- **Windows TaskDialog（`window.prompt`）**：見 §1。
- **取消後保留文件區的提示與「輸入密碼…」按鈕**：多一種狀態；回到空白視窗，再開一次檔案即可。
- **關閉輸入法（像 Win32 `ES_PASSWORD`）**：中文密碼無法輸入。
- **允許「另存副本」照位元組複製**：writer 驗證時需要密碼，得另外開一條秘密通道給子行程，並修改 ADR 0013；暫不做。
- **同一檔案再開時沿用記憶體中的密碼**：多一份長期存在的副本；不採用。

## Validation

- 單元與整合測試：`Password` 與 `OpenOptions`、`Open` 指令的 `Debug`；清除後保留容量；host 重啟後用保留的密碼重開（`password_survives_host_restart`）；`owner_passwords_by_revision`；`open_loaded` 的鎖定、密碼錯誤（沿用同一份 bytes）與成功；密碼欄的位移換算（中文、emoji）；開發腳本 step 的 `Debug`。
- GUI 檢查（2026-10-24，release build，`tools/gui-check.ps1`，預設的 `hayro-isolated`）：
  - 繁體中文介面：提示卡、密碼錯誤的紅字、正確密碼後顯示第 1 頁與「檔名 - FastPDF」標題、編輯選單停用、Ctrl+Shift+S 顯示加密文件的提示且沒有產生 `fastpdf-write-*.job`、關閉後最近開啟的檔案列出這份文件；
  - 英文介面：Unicode 密碼 `測試密碼2026` 以 8 個 • 顯示，確認後開啟；
  - AES-128 的 user password 可以開啟；只有 owner password 的 AES-128 與 RC4-40 不詢問就開啟，編輯選單可用；
  - 取消（`prompt-cancel` 與 Esc）回到空白視窗，最近開啟的檔案沒有新增；提示卡顯示時，側邊欄、列印、尋找與上一頁的指令都沒有作用；
  - log 裡找不到任何 fixture 密碼，step 一律顯示 `Password(<redacted>)`；執行期間 render host 的命令列只有 `--render-host` 與 pipe 參數。
- 待 owner 手動確認：注音輸入法組字時只顯示 •；欄位裡按 Ctrl+C 不改變剪貼簿。
- 閱讀 KPI（2026-10-25，round 1 前後的 B-1 與 B-8 配對比較）：開檔、第一頁、idle 與記憶體沒有可判定的變化，見 [`docs/benchmarks/reading-round1.md`](../benchmarks/reading-round1.md)。
