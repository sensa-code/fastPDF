# B-1：M1 baseline 與 HEAD 的配對比較

對應 spec §30（Performance Regression）。用來回答：從 M1 baseline 到現在，engine 層（read、open、第一頁、text、記憶體）有沒有退步。

## 結論

- **沒有退步。**
  - 84 個檔案的狀態完全相同：77 個 ok、4 個 partial、1 個 load_error、2 個 open_error。
  - 「到第一頁的時間」幾何平均比值 1.001，沒有任何檔案慢超過 10%。
  - 記憶體峰值的比值是 1.000。
- **一項改善**：`cad/a0-fem-mesh-hairlines.pdf` 的第一頁從 458 ms 降到 384 ms（−16%）。
- **baseline 不更新**：`benchmarks/baseline.json` 維持 M1 的版本。它是在安靜的機器上量的（量測前 CPU 2.8%），HEAD 的效能和它相同。現在這台機器上一直有使用者的背景負載，重量出來的 baseline 反而會比較不準。
- **先前看到的大量差異來自負載**：hayro agent 曾拿有負載時的 corpus run 和 baseline 比較，有 129–135 項超過 10%。那些差異來自量測時 35–80% 的 CPU 負載，不是程式退步。

## 方法

- **工具**：`tools/bench_paired.py`。對每個檔案，用兩個 build 交替執行 `fastpdf-bench full <file>`（A B、B A、…），每次都是新的 process，取每對比值（B／A）的中位數。負載的慢速漂移會在每一對之間抵消。
- **A（M1）**：`target/baseline` 的 release `fastpdf-bench`，產生 `baseline.json` 的 build（`a920ea8`）。
- **B（HEAD）**：`756dfe1` 的 `git archive` 匯出、release build。
- **語料與次數**：`fixtures/generated/manifest.json` 的 84 個檔案，每個檔案 3 對，engine Hayro（in-process）。
- **背景負載**：使用者的 WSL VM 等工作負載都在跑，量測期間沒有編譯。
- **判定規則**：
  - 比值超過 ±10%；
  - 指標本身超過門檻（時間 1 ms、記憶體 5 MB、`cpu_ms` 100 ms），兩邊的差也超過門檻（時間 1 ms、記憶體 2 MB、`cpu_ms` 31.25 ms）。
  - 低於門檻的抖動不予判定。原因是 1 ms 以下的量測每次會差 10–25%，而 Windows 的 CPU time 以 15.625 ms 為單位累計。

## 結果

狀態都是 ok 的 77 個檔案：

| 指標 | 檔案數 | 幾何平均（HEAD／M1） | 退步（判定） | 改善（判定） |
|---|---|---|---|---|
| `open_ms` | 77 | 0.979 | 0 | 0 |
| `first_page_ms` | 77 | 1.004 | 0 | 1 |
| `time_to_first_page_ms` | 77 | 1.001 | 0 | 1 |
| `text_ms` | 77 | 0.988 | 0 | 0 |
| `rss_peak_mb` | 77 | 0.999 | 0 | 0 |
| `private_peak_mb` | 77 | 1.000 | 0 | 0 |
| `cpu_ms` | 69 | 0.985 | 0 | 0 |

- **各類別**：「到第一頁的時間」的幾何平均落在 0.931（cad）到 1.043（transparency）之間，沒有任何類別的單一檔案超過 +10%。
- **低於門檻的變動**：沒有判定的 ±10% 變動全部低於門檻，例如 `text_ms` 0.13 → 0.19 ms、`open_ms` 0.14 → 0.18 ms，以及 `cpu_ms` 在 15.625 ms 與 31.25 ms 之間跳動。

## 怎麼用

```bash
python tools/bench_paired.py --a OLD/fastpdf-bench.exe --b NEW/fastpdf-bench.exe --pairs 3
```

- 兩個 binary 都要從各自 commit 的乾淨匯出 build，並複製到 target dir 之外，避免量測途中被重新 build 取代。
- exit code：沒有狀態變化、也沒有判定為退步時是 0。
- 這只比較 engine 層（in-process Hayro）。app 層的啟動、idle、捲動看 B-8（`docs/benchmarks/b8-app.md`），render host 看 `docs/benchmarks/render-host.md`。
