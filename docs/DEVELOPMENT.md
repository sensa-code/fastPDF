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
python tools/license_report.py --check
```

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

PDF editing、OCR、AI chat、cloud storage、account、login、sync、annotations 編輯、signature、merge／split／convert、compression、online service、telemetry、analytics、auto-update framework、plugin marketplace。也不要：全面 rewrite、換 UI framework、引入 Electron／Chromium + PDF.js（spec §34）、改用 Tauri（spec §35，除非有 benchmark 證據並先寫 ADR）、大量增加依賴、為了美觀重寫 UI、一開始就建 plugin system、一開始就完整實作兩套 renderer。

## Logging（spec §31）

- 開發：`FASTPDF_LOG=debug`（或 `FASTPDF_LOG=fastpdf_render=trace,info`）。
- Release 預設只輸出 warn／error。熱路徑只用 `trace!`。
