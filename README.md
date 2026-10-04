# FastPDF

以 Rust 為核心、Windows 11 優先的極速 PDF Reader。目標不是功能數量，而是：

- 開檔即看：開檔不需要處理整份 PDF，第一頁立刻出現
- 捲動與 zoom 不卡、不空白（tile-based rendering + progressive rendering）
- 大型 PDF 不爆 RAM：每個 cache 都有硬性上限
- Idle 時 CPU 趨近 0、零 telemetry、零網路、零登入

> 產品規格：[`docs/SPEC.md`](docs/SPEC.md)（原始檔 `FastPDF.docx`）。
> Audit、架構決策與計畫：[`docs/PROJECT_AUDIT.md`](docs/PROJECT_AUDIT.md)。

## 狀態

| Milestone | 內容 | 狀態 |
|---|---|---|
| M0 Audit | pdf-reader-gpui／zpdf／hayro／GPUI audit、架構提案 | ✅ `docs/PROJECT_AUDIT.md` |
| M1 Baseline | `fastpdf-bench` + `benchmarks/baseline.json` | ✅ |
| M2 Engine isolation | `fastpdf-engine-api`，只有 adapter 能依賴 engine | ✅ `tools/check_engine_isolation.py` |
| M3 zpdf PoC | `fastpdf-engine-zpdf`（feature `engine-zpdf`） | ✅ |
| M4 Renderer comparison | `docs/engine-comparison.md` | ✅ Hayro 為預設 engine（ADR 0007） |
| M5 Tile renderer | tile／scheduler／cache + GPUI viewport；B-3／B-4（2 workers；tile 508，對齊 GPU atlas） | ✅ |
| M6 Memory budget | `MemoryBudgetManager`、overlay 分項、B-5 驗證（大型 PDF 不爆 RAM） | ✅ |
| M7 UX | sidebar（outline、縮圖）、搜尋、選取／複製、列印、recent files、深色外觀、夜間模式、設定、繁體中文介面、平滑捲動 | V0.1 功能清單完成；檔案關聯目前只有命令列（`--register-file-types`）；安裝程式待做 |
| 發佈 | 可攜版 zip：icon／版本資訊、第三方授權全文、SHA-256、smoke test | ✅ `tools/package.ps1`、`docs/RELEASE.md`（公開發佈前仍有 4 個 crate 缺授權全文） |
| Render host | engine 移到獨立 process（ADR 0008）：crash、配置失敗、卡住只會結束 host | ✅ Windows 的預設（`hayro-isolated`）；`--engine hayro` 在 process 內 render。吞吐量與啟動時間和 in-process 相同（`docs/benchmarks/render-host.md`） |

## KPI（spec §29，最終版 `b7797fc` 實測）

預設 engine 是 render host（`hayro-isolated`），idle 時有 3 個 process：app、文件 host、待命 host。

| 指標 | 目標 | 實測 | |
|---|---|---|---|
| 執行檔 | < 30 MB | 16.1 MiB | ✅ |
| 小檔首頁 | < 200 ms | 中位數 204 ms（200–216 ms）；第一個 frame 就是清晰的第一頁，下限是 GPUI 啟動（ADR 0009） | ❌ 差約 4 ms |
| Idle RAM（private working set） | < 50 MB | 28.3 MB（3 頁文件，3 個 process 合計）；private bytes 114 MB，大部分是 GPU driver | ✅ |
| 大型 PDF | 不需完整掃描 | 2000 頁捲到底，private 穩定在 190–220 MiB（B-5）；engine 層與 M1 baseline 相同（B-1 配對比較） | ✅ |
| Idle CPU | 接近 0 | 單核 0.3–0.5%；FastPDF idle 時 0 frame，但 GPUI 的 vsync 迴圈仍每秒喚醒主執行緒約 95 次 | ⚠️ |
| 網路／telemetry | 0 | 0 | ✅ |

量測機是高階桌機，每次啟動前都等系統負載降到 30% 以下。定義與細節見 `benchmarks/README.md`、`docs/benchmarks/b8-app.md`。

## 需求

- Windows 11、Visual Studio 2022 Build Tools（C++ workload）、Windows SDK
- Rust：版本由 `rust-toolchain.toml` 決定，rustup 會自動安裝
- Python 3.12 + [uv](https://docs.astral.sh/uv/)（只有產生測試語料時需要）

## 常用指令

```bash
cargo run --release -p fastpdf-app -- path/to/file.pdf   # 開啟 FastPDF
cargo run --release -p fastpdf-app -- --register-file-types --dry-run   # 檢視檔案關聯會寫入的 registry
cargo test --workspace                                  # 單元測試
uv run tools/fixtures/generate.py                       # 產生測試 PDF 到 fixtures/generated/
cargo run --release -p fastpdf-bench -- full fixtures/generated/small-text/<file>.pdf
cargo run --release -p fastpdf-bench -- corpus fixtures/generated/manifest.json --repeat 3 --out benchmarks/runs/mine.json
cargo run --release -p fastpdf-bench -- compare benchmarks/baseline.json benchmarks/runs/mine.json
```

## 結構

```text
crates/
  fastpdf-engine-api/    domain model + PdfEngine trait + GuardedDocument（零依賴）
  fastpdf-engine-hayro/  Hayro adapter
  fastpdf-engine-zpdf/   zpdf adapter（PoC，feature-gated）
  fastpdf-engine-remote/ render host：engine 在獨立 process 中執行（ADR 0008，Windows 的預設）
  fastpdf-cache/         byte-budget LRU + MemoryBudgetManager
  fastpdf-render/        scale bucket、tile grid、layout、viewport、scheduler、tile cache
  fastpdf-search/        lazy、incremental、可取消的全文搜尋
  fastpdf-core/          文件載入、DocumentSession、keymap、選取、recent files、memory monitor
  fastpdf-ui/            GPUI views（唯一依賴 GPUI 的 library crate）
  fastpdf-app/           執行檔 `fastpdf`
  fastpdf-print/         Win32 列印（分段 render）
  fastpdf-shell/         Windows 檔案關聯（per-user HKCU 註冊計畫）
  fastpdf-bench/         無 GUI 的 benchmark harness
docs/                    spec、audit、ADR、profiling、development guide
fixtures/                測試語料說明（產出不 commit）
benchmarks/              baseline 與量測紀錄
tools/                   fixture 產生器、license report、engine isolation 檢查、打包（package.ps1）、app 層 benchmark（bench-app）
```

## 開發規範

見 [`docs/DEVELOPMENT.md`](docs/DEVELOPMENT.md)：小步修改＋benchmark、dependency 與 license 政策、V0.1 禁止事項。

## 授權

本專案尚未選定授權（保留所有權利），以保留商業化選項（spec §37）。第三方依賴的授權見 [`THIRD_PARTY_LICENSES.md`](THIRD_PARTY_LICENSES.md)。
