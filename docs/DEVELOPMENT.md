# Development Guide

本文件把 spec（`docs/SPEC.md`）中與「怎麼開發」有關的規則整理成可執行的流程。產品方向與架構以 spec、`docs/PROJECT_AUDIT.md` 與 `docs/adr/` 為準。

## 核心優先序（spec §53）

1. Latency → 2. Memory → 3. Responsiveness → 4. Correctness → 5. Compatibility → 6. Features

Correctness 與 security 不能為了效能被破壞。每加一個功能都先問：**它會不會讓開 PDF 變慢？** 會的話就 lazy load、isolate、defer、預設關閉，或乾脆不加。

## 每一次修改的流程（spec §38）

```text
baseline → test → small architecture change → benchmark → commit → next change
```

- 不做「rewrite the entire project」等級的大改。拆成可以單獨 review、單獨 benchmark 的小步驟。
- 動到 renderer、cache、scheduler、document loading 的修改都要跑 benchmark：

  ```bash
  cargo run --release -p fastpdf-bench -- corpus fixtures/generated/manifest.json --repeat 3 --out benchmarks/runs/<change>.json
  cargo run --release -p fastpdf-bench -- compare benchmarks/baseline.json benchmarks/runs/<change>.json
  ```

- 效能修改的說明一律包含 **Before／After／Why／Tradeoff**（格式見 `docs/profiling.md` §7）。達不到目標也要照實寫。
- 還沒 benchmark 就不要宣稱「比較快」。

## 提交前檢查

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
python tools/check_engine_isolation.py
python tools/license_report.py --all-features --check
```

## GUI 檢查

畫面上看得到的改動用 `tools/gui-check.ps1` 驗證：它以 `FASTPDF_DEV_SCRIPT`（步驟見 `crates/fastpdf-ui/src/devscript.rs` 開頭的表）驅動 release build，把 `FASTPDF_SETTINGS_FILE`、`FASTPDF_RECENT_FILE` 指到 `-OutDir` 裡的檔案，`FASTPDF_PRINT_TO_FILE` 指到 `-OutDir\print.out`（列印工作不會送到印表機），log 寫進 `-OutDir\fastpdf.log`，每個 `mark=PATH` 存成 `PATH.png`；`expect-page=N`、`expect-zoom=P` 不成立時 script 會停下，log 寫出實際的頁碼或縮放。啟動前等鍵盤滑鼠閒置（`-IdleSeconds`，預設 20 秒）且沒有 cargo／rustc 在跑；結束時關掉本次啟動的所有 FastPDF 行程，並比對 `%APPDATA%\FastPDF` 內檔案的修改時間，印出 PASS 或 FAIL。自己啟動 FastPDF 做測試時，這兩個變數一定要設成非空的路徑：PowerShell 的 `$env:X = ''` 會刪掉變數，FastPDF 就改用 `%APPDATA%\FastPDF` 裡使用者真正的設定與最近開啟清單。`%APPDATA%\FastPDF` 只看修改時間，不刪除、不改寫、不讀出內容；簽章相關的檢查先跑 `sign-test`，不列出也不截圖 Windows 憑證存放區裡的個人憑證；script 在 `sign-test` 之前開簽章面板（`sign-panel`、`cmd=Sign`）時，driver 拒絕啟動。參數說明見 `Get-Help tools/gui-check.ps1 -Detailed`。

```bash
pwsh -NoProfile -File tools/gui-check.ps1 -Exe target/release/fastpdf.exe -Pdf fixtures/generated/large-text/dense-300p-times.pdf -OutDir <output> -Script "wait-open; idle; page=12; expect-page=12; mark=<output>/p12; cmd=Quit"
```

## 本機狀態檔（設定、最近開啟、閱讀位置）

FastPDF 只在本機保存三份狀態，全部是 std-only 的文字檔，讀取時遇到壞資料只略過那一筆，不會讓程式出錯；寫入都經過暫存檔加 rename，當機不會留下寫一半的檔案。

| 檔案 | 預設位置 | 覆寫方式 | 空字串時 |
|---|---|---|---|
| UI 設定 | `%APPDATA%\FastPDF\settings.toml` | `FASTPDF_SETTINGS_FILE` | 用預設值，不寫入 |
| 最近開啟清單 | `%APPDATA%\FastPDF\recent.txt` | `FASTPDF_RECENT_FILE` | 不讀不寫 |
| 閱讀位置 | 最近開啟清單旁的 `<主檔名>-positions.txt`（預設 `recent-positions.txt`） | 跟著 `FASTPDF_RECENT_FILE` | 不讀不寫，也不建立檔案 |

閱讀位置（`crates/fastpdf-core/src/positions.rs`、`crates/fastpdf-ui/src/reading.rs`）：

- **內容**：每份文件一行，最新的在前，欄位以 TAB 分隔、路徑放最後：頁碼、頁內位置（該列高度的比例）、水平位置、縮放（`fit-width`、`fit-page` 或倍率）、檢視旋轉、頁數、檔案大小與修改時間。最多 200 份文件、512 KiB；超過大小上限或不是 UTF-8 的檔案視為空的，下次寫入時取代。路徑只拿來比對，不會被開啟；比對用絕對路徑、不分大小寫（和最近開啟清單相同，不做 canonicalize）。
- **還原規則**：檔案大小、修改時間與頁數都沒變時，還原頁碼、頁內位置、水平位置、縮放與旋轉；檔案變了但那一頁還在時，只還原頁碼與頁內位置，縮放用預設值；那一頁已經不存在時什麼都不還原，從第 1 頁開始。頁面配置（單頁、連續、雙頁）維持全域設定。
- **第一頁保證**：查詢與還原那一列的 geometry 都在開檔執行緒上完成，session 在第一個 frame 之前就套用位置，所以第一個規劃、render 的就是還原的那一頁，不會先畫第 1 頁。命令列開檔的 early session、之後的開檔與輸入密碼解鎖都走同一個入口（`reading::open_for_reading`、`with_start`、`apply_start`）；之後要讓文件從第 1 頁以外的地方開始的功能也要用它。
- **寫入時機**：不會每次捲動就寫。畫面停止移動 2 秒後由一個一次性 timer 排入記錄、在背景寫檔（idle 時沒有 timer）；關閉文件或切換文件時在背景寫；結束程式（Ctrl+Q 與視窗的關閉鈕）時在 UI 執行緒上同步寫完。當機最多遺失最後 2 秒的移動。
- **多個行程**：每次寫入都在 `<檔名>.lock`（留在旁邊的空檔案）上取得所有 FastPDF 行程共用的鎖，重新讀檔、合併後再取代，所以多個視窗不會互相覆蓋；鎖最多等 0.5 秒，等不到時這次不寫，記錄留到下一次。
- **設定**：設定面板的「記住閱讀位置」（`remember_position`，預設開啟）。關閉後不再讀取也不再寫入，但保留既有的檔案；在文件開著時打開，會從當下位置開始記錄（不知道檔案身分，下次以預設縮放還原頁面位置）。
- **測試與 benchmark**：位置檔跟著 `FASTPDF_RECENT_FILE`，`tools/gui-check.ps1` 會把它放在 `-OutDir`，`tools/bench-app` 把變數設成空字串所以完全不使用；自己啟動時一樣要把變數設成非空的路徑，不要碰 `%APPDATA%\FastPDF`。

## Dependency policy（spec §36）

每個新依賴都要在 PR 中回答：

| 問題 | 說明 |
|---|---|
| Why? | 解決什麼問題，std 或既有依賴為何不夠 |
| Size? | 增加多少 crate（`cargo tree -e normal` 前後比較） |
| Compile impact? | clean build 時間變化 |
| Runtime memory? | 載入後的常駐成本 |
| License? | 執行 `python tools/license_report.py`，不可出現待審項目 |
| Maintenance? | 最近一年的維護狀況、維護者數量 |
| Can std do it? | 能用 std 就用 std |

不要為了一個 helper function 引入 30 個 crate。workspace 的 `Cargo.toml` 中每個第三方依賴旁都要有一行理由註解。

## License policy（spec §37）

- 優先 MIT、Apache-2.0、BSD。GPL／AGPL／LGPL 一律不進 shipping 的依賴圖；MPL-2.0 需要個案審查。
- `THIRD_PARTY_LICENSES.md` 由 `tools/license_report.py` 產生，依賴變動後重新產生。
- 從 upstream 移植的程式碼（例如 pdf-reader-gpui，Apache-2.0）要保留原始 copyright header，並登記在 `THIRD_PARTY_LICENSES.md` 的 Ported source 章節。

## Engine isolation（spec §4、§42）

- UI、core、render、search 只認得 `fastpdf-engine-api` 的 domain types。
- 只有 `fastpdf-engine-<name>` 可以依賴該 engine；`tools/check_engine_isolation.py` 會檢查。
- 所有 engine 呼叫都透過 `GuardedDocument`（驗證、guardrail、panic isolation）。release profile 必須維持 `panic = "unwind"`。

## Robustness（spec §24–§25）

- PDF 是不可信輸入。parser／renderer 的輸入一律假設為 hostile。
- 不允許 `unwrap()`／`expect()` 出現在處理文件資料的路徑上（clippy 會警告；測試例外）。
- 新的 guardrail 放在 `ResourceLimits`，並在 `fixtures` 的 malformed 類別加上對應的測試檔。
- **Windows Defender 注意**：惡意 PDF 測試語料（例如 zpdf upstream 的 bug tracker 收集檔）可能被 Defender 隔離。不要 commit 這類檔案；需要時放在 `fixtures/local/`，並在本機將該資料夾加入排除清單（由使用者自行決定）。

## ADR（spec §39）

重大架構決策寫在 `docs/adr/NNNN-title.md`：Context／Decision／Consequences／Alternatives considered／Validation。改變既有決策時新增一份 ADR 並在舊的那份標註 Superseded，不要直接改寫歷史。

## V0.1 禁止事項（spec §9、§48）

OCR、AI chat、cloud storage、account、login、sync、annotations 編輯、convert、compression、online service、telemetry、analytics、auto-update framework、plugin marketplace。也不要：全面 rewrite、換 UI framework、引入 Electron／Chromium + PDF.js（spec §34）、改用 Tauri（spec §35，除非有 benchmark 證據並先寫 ADR）、大量增加依賴、為了美觀重寫 UI、一開始就建 plugin system、一開始就完整實作兩套 renderer。

PDF 寫入（另存副本、頁面操作、印章、文字修改、數位簽章）依 ADR 0013 進行：一律另存新檔、在 `--write-job` 子行程執行、閱讀路徑零成本。不在原處修改 PDF。

加密文件的開啟密碼依 ADR 0014 處理：只用 `fastpdf_engine_api::Password` 傳遞（`Debug` 會遮蔽、drop 時清除），不寫進 log、檔案、命令列參數或環境變數；用密碼開啟的文件不寫入，新的寫入指令要列入 `ReaderCommand::writes_document()`。

文件中的連結依 ADR 0015 處理：任何要交給 Windows shell 的東西都只經過 `fastpdf_shell::check_external_uri`（只允許 http、https、mailto）與 `fastpdf_shell::open_external`，而且每次都要使用者在提示卡上按「開啟」；`Launch`、`GoToR`、JavaScript、`file:` 等一律不執行。開發腳本一律 dry run，不啟動瀏覽器、不碰剪貼簿。

## Logging（spec §31）

- 開發：`FASTPDF_LOG=debug`（或 `FASTPDF_LOG=fastpdf_render=trace,info`）。
- Release 預設只輸出 warn／error。熱路徑只用 `trace!`。
