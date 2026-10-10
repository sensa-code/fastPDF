# Hayro 升級到 `14410d75`（0.8.0 版本線，2026-10-11）

對應：`crates/fastpdf-engine-hayro/Cargo.toml`、`docs/audit/hayro.md`〈升級紀錄〉、`docs/upstream-issues/hayro.md` #1。

## 結論

- **升級內容**：從 `ced00dd0`（2026-10-03）升到 `14410d75`（2026-10-10）。
  - FastPDF 的程式碼不需修改：0.8.0 的版本提升（#1390）只改了版本號。
  - `Cargo.lock` 中 hayro 系列 9 個 crate 換到新 rev。
- **corpus**（84 檔、185 頁）：
  - 開檔狀態 0 變化，包含 29 個 malformed 檔。
  - 184 頁的 Hayro 輸出與舊版逐位元組相同。
  - 唯一改變的是 `transparency/softmask-groups.pdf` 第 1 頁：
    - alpha soft mask 從不透明的方塊變成正確的半透明圓（#1394，FastPDF 送的 PR）；
    - 與 zpdf 的差異從 10.884% 降到 2.868%；
    - 剩下的差異是 knockout group，對應 #1395，尚未 merge。
- **JPEG 2000**：
  - corpus 沒有 JPX 檔，所以另外用 Pillow（OpenJPEG）產生 5 種編碼變體做冒煙測試。
  - 新舊兩版都與 OpenJPEG 的解碼一致：無損變體逐像素相同，有損變體最大差 1。
  - 這次的 JPX 修正（POC、RGN、subsampled component 的 precinct）這些變體都沒有涵蓋，只能依靠 upstream 的測試。
- **效能**（paired B-1）：
  - 3 對、84 檔：各項 geomean 都在 ±4% 內，狀態 0 變化。
  - 初篩標出 4 個超過 10% 的檔案，用 10 對重測：
    - 其中 3 個回到 1.0x（雜訊）；
    - `softmask-groups.pdf` 首頁 6.55 → 7.63 ms（+1.1 ms，x1.19），原因是現在真的要計算 soft mask，屬於正確性修正的代價。
- **為什麼仍 pin git rev**：crates.io 的 0.8.0 於 2026-10-04 從 `ea9c81dc` 發布，沒有 #1394 與 JPX 修正。等有包含 #1394 的 release 再改用 crates.io。

## 環境與方法

- **環境**：Windows 11 Pro、Ryzen 9 9950X（32 個邏輯 CPU）、rustc 1.99.0、release profile、`--features engine-zpdf`。
- **兩個版本**：
  - 舊：FastPDF `236ba99`（Hayro `ced00dd0`），從乾淨的 clone build。
  - 新：同一個 commit，只改 Hayro 的 pin（`14410d75`）與 `Cargo.lock`。
  - 量測前先把兩個 `fastpdf-bench.exe` 複製到 target 以外的位置，避免重建時被換掉。
- **像素比對**：
  - 兩版各跑一次 `fastpdf-bench diff-corpus fixtures/generated/manifest.json --engine hayro,zpdf --images DIR`（tolerance 16）。
  - 新舊 Hayro 的 PNG 先比 SHA-256，不同的再算像素差。
- **效能**：
  - `python tools/bench_paired.py --pairs 3`；
  - 超過 10% 的檔案再用 `--pairs 10` 重測。
- **JPX 冒煙測試**：
  - 用 Pillow 12 產生 384×256 影像的 5 種變體：
    - 無損、LRCP、6 層解析度；
    - 有損、3 個品質層、RPCL；
    - 128×128 tile、PCRL；
    - 64×64 precinct 加 32×32 code-block、RLCP；
    - 灰階、CPRL。
  - PDF 頁面大小設成 1 個影像像素等於 1 個裝置像素，用 `fastpdf-bench diff` render。
  - 產生與比對用的腳本放在 session 的暫存資料夾，沒有放進 repo。

## 結果

### corpus 像素比對

| 比對 | 結果 |
|---|---|
| 開檔狀態（Hayro 端，84 檔） | 0 變化 |
| 新舊 Hayro 的 PNG（185 頁） | 184 頁逐位元組相同；1 頁不同（`softmask-groups` p1，最大差 230，8.025% 像素超過 16） |
| 與 zpdf 的差異（每頁 differing %） | 191 筆相同、1 筆變好（`softmask-groups` p1：10.884% → 2.868%）、0 筆變差 |

`softmask-groups` p1 的三張圖並排（舊 Hayro／新 Hayro／zpdf）：右上角的「Alpha soft mask (group ca 0.4)」在舊版是不透明的藍色方塊，新版與 zpdf 都是半透明的圓。中下兩組「knockout」三版仍不同，新舊 Hayro 都把重疊處混色，zpdf 只留後畫的圓，這部分等 #1395。

### JPX 冒煙測試

| 變體 | 新舊 Hayro | Hayro vs OpenJPEG | zpdf vs OpenJPEG |
|---|---|---|---|
| 無損 LRCP | 相同 | 最大差 0 | 最大差 0 |
| 有損 3 品質層 RPCL | 相同 | 最大差 1 | 最大差 2 |
| 128×128 tile PCRL | 相同 | 最大差 0 | 最大差 0 |
| precinct＋code-block RLCP | 相同 | 最大差 0 | 最大差 0 |
| 灰階 CPRL | 相同 | 最大差 0 | 最大差 0 |

zpdf 也使用 `hayro-jpeg2000`（crates.io 0.4.0），所以這一欄不是獨立的參考。

### 效能（paired B-1，B = 新版）

3 對，84 檔（77 檔兩版都成功）：

| metric | files | geomean B/A | > +10% | < -10% |
|---|---|---|---|---|
| open_ms | 77 | 1.009 | 0 | 0 |
| first_page_ms | 77 | 0.987 | 2 | 4 |
| time_to_first_page_ms | 77 | 0.991 | 2 | 4 |
| text_ms | 77 | 1.005 | 1 | 0 |
| rss_peak_mb | 77 | 1.002 | 0 | 0 |
| private_peak_mb | 77 | 0.999 | 0 | 0 |
| cpu_ms | 67 | 0.965 | 1 | 1 |

初篩超過 10% 的 4 個檔案，用 10 對重測：

| 檔案 | 初篩（3 對） | 重測（10 對） |
|---|---|---|
| `transparency/softmask-groups.pdf` first_page_ms | 5.63 → 6.99（x1.19） | 6.55 → 7.63（x1.19） |
| `encrypted/aes256-r5-owner-only.pdf` first_page_ms | 6.24 → 7.25（x1.17） | 4.78 → 4.68（x1.001，比值取每對的中位數） |
| `cjk/cid-nonembedded-vertical.pdf` text_ms | 1.86 → 2.88（x1.59） | 1.45 → 1.46（x0.993） |
| `malformed/deep-nesting-content-100000.pdf` cpu_ms | 234 → 281（x1.12，15.6 ms 刻度） | 234 → 234（x1.000） |

## 升級包含的 upstream commit

| commit | 內容 |
|---|---|
| `ea9c81dc` | Version bump 0.8.0（#1390，crates.io 0.8.0 即此 commit） |
| `9016f891` | hayro-interpret：group 沒有 color space 時保留 soft mask（#1394，FastPDF 送的 PR；只 merge alpha 那一半） |
| `3e714c53` | hayro-jpeg2000：progression iterator 的自訂邊界（#1397） |
| `f94de026` | hayro-jpeg2000：支援 progression order change（POC，#1398） |
| `dc9b72b6` | hayro-jpeg2000：支援 region of interest（RGN，#1399） |
| `14410d75` | hayro-jpeg2000：subsampled component 的 precinct 位置（#1400） |

## 後續

- #1395（knockout group）merge 後再升級一次，`softmask-groups` 剩下的 2.868% 應該會消失。
- 有包含 #1394 的 crates.io release 之後，改用 crates.io。
- `benchmarks/baseline.json` 是用 `ced00dd0` 量的；之後跑 `compare` 時，`softmask-groups` 的首頁會多約 1 ms（+19%），這是預期中的。
