# B-5：記憶體預算驗證（scroll benchmark）

對應 spec §15（Memory Budget）、§16（Memory Pressure）、§46（M6）。目的：用接近實際閱讀的操作驗證「大型 PDF 不爆 RAM、每個 cache 守住 budget」，並把 engine（Hayro adapter）內部的記憶體拆出來。

量測日期 2026-10-04；程式基準 `31828e9` 加上本次對 `crates/fastpdf-bench`、`crates/fastpdf-engine-hayro` 的修改（未 commit）。**只看記憶體，不看時間**（量測時機器上有其他 agent 在編譯；表中的秒數只供參考）。

## 結論

- **大型 PDF 不會爆 RAM。** 2000 頁的三種結構與 800 MiB 的 2000 頁影像檔，從第 1 頁捲到最後一頁、在 25/50/75% 處做 100%→400%→100% 縮放、再跳回第 1 頁：process private bytes 從第 100 頁左右起就穩定在 190–220 MiB（tile cache 128 MiB + engine 約 60–90 MiB），之後不隨頁數成長；OS 記錄的 private 峰值 224–240 MiB。關掉文件後回到 3–5 MiB，沒有洩漏。
- **每個 cache 都守住 budget。** 所有執行中 tile cache 都沒有超過 budget（128 MiB、16 MiB 皆然），Hayro 的 block cache 沒有超過 48 MiB，解碼後的 content stream 最多 3.4 MiB（budget 64 MiB）。小 budget（16 MiB）下 eviction 正常，跳回第 1 頁時 tile 會重新 render，不是空白。
- **Relief 有作用。** 把 soft/hard limit 調低後，每次 relief 釋放 tile 60–90 MiB、private 從約 196 MiB 降到 58–88 MiB，Hayro 依 Soft／Hard 清掉 block、執行緒 cache、重開 `Pdf`（17 次），畫面始終正確。預設 limit（320/512 MiB）下，一般檔案碰不到 soft limit。
- **本次在 Hayro adapter 修掉 5 個問題**（細節見〈發現與修正〉）：閒置執行緒留著已解碼的影像、memory trim 後執行緒數膨脹、每個執行緒的 hayro cache 閒置時不釋放、多執行緒同時解碼大影像造成瞬間峰值、text layer 太大（dense 頁 290 → 163 KB、CJK 頁 −45～66%，複製結果不變）。600 dpi 彩色掃描檔的 OS private 峰值從 578 MiB 降到 338 MiB；複雜向量檔與 CJK 檔關閉 session 後的 private 從 205–254 MiB 降到 7–9 MiB（engine 部分 0–3 MiB）。
- **仍有 4 個需要其他 crate 處理的問題**（見〈尚未解決〉）：mmap 檔案讓 working set 成長到檔案大小、tile budget 小於可見範圍時會無限重繪、relief 後 cache 立刻回填（鋸齒）、1 秒一次的 memory monitor 抓不到短暫峰值。另外 CJK 與複雜向量頁在**操作中**每個 render 執行緒仍會佔 47–67 MiB 的 hayro cache 或 vello buffer，這需要 hayro 端的 API（upstream issue 草稿 #9）。

## 方法

### `fastpdf-bench scroll`

新子命令（`crates/fastpdf-bench/src/scroll.rs`），headless、不需要 GPUI：

```text
fastpdf-bench scroll <file.pdf> [--viewport WxH] [--zoom PCT] [--step PX] [--tile-budget-mb N]
                     [--soft-limit-mb N] [--hard-limit-mb N] [--sample-every N] [--settle SECS] [--out report.json]
```

- 用 `fastpdf_core::DocumentSession<Arc<Pixmap>>` 開檔（與 app 相同的 `loader::load` → `open_guarded`；800 MiB 檔是 mmap），viewport 1920×1080、device scale 1.0、zoom 100%，tile 512 px、4 個 scheduler worker（機器預設）。
- 腳本：第 1 頁開始，每步 `scroll_by(0, 972)`（Page Down 的 90%）直到最後一頁；目前頁到達 25%、50%、75% 時各做一次縮放：以畫面中心 `zoom_in` 8 次（110%…400%）再 `zoom_out` 8 次回 100%；最後 `first_page()` 跳回開頭。
- 每一步呼叫 `frame()`，直到可見 tile 全部是精確解析度（`pending == 0`），用 session 的 wake hook 等待，逾時（預設 30 s）記為 unsettled。
- 記憶體管理照 app 接線（`fastpdf-ui/src/reader.rs`）：`MemoryBudgetManager::new(BudgetConfig)`、`register(session.tile_cache().budgeted())`、`MemoryMonitor::new` + `watch(&doc)`，每次 frame 都 `poll()`（monitor 自己限制每秒最多取樣一次）。
- 每 N 步（`--sample-every`，2000 頁檔用 20）加上每個縮放步、跳回、逾時步都記一筆時間序列：private bytes、working set、external（private − 已登錄 cache）、`EngineDocument::memory_usage()`、tile cache bytes／entries／evictions、scheduler 統計、pressure、Hayro 內部計數（block cache、content stream、render context 估計、decode budget、執行緒、trim 次數）。
- 結束時另外量三個點：**after idle**（停止操作 6 s，tile 還在）、**after close**（`session.close()` 釋放所有 tile，文件仍開著）、**after drop**（session 與文件都釋放）。
- 輸出 JSON（`--out`），stderr 印一行摘要。

### 檔案與設定

| 檔案 | 頁數 | 大小 | 內容 |
|---|---|---|---|
| `large-page-count/flat-2000p-reportlab.pdf` | 2000 | 1.3 MiB | 平坦 page tree、文字 |
| `large-page-count/balanced-tree-2000p.pdf` | 2000 | 0.7 MiB | 平衡 page tree |
| `large-page-count/objstm-xrefstream-2000p.pdf` | 2000 | 0.4 MiB | object stream + xref stream |
| `large-file/large-file-2000p.pdf` | 2000 | 800.8 MiB | 每頁一張獨立 JPEG（`generate.py --profile full`） |
| `scanned/scan-gray-jpeg-20p.pdf` | 20 | 17.6 MiB | 300 dpi 灰階掃描（2480×3508） |
| 補充：600 dpi 彩色掃描 | 6 | 10.9 MiB | 每頁一張 4960×7016 RGB JPEG（暫存檔，Pillow 產生） |
| 補充：`traditional-chinese/big5-level1-chars-ttfsubset.pdf` | 5 | 1.8 MiB | 5,401 個 Big5 常用字（約 22 個內嵌 TrueType subset） |
| 補充：`vector-heavy/bezier-curves-40k.pdf` | 4 | 1.3 MiB | 4 萬條細曲線 |

800 MiB 檔用 `uv run tools/fixtures/generate.py --profile full --only large-file --out <暫存目錄>` 產生（為了不覆寫共用的 `fixtures/generated/manifest.json`，輸出到暫存目錄），量測完已刪除。

預設：tile budget 128 MiB、soft/hard 320/512 MiB（`BudgetConfig::default()`）。機器：AMD Ryzen 9 9950X（32 threads）、125 GB RAM、Windows 11 Pro。

## 結果

### 1. 預設設定

記憶體單位 MiB。「private 最大」是時間序列取樣的最大值；「OS 峰值」是 OS 記錄的 `PeakPagefileUsage`（包含取樣之間的瞬間峰值）。「external」= private − tile cache，也就是 tile cache 以外（engine、allocator、程式本身）的部分。

| 檔案 | steps | unsettled | private 最大 | OS 峰值 | external 最大 | working set 最大 | tile 最大 | evictions | block cache 最大 | after idle | after close | after drop |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| flat 2000p | 2381 | 0 | 206.8 | 226.7 | 79.4 | 206.4 | 128.0 | 12,110 | 47.9 | 145.9 | 11.4 | 3.6 |
| balanced 2000p | 2331 | 0 | 218.1 | 223.9 | 91.0 | 203.2 | 128.0 | 11,934 | 48.0 | 145.3 | 9.5 | 3.4 |
| objstm 2000p | 2178 | 0 | 207.8 | 225.7 | 79.9 | 198.7 | 128.0 | 14,106 | 47.7 | 139.6 | 8.9 | 3.0 |
| large-file 800 MiB | 2381 | 0 | 213.6 | 239.6 | 85.8 | **998.0** | 128.0 | 12,132 | 47.9 | 147.0 | 13.5 | 5.4 |
| scan 300 dpi 20p | 73 | 0 | 250.2 | 295.2 | 122.3 | 243.3 | 127.9 | 328 | 47.9 | 153.1 | 21.5 | 2.7 |
| scan 600 dpi 6p | 56 | 0 | 297.2 | 338.2 | 169.4 | 199.7 | 128.0 | 297 | 47.8 | 126.9 | 15.3 | 4.9 |
| CJK 5p | 55 | 0 | 405.1 | 412.4 | 277.1 | 380.5 | 127.9 | 188 | 47.1 | 139.6 | 6.9 | 5.8 |
| vector 4p | 54 | 0 | 368.4 | 429.5 | 255.3 | 345.3 | 127.6 | 232 | 47.5 | 137.9 | 8.7 | 3.9 |

所有 run：tile cache 沒有超過 budget；block cache 沒有超過 48 MiB；跳回第 1 頁時 6 個可見 tile 全部重新 render 且有內容（非白像素 3,994–889,975 個）；after drop 時 Hayro 的 document、generation、執行緒、block 計數都歸零。

2000 頁三種結構的差異（page tree 形狀、object stream）對記憶體沒有可見影響；decoded content stream 全程只有 0.2–1.1 MiB（每頁內容很小），從未觸發 64 MiB 的 reopen。

### 2. 記憶體曲線（large-file 800 MiB，預設設定）

`#` = private，`+` = working set 多出 private 的部分，1 字元 = 25 MiB。

```text
 step  page  zoom  private       WS   tiles
    1     1  100%     17.2     28.7     3.4  +
  260   223  100%    190.0    290.5   127.9  #######++++
  520   446  100%    190.7    379.6   127.6  #######++++++++
  594   501  250%    200.5    395.3   127.5  ########+++++++
  720   604  100%    193.8    443.7   127.7  #######++++++++++
  980   827  100%    193.6    532.0   127.7  #######++++++++++++++
 1186  1001  150%    204.5    605.8   127.7  ########++++++++++++++++
 1440  1208  100%    200.3    684.7   127.7  ########+++++++++++++++++++
 1700  1431  100%    199.5    771.7   127.7  #######+++++++++++++++++++++++
 1789  1501  300%    213.6    812.2   127.8  ########++++++++++++++++++++++++
 1880  1571  100%    196.5    830.3   127.9  #######++++++++++++++++++++++++++
 2140  1794  100%    193.5    916.0   127.9  #######+++++++++++++++++++++++++++++
 2381     1  100%    208.7    998.0   127.9  ########+++++++++++++++++++++++++++++++
```

- private 在 tile cache 填滿（約第 100 頁）後就是一條平線：190–214 MiB，縮放時多 10–20 MiB。
- working set 線性成長到約 1 GB：多出來的約 800 MiB 是 mmap 的 PDF 檔頁面（hayro 直接從 mapping 讀 JPEG），屬於 file-backed、乾淨、可被 OS 隨時回收的記憶體，不算 commit。關閉 session 後仍有 817 MiB（文件還開著），drop 文件後回到 10 MiB。見〈尚未解決 1〉。

CJK 檔（1 字元 = 10 MiB）在 0.6 s 內就爬到 405 MiB，超過 soft limit（320 MiB）；這個 run 太短，monitor 只在開頭取樣一次，所以沒有 relief（實際 app 每秒會取樣，會觸發 Soft trim）：

```text
 step  page  zoom  private       WS   tiles
    1     1  100%     22.0     25.6     3.4  ##
    6     2  175%    153.9    151.6    36.6  ###############
   11     2  300%    222.6    215.1    94.3  ######################
   21     3  125%    273.9    262.6   106.6  ###########################
   25     3  250%    351.7    333.5   127.5  ###################################
   41     4  175%    378.7    358.9   127.9  #####################################
   55     1  100%    405.1    380.5   127.9  ########################################
```

external（private − tile）達 277 MiB：block cache 47 MiB，其餘主要是 3 個 render 執行緒各自的 hayro 字型／glyph outline cache（見〈engine 內部記憶體〉）。停止操作 6 s 後執行緒退出，private 回到 139.6 MiB（約等於 tile cache）。

### 3. 小 tile budget（`--tile-budget-mb 16`，`--settle 5`）

| 檔案 | private 最大 | OS 峰值 | tile 最大 | evictions | rendered tiles | unsettled | 跳回第 1 頁 |
|---|---|---|---|---|---|---|---|
| large-file 800 MiB | 104.1 | 117.3 | 16.0 | 12,916 | 12,948 | 0 | 重新 render 6 個 tile，有內容 |
| flat 2000p | 92.1 | 102.9 | 15.9 | 13,119 | 13,149 | 0 | 重新 render 6 個 tile，有內容 |
| scan 300 dpi | 121.0 | 180.7 | 16.0 | 6,917 | 6,945 | **4** | 重新 render 6 個 tile，有內容 |

- eviction 有作用：cache 一直維持在 16 MiB 以下，private 比預設少約 110 MiB（tile 差額）。
- 掃描檔在 300% 縮放的 4 個步驟（第 36、38、58、60 步）各等滿 5 s 仍未完成：那個位置的可見 tile 有 15–20 個（每個 516×516×4 ≈ 1.06 MB），合計超過 16 MiB，插入新的可見 tile 會把其他可見 tile 擠掉，scheduler 閒下來後又重排，**每 5 s 重繪 1,169–1,802 個 tile**，直到畫面移動。見〈尚未解決 2〉。
- 預設 128 MiB 下，1920×1080 的可見範圍最多約 21 MB，不會發生；4K 以上螢幕且 tile budget 很小時才會。

### 4. Memory pressure relief（調低 limit，tile budget 128 MiB，800 MiB 檔）

| 設定 | relief 次數 | 每次釋放的 tile | relief 前 → 後 private（平均） | Hayro 動作 | unsettled | 跳回 |
|---|---|---|---|---|---|---|
| soft 160 / hard 512 | Soft × 17 | 52.5–64.9 MiB | 196.0 → 87.7 | block 清空、執行緒 cache 丟棄 × 17 | 0 | 正常 |
| soft 128 / hard 192 | Hard × 17 | 86.4–94.2 MiB | 198.8 → 57.9 | 另外重開 `Pdf` × 17 | 0 | 正常 |

relief 每秒一次（monitor 間隔），每次都把 private 降到 limit 以下；但 tile budget（128 MiB）本身比 limit 留給它的空間大，下一秒又回填到 190–200 MiB，形成鋸齒。見〈尚未解決 3〉。

### 5. engine 內部記憶體（拆解）

Hayro adapter 現在實作 `EngineDocument::memory_usage()`：finished block（≤ 48 MiB）+ 目前 `Pdf` 已解碼的 content stream + render context 的估計（每執行緒「最大 target 像素 × 4 bytes」）+ 正在解碼的影像（decode budget 的估計值）。mmap 的檔案與系統字型是 file-backed、跨文件共用，不算在內（另外由 `diagnostics::memory().mapped_font_bytes` 回報，本次 1–3 MiB）。

| 項目 | 有 budget 嗎 | 實測 | 何時釋放 |
|---|---|---|---|
| block cache（相鄰 tile 共用的 2048 px block） | 48 MiB／文件 | 最大 47.1–48.0 MiB | Soft／Hard trim；所有執行緒閒置退出時（新） |
| 解碼後的 content stream（hayro 存在 `Pdf` 內） | 64 MiB 觸發 reopen | 最大 3.4 MiB | Hard trim（重開 `Pdf`）、超過 budget |
| render 執行緒的 hayro cache（字型、glyph outline）+ vello buffer | 無法量測（hayro 沒有 API） | 文字頁約 3 MiB／執行緒；CJK 頁 47–67 MiB／執行緒；4 萬曲線頁約 55 MiB／執行緒 | Soft／Hard trim、48 頁換新、閒置 5 s 退出（新） |
| 影像解碼的暫存（解碼樣本 + RGBA） | 256 MiB／process（新的 decode budget） | 估計：300 dpi 灰階頁約 44 MB／次，600 dpi 彩色頁約 240 MB／次 | render 結束（新：結束時立刻釋放 scene） |

`memory_usage()` 的最大值：文字類 80–97 MiB、800 MiB 檔 114 MiB、掃描檔 174–328 MiB。掃描檔的值包含解碼估計（全解析度樣本 + RGBA，是上限），比實測 external（122–169 MiB）高。CJK 與向量頁它只回報約 90–96 MiB，實際 external 達 255–277 MiB：差額就是上表第 3 列量不到的 hayro cache。也就是說這個數字適合當「engine 至少佔多少」的 overlay 指標，不適合當精確值。

### 6. Text layer（text cache 的項目大小）

UI 端用 dev overlay 量到 Hayro 的 `TextLayer` 在 dense-300p 每頁約 285 KB（zpdf 約 14 KB），32 MiB 的 text cache 只放得下約 114 頁。量測（每檔前 40 頁，`TextLayer::heap_bytes()`）顯示 dense 檔的 span 其實已經是整行（每頁 72 個、平均 130 字），體積來自每個字 16 bytes 的 `char_bounds` 加上 `Vec` 成長留下的空間；span 切得太碎的是 CJK 的 TrueType subset 檔（producer 把一行字分給好幾個 subset 字型，adapter 原本在換字型時斷開 span）。修正（`text.rs`）：

- 同一行、字級相同的 glyph 不再因為換字型而斷開；
- span 完成時把 `text`、`char_bounds` 縮到實際長度；
- 均分 span 外框就能得到相同矩形時（全形 CJK、等寬字）不存 `char_bounds`；selection 對沒有 `char_bounds` 的 span 本來就是均分（誤差上限 2% 字寬）。

| 檔案 | 改善前 heap／頁 | 改善後 heap／頁 | 改善後 bytes／字 | span／頁（前 → 後） |
|---|---|---|---|---|
| `large-text/dense-300p-times.pdf` | 290,344 B | 163,199 B（−44%） | 17.5 | 72 → 72 |
| `large-text/dense-300p-2col-helvetica.pdf` | 280,247 B | 179,101 B（−36%） | 18.0 | 155 → 155 |
| `traditional-chinese/gov-letter-embedded-ttfsubset.pdf` | 21,072 B | 7,546 B（−64%） | 16.0 | 103 → 39 |
| `traditional-chinese/gov-letter-embedded-type0.pdf` | 14,776 B | 7,546 B（−49%） | 16.0 | 39 → 39 |
| `traditional-chinese/gov-letter-cid-msung-nonembedded.pdf` | 14,768 B | 8,146 B（−45%） | 17.3 | 39 → 39 |
| `traditional-chinese/big5-level1-chars-ttfsubset.pdf` | 32,028 B | 10,927 B（−66%） | 8.8 | 79 → 73 |
| `traditional-chinese/big5-level1-chars-type0.pdf` | 30,934 B | 10,927 B（−65%） | 8.8 | 73 → 73 |

複製結果沒有退步：上面 7 個檔共 96 頁，`fastpdf_core::selection::selected_text(select_all)` 的輸出改善前後逐字相同（空白、換行都一樣）。新增的測試：`text.rs` 的單元測試（均分的 span 丟掉 `char_bounds` 後 `hit_test` 與複製結果不變、比例字型保留、直排保留、同一行跨字型合併）與 `tests/text_layers.rs`（三個 fixture 的 bytes／字上限與複製文字片段）。

32 MiB 的 text cache 現在可以放約 205 頁 dense 文字（原本約 115 頁）。要再縮小需要改 engine-api：比例字型的每字矩形無法用均分代替，而 `PageRect` 的 y 值在橫排 span 內都相同；若 `TextSpan` 改存「沿 baseline 的 n+1 個 x 邊界」（每字 4 bytes），dense 頁約可降到 50 KB。這是 `fastpdf-engine-api` 的 API 變更，交給指揮官決定。

回應 UI 端「大量操作後 private 到 284 MiB」的追查：以 Hayro 來說，engine 自己的部分在操作中最多約 block 48 MiB + 每執行緒 3–67 MiB（4 個 worker）+ 解碼暫存；停止操作 5 s 後執行緒退出、block 清空，只剩 content stream（< 4 MiB）。若 284 MiB 是在 CJK 或複雜向量文件上、操作剛結束時量到，大部分可能就是這些執行緒 cache；修正後停止操作 6 s 量測時，engine 部分應接近 0（tile 與 text cache 不變）。

## 發現與修正（Hayro adapter，已修）

1. **閒置執行緒留著上一頁的已解碼影像。** vello 的 `RenderContext` 把影像 paint（`Arc<Pixmap>`）留在 scene 裡，直到下一次 `reset`；adapter 原本只在下一個 job 開始時 reset，閒置執行緒就一直持有。600 dpi 彩色頁每張約 140 MB RGBA。修正：render 完成（以及中途取消）後立刻 `ctx.reset()`（`render.rs`）。只加這個修正時，OS private 峰值：600 dpi 檔 578 → 506 MiB、300 dpi 檔 328 → 296 MiB。
2. **memory trim 後執行緒數膨脹到上限。** trim 會喚醒所有閒置執行緒重設 cache，這段時間它們不算「閒置」，新 job 進來就多開執行緒；Soft relief 17 次後 pool 從 3 個長到 8 個（每個都有自己的 cache）。修正：改成計算「忙碌中」的執行緒，只有 job 數多於非忙碌執行緒時才開新的，而且執行緒在回覆前就標記為不忙碌（`pool.rs`）。修正後同樣情境維持 3 個。
3. **每個執行緒的 hayro cache 閒置時不釋放。** 實驗（每次 render 後丟掉不同部分，量 session 關閉後剩下的 private）：CJK 頁每執行緒 47–67 MiB，只丟 `RenderContext` 不變，連 `RenderCache`/`InterpreterCache` 一起丟才回到 0.2 MiB（字型與 glyph outline）；4 萬曲線頁每執行緒約 55 MiB，丟 `RenderContext` 後回到 5.5 MiB（vello 的 scene buffer 保留峰值容量）。原本執行緒要閒置 30 s 才退出。修正：閒置 5 s 退出，最後一個執行緒退出時清空 block cache（`pool.rs`）。session 關閉後的 private（執行緒閒置不到 30 s 時）：向量檔 254 → 9 MiB、CJK 檔 205 → 7 MiB；文件開著、閒置 6 s 時 engine 部分 = 0–3 MiB。代價：停止 5 s 後的第一次 render 要重建 cache（重新解析字型，數 ms）。
4. **多執行緒同時解碼大影像，瞬間峰值超過 hard limit。** hayro 每次 `render_into` 都以全解析度重新解碼頁面上的影像並轉成 RGBA（沒有影像 cache）；600 dpi 彩色頁一次約 240 MB，3–4 個執行緒同時解碼就超過 512 MiB，而且是 monitor 看不到的瞬間值。修正：靜態掃描時估計每頁最大影像的解碼量（`scan.rs`），render 前向 process 級的 **decode budget（256 MiB）** 申請，放不下就等（`decode.rs`）；需求小於 16 MiB 的頁面不受影響。600 dpi 檔 OS 峰值 506 → 338 MiB（等待 39 次）；300 dpi 檔（每次約 44 MB）沒有等待，不受影響。
5. **Text layer 太大**（見〈結果 6〉）：跨字型合併同一行、縮減容量、均分可重建時不存 `char_bounds`；dense 頁 −36～44%、CJK 頁 −45～66%，複製結果不變。

另外為了 B-5 新增：`EngineDocument::memory_usage()` 的實作、`diagnostics::memory()`（block、content、render context、decode、mapped font、文件／generation／執行緒計數、trim 次數），以及對應的測試。

## 尚未解決（需要其他 crate，建議）

1. **mmap 讓 working set 成長到檔案大小**（`fastpdf-core` loader）。800 MiB 檔看完整份後 working set 約 1 GB，其中約 800 MiB 是 mapping 的頁面。它們可被回收、不算 commit，工作管理員預設的「記憶體」欄（private working set）也不包含，但「working set」欄與 `rss_peak_mb` 會包含。建議：`MappedFile` 在 memory pressure（或文件閒置）時對整個 mapping 呼叫 `VirtualUnlock`，把頁面移出 working set（留在 OS 的 standby cache，再讀是 soft fault）；並在 overlay 區分 private 與 working set。
2. **tile budget 小於可見範圍時無限重繪**（`fastpdf-render` TileCache／`fastpdf-core` session）。`SharedCache` 的 `protected` 只保護 relief，不保護一般 budget 淘汰。建議：插入時不淘汰本 frame 標記為可見的 tile（必要時暫時超出 budget），或 session 把有效 budget 夾在「可見 bytes × 1.5」以上，或 budget 不足時停止 prefetch。
3. **relief 後立刻回填（鋸齒）**（`fastpdf-cache`）。relief 只 `shrink_to`，cache 的 budget 不變，下一秒又長回去。建議：Hard（或連續 Soft）之後暫時把 cache budget 降到 relief 後的大小，壓力回到 Normal 一段時間後再逐步恢復。
4. **monitor 每秒取樣一次，抓不到瞬間峰值。** OS 峰值比取樣最大值高 7–61 MiB（文字檔約 20 MiB、掃描與向量檔 41–61 MiB），CJK 檔在 0.6 s 內超過 soft limit 時 monitor 還沒取樣。decode budget 已經擋住最大的一塊；剩下的建議在 tile 到達時（app 已經有的 hook）提高大頁面附近的取樣頻率，或對 `PeakPagefileUsage` 的變化做反應。
5. **hayro 端**：影像沒有跨 `render_into` 的解碼 cache、`RenderCache` 沒有大小上限且每執行緒一份。已寫成 upstream issue 草稿 #9（`docs/upstream-issues/hayro.md`）。

## 重現

```text
cargo build --release -p fastpdf-bench
target/release/fastpdf-bench scroll fixtures/generated/large-page-count/flat-2000p-reportlab.pdf --sample-every 20 --out flat.json
target/release/fastpdf-bench scroll <large-file-2000p.pdf> --sample-every 20 --tile-budget-mb 16 --settle 5 --out large-16mb.json
target/release/fastpdf-bench scroll <large-file-2000p.pdf> --sample-every 20 --soft-limit-mb 128 --hard-limit-mb 192 --out large-hard.json
```

`--engine zpdf`（以 `--features engine-zpdf` build）也能跑，只是沒有 Hayro 的內部計數；`memory_usage()` 對沒有實作的 engine 為 `None`。原始 JSON 沒有放進 repo（每檔 40–150 KB），用上面的指令可以重新產生。
