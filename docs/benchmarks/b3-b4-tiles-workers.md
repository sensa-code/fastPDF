# B-3 / B-4 — Tile Size × Render Workers

- 日期：2026-10-04；FastPDF `eb51a05`（release，從 `git archive` 匯出的乾淨目錄 build）；engine Hayro（`ced00dd0`）
- 機器：AMD Ryzen 9 9950X（16C/32T）、128 GB RAM；量測前 CPU 6.4%，沒有編譯中的 process
- 工具：`python tools/bench_tile_matrix.py --bench <fastpdf-bench.exe> --repeat 3`
- 量測：`fastpdf-bench render --tile T --workers W --viewport 1920x1080`。每個組合 3 次，每次都是全新 process（engine cache 是冷的），取中位數。指標是「頁面頂端 1920×1080 viewport 的所有 tile 都 render 完成」的時間（viewport fill）。
- 矩陣：6 個 fixture（small-text、繁中公文、30 萬線段地圖、A0 平面圖、20 頁灰階掃描、照片）× display scale 1／2／6 × tile 256／512／1024 × workers 1／2／4／6／8，共 270 個組合、810 次執行。

## 結果

viewport fill 時間的幾何平均，以 tile 512 + 4 workers 為 1.00（越低越好）：

| tile \ workers | 1 | 2 | 4 | 6 | 8 |
|---|---|---|---|---|---|
| 256 | 0.98 | 0.98 | 0.99 | 1.01 | 1.02 |
| 512 | 0.99 | 0.99 | 1.00 | 1.01 | 1.01 |
| 1024 | 0.98 | 0.97 | 0.98 | 0.99 | 1.00 |

各 fixture 中位數（ms）：

| display scale | tile 256（w1／w2／w4／w8） | tile 512（w1／w2／w4／w8） | tile 1024（w1／w2／w4／w8） |
|---|---|---|---|
| 1.0 | 19.9／19.8／20.1／20.3 | 19.5／19.6／19.7／20.0 | 19.6／19.7／19.8／20.2 |
| 2.0 | 28.6／28.8／29.0／30.1 | 28.7／28.8／29.5／29.6 | 27.6／27.9／28.1／28.3 |
| 6.0 | 23.1／23.2／23.5／24.0 | 23.1／23.5／23.5／23.9 | 23.2／22.8／23.1／23.4 |

peak RSS 中位數（MB）：tile 256 約 37–38、512 約 41–43、1024 約 44–49。

## 解讀

- **tile 大小與 worker 數對 viewport fill 幾乎沒有影響（±3% 以內）**。原因是 Hayro adapter 把可見 tile 合併成 2048 px block 一次 render（ADR 0003、`docs/audit/hayro.md`）：1920×1080 的 viewport 通常落在一兩個 block 內，多個 worker 只是在等同一個 block。
- worker 越多，延遲反而略增（1–3%），記憶體也較高。B-5 測到 Hayro 每條 render thread 在 CJK 或複雜向量頁上會保留 47–67 MiB 的 cache（`docs/benchmarks/b5-memory.md`）。
- tile 越大記憶體越高（邊緣 tile 浪費更多）；tile 越小，GPU atlas 項目與 upload 次數越多（GPUI audit）。

## 決定

- **Render workers 預設 2**（原本暫定 2–4）。依 spec §18「latency 優先，而不是 throughput 最大化」：2 個 worker 的延遲和更多 worker 相同，記憶體較少，同時保留一個 worker 給 prefetch 或第二個 block。
- **Tile size 維持 512**：時間與 256 相同，只多約 4 MB RSS，但 atlas 項目與上傳次數是 256 的四分之一。
- 限制：只量了冷啟動的第一個 viewport；快速捲動時的 prefetch 吞吐量（多個 block 並行）沒有納入，之後以 app 層 B-8 的捲動情境補量。

## 508 vs 512（2026-10-04，atlas 對齊）

- **背景**：tile 加上兩側各 2 px 的 gutter 後是 516 px，GPUI 最小的 1024×1024 atlas texture 只放得下 1 個。改成 508 px 後剛好 512 px，一張可以放 4 個。B-8 實測縮放後的 private bytes 少約 30 MB（`docs/benchmarks/b8-app.md`〈508 px tile 實驗〉）。
- **方法**：
  - 和本文相同的 6 個 fixture 與 3 種 scale（1、2、6），2 個 worker，viewport 1920×1080；
  - 每種組合 5 對，順序交替（508、512 ／ 512、508 …），每次都是新的 `fastpdf-bench render` process；
  - release build（HEAD `b9e9d59`），量測時沒有編譯在跑。
- **結果**：

| Fixture | ×1 | ×2 | ×6 |
|---|---|---|---|
| small-text／three-pages | 1.055 | 1.040 | 1.033 |
| traditional-chinese／gov-letter | 1.011 | 0.992 | 0.999 |
| vector-heavy／dense-polyline-map | 1.003 | 0.996 | 0.998 |
| cad／a0-floorplan | 0.991 | 1.009 | 1.014 |
| scanned／scan-gray-jpeg | 1.061 | 1.037 | 1.033 |
| image-heavy／photos-rgb-jpeg | 1.011 | 1.019 | 1.008 |

  表中是 508／512 的 viewport fill 時間比值，取每對比值的中位數，> 1 表示 508 較慢。
  - 幾何平均 1.017，範圍 0.991–1.061。
  - 比值最大的兩個（small-text、scanned ×1）絕對差距只有 0.3–1.2 ms。
  - 兩者的可見 tile 數相同（6 或 12 個），peak RSS 差距在 ±2 MB 內。
- **結論**：差距落在本文原本判定為雜訊的 ±3% 內，所以預設改為 508。
