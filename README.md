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
| M5 Tile renderer | tile／scheduler／cache + GPUI viewport | ✅ 第一版可用的視窗 |
| M6 Memory budget | `MemoryBudgetManager`、overlay、B-5 驗證 | 進行中 |
| M7 UX | sidebar、搜尋、選取、列印、設定… | 進行中 |

## 需求

- Windows 11、Visual Studio 2022 Build Tools（C++ workload）、Windows SDK
- Rust：版本由 `rust-toolchain.toml` 決定，rustup 會自動安裝
- Python 3.12 + [uv](https://docs.astral.sh/uv/)（只有產生測試語料時需要）

## 常用指令

```bash
cargo run --release -p fastpdf-app -- path/to/file.pdf   # 開啟 FastPDF
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
  fastpdf-cache/         byte-budget LRU + MemoryBudgetManager
  fastpdf-render/        scale bucket、tile grid、layout、viewport、scheduler、tile cache
  fastpdf-search/        lazy、incremental、可取消的全文搜尋
  fastpdf-core/          文件載入、DocumentSession、keymap、選取、recent files、memory monitor
  fastpdf-ui/            GPUI views（唯一依賴 GPUI 的 library crate）
  fastpdf-app/           執行檔 `fastpdf`
  fastpdf-print/         Win32 列印（分段 render）
  fastpdf-bench/         無 GUI 的 benchmark harness
docs/                    spec、audit、ADR、profiling、development guide
fixtures/                測試語料說明（產出不 commit）
benchmarks/              baseline 與量測紀錄
tools/                   fixture 產生器、license report、engine isolation 檢查
```

## 開發規範

見 [`docs/DEVELOPMENT.md`](docs/DEVELOPMENT.md)：小步修改＋benchmark、dependency 與 license 政策、V0.1 禁止事項。

## 授權

本專案尚未選定授權（保留所有權利），以保留商業化選項（spec §37）。第三方依賴的授權見 [`THIRD_PARTY_LICENSES.md`](THIRD_PARTY_LICENSES.md)。
