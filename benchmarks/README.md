# Benchmarks

- `baseline.json`：M1 baseline（spec §41）。所有效能修改都和它比較。
- `runs/`：每次量測的輸出（不 commit）。
- 量測方法、指標定義、計畫（B-0 到 B-8）見 `docs/PROJECT_AUDIT.md` 的〈Benchmark Plan〉；profiler 用法見 `docs/profiling.md`。

## 產生 baseline 的程序

```bash
# 1. 測試語料（deterministic）
uv run tools/fixtures/generate.py --profile quick

# 2. 關掉其他重度負載（瀏覽器影片、編譯、索引），插電、電源模式「最佳效能」

# 3. 每個檔案 3 次獨立子 process，取 median
cargo run --release -p fastpdf-bench -- corpus fixtures/generated/manifest.json \
    --repeat 3 --timeout 120 --out benchmarks/baseline.json
```

## 讀數字時要注意

- 數字只能和**同一台機器、同一個 toolchain**（報告裡的 `machine`、`rustc`）的數字比較。
- 時間是 **warm file cache**（檔案已在 OS cache）。cold 數字要另外量並註明。
- `rss_peak_mb` 是該檔案子 process 的 peak working set，包含 harness 本身（約 5–10 MB）。
- `time_to_first_page_ms` 是 engine 端的「read + open + metadata + 第 1 頁 render」，**不含**視窗建立、GPU 上傳與合成；完整的 time to first visible page 由 app 層 benchmark（B-8）量。
- `status` 不是 `ok` 的檔案要逐一檢視：malformed 類別預期會是 `open_error` 或 `partial`；`crash`／`timeout` 一律是 bug。
