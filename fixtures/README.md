# FastPDF 測試／Benchmark PDF Corpus（`fixtures/`）

對應 `docs/SPEC.md` §27（Test Corpus），並服務 §19（GPU／CPU 各類 workload）、§24（Error Handling）、§25（Security guardrails）、§26／§41（`fastpdf-bench` baseline）。

所有 PDF 都由 `tools/fixtures/generate.py` **以程式合成**，不從網路下載任何 PDF 或字型；內容（文字、公文、檢驗報告、影像）全部為虛構，不含真實個資。

## 目錄結構

```text
fixtures/
├── README.md              ← 本文件（commit）
├── manifest.quick.json    ← quick profile 的 manifest（commit，用來追蹤輸出是否 deterministic）
├── generated/             ← 產生器輸出（.gitignore，不 commit）
│   ├── manifest.json
│   └── <category>/*.pdf
└── local/                 ← 使用者自備的真實世界 PDF（.gitignore，不 commit）

tools/fixtures/
├── generate.py            ← 產生器入口（PEP 723 inline metadata）
├── generate.py.lock       ← `uv lock --script` 產生的完整依賴鎖定檔
├── verify.py / .lock      ← 用 pypdf 驗證 manifest（sha256、頁數、密碼）
└── fxlib/                 ← 產生器模組（rawpdf writer、painter、各分類）
```

## 產生方式

需要 [uv](https://docs.astral.sh/uv/)（依賴會安裝到 uv 的隔離環境，不影響系統 Python）。在 repo 根目錄執行：

```powershell
# quick profile（預設）：除 large-file 外的全部分類
uv run tools/fixtures/generate.py

# full profile：quick + large-file（預設目標 800 MiB、2000 頁）
uv run tools/fixtures/generate.py --profile full
uv run tools/fixtures/generate.py --profile full --large-file-mb 200

# 產生後順便驗證
uv run tools/fixtures/generate.py --verify

# 單獨驗證（sha256、pypdf 開檔、頁數、密碼；malformed 只做資訊性 probe）
uv run tools/fixtures/verify.py
```

| 參數 | 說明 |
| --- | --- |
| `--profile quick\|full` | quick：84 個檔案、約 62 MB；full：再加 `large-file/` |
| `--out DIR` | 輸出目錄，預設 `fixtures/generated` |
| `--large-file-mb N` | large-file 目標大小（MiB），只在 full 生效，預設 800 |
| `--jobs N` | 平行 worker 數，預設 CPU 數 |
| `--only CAT,...` | 只產生指定分類（開發用；不會更新 `manifest.quick.json`） |
| `--verify` | 產生後執行 `verify.py` 的檢查 |

參考數據（Windows 11、32 threads）：quick profile 約 7 秒；verify 約 2 秒；large-file 以串流方式寫檔，800 MiB 的影像池校準約 2.5 秒，其餘時間主要是磁碟寫入。

規則：
- 只有 `--profile quick`、未指定 `--only`、輸出到預設目錄時，才會同步寫出 `fixtures/manifest.quick.json`。
- quick profile 不會動到既有的 `generated/large-file/`，但該檔不會列在 quick 的 `manifest.json` 中。
- 每個檔案先寫成 `*.partial` 再 rename，其他程序不會讀到寫到一半的檔案；同一分類中不再產生的舊檔會被移除。
- 依賴系統字型的 fixture 若找不到字型會被略過，列在 `manifest.json` 的 `skipped`。

## 分類

| 分類 | 檔案 | 主要測試點 |
| --- | --- | --- |
| `small-text` | 1 頁 Helvetica、3 頁 platypus（Times，含表格、outline、連結）、2 頁 Courier Letter | 最基本的開檔與首頁渲染 baseline |
| `large-text` | 300 頁單欄 Times 9pt、300 頁雙欄 Helvetica 8pt | 大量 `Tj`、文字抽取順序、outline |
| `scanned` | 20 頁 300 dpi 灰階 JPEG（雜訊、傾斜、模糊、灰塵、掃描器陰影）；5 頁同類掃描＋隱形 OCR 文字層（`Tr 3`）；10 頁 1-bit CCITT G4 | 每頁一張大圖的 decode、可搜尋掃描檔、CCITT |
| `image-heavy` | 6 張 3000x2000 照片 JPEG；800 張小縮圖；Flate＋PNG predictor／SMask／16-bit／Indexed 8 與 4-bit／ImageMask／color-key mask／`/Decode`／`/Interpolate`／ICCBased；24 MP JPEG、Adobe CMYK、progressive、灰階、4:4:4、restart markers | 影像 decode 路徑與記憶體 |
| `vector-heavy` | 4 萬條 bezier；2000 個自交多邊形（nonzero／even-odd）＋漸層＋線型；shading type 1–7＋tiling／shading pattern；約 30 萬線段的地圖 polyline | 向量 rasterizer、shading |
| `cad` | A0 平面圖（6 個 OCG 圖層、hatch、hairline、尺寸標註）；A1 電路圖（Form XObject 重複引用約 1,400 次）；A0 有限元素網格（5.6 萬個填色三角形＋8.4 萬條 hairline） | 超大頁面、數萬條細線、tile／zoom |
| `fonts` | standard-14 全部＋字碼表＋MacRoman／`/Differences`；嵌入 TrueType subset（Bitstream Vera）；嵌入 Type 1（callig15）；Type3（d0／d1／dvips 風格點陣字）；文字狀態運算子 | 字型程式與編碼 |
| `cjk` | 非嵌入 CID 字型：MSung-Light（繁中）、STSong-Light（簡中）、HeiseiMin-W3／HeiseiKakuGo-W5（日）、HYSMyeongJo-Medium／HYGothic-Medium（韓）；直排（`-V` CMap）；50 頁密集繁中；reportlab 舊版 CMap 不一致案例 | **非嵌入 CJK 字型**的系統字型替代 |
| `japanese` | 非嵌入 Heisei 字型（橫排＋直排）；嵌入 TrueType subset（Yu Gothic）；嵌入 Type0／Identity-H（MS Gothic） | 日文字型與直排 |
| `traditional-chinese` | 模擬公文「函」與醫院檢驗報告，各有 TrueType subset、Type0／Identity-H、非嵌入 MSung-Light 三種版本；掃描版公文（200 dpi 彩色 JPEG＋紅色印章、300 dpi CCITT G4）；Big5 第一字面 5401 字（TrueType subset 與 Type0 各一） | 台灣使用情境（見下節） |
| `transparency` | 常數 alpha 重疊、3000 個半透明矩形、半透明文字、RGBA 影像；16 種 blend mode；luminosity／alpha soft mask、isolated／knockout group、巢狀 group、page group | 透明度合成 |
| `encrypted` | AES-256 R6／R5、AES-128、RC4-128、RC4-40（空 user password＋owner password，限制權限）；AES-256 與 AES-128 需 user password；中文 user password | Standard security handler |
| `malformed` | 29 個損壞或惡意檔案（見下表） | §24／§25 guardrails |
| `large-page-count` | 2000 頁扁平 page tree（reportlab）；2000 頁平衡 page tree＋屬性繼承；2000 頁 object stream＋xref stream＋混合頁面尺寸 | 開檔時間、頁面索引、page tree 走訪 |
| `large-file` | （僅 full）約 2000 頁、每頁一張獨立 JPEG XObject，總大小約 `--large-file-mb` | §11：2000 頁／800 MB 也要立即可用 |

### 台灣使用情境（`traditional-chinese/`）

- `gov-letter-*`：模擬「虛構市政府環境保護局 函」，包含檔號、保存年限、發文機關、受文者、發文日期（民國紀年）、發文字號、速別、主旨、說明（一、二、三…懸掛縮排）、附表、正本／副本、署名、附件表格。
- `lab-report-*`：模擬「測試綜合醫院（虛構）檢驗醫學部」臨床檢驗報告：病人資料表、CBC／生化／血脂／尿液／甲狀腺，含數值、單位、參考範圍、H／L／A 註記（紅字）。
- 每頁都印有「【測試樣本】本文件內容全部為虛構」字樣；機關、人名、地址、電話、字號、病歷號碼與檢驗數值均為合成。
- 同一份文件有多種字型結構，方便比較：
  - `*-embedded-ttfsubset.pdf`：reportlab 的 simple TrueType subset（每個 subset ≤256 字，字多時會產生多個字型物件）。
  - `*-embedded-type0.pdf`：Type0＋CIDFontType2＋`Identity-H`＋ToUnicode，這是 Word、瀏覽器列印等常見輸出的結構（以 fontTools subset，保留原 GID）。
  - `*-cid-msung-nonembedded.pdf`：非嵌入 MSung-Light（`UniCNS-UCS2-H`），reader 必須自行找系統字型替代。
- 字型偏好：公文優先標楷體（`kaiu.ttf`），檢驗報告 TrueType 版優先微軟正黑體（`msjh.ttc`），Type0 版與 Big5 字表優先新細明體（`mingliu.ttc` 的 PMingLiU）；找不到就依序改用其他繁中字型，全都找不到則略過並記錄在 `skipped`。實際使用的字型記錄在每個檔案的 `inputs.system_font`。
- 注意：FreeType 把標楷體（DFKai-SB）與細明體系列列為 *tricky font*，字形可能依賴 TrueType hinting 指令組合筆畫；rasterizer 若忽略 hinting，可能畫出錯位的筆畫。這組 fixture 正好可以用來檢查。

### malformed 一覽

| 檔案 | expected | 測試的 guardrail |
| --- | --- | --- |
| `truncated-at-60pct` | render_partial_ok | 檔案截斷（後半物件、xref、trailer 遺失） |
| `truncated-before-xref` | recover_expected | 無 xref／trailer，需重建 xref |
| `bad-xref-offsets` | recover_expected | xref offset 全部偏移 |
| `startxref-beyond-eof` | recover_expected | startxref 指向檔尾之後 |
| `xref-table-garbled` | recover_expected | xref 表格式錯亂 |
| `xref-prev-cycle` | recover_expected | `/Prev` 鏈循環（無窮迴圈） |
| `no-header`、`leading-junk-before-header` | recover_expected | 缺 header、header 前有垃圾 |
| `garbage-after-eof-small`／`-64k` | open_ok／recover_expected | `%%EOF` 後有垃圾 |
| `no-catalog`、`not-a-pdf-random-bytes`、`empty-file` | open_error_expected | 無法開啟時要乾淨地回報錯誤 |
| `missing-object` | render_partial_ok | 參照不存在的物件 |
| `corrupt-flate-stream` | render_partial_ok | 損壞或截斷的 Flate stream |
| `wrong-stream-length` | recover_expected | `/Length` 太小、太大、指向不存在物件、超過 64-bit、負值 |
| `page-tree-kids-cycle`、`page-parent-cycle` | render_partial_ok | page tree 循環、屬性繼承循環 |
| `page-count-lie` | recover_expected | `/Count` 2147483647（不可依此預先配置） |
| `indirect-reference-cycle` | guardrail_expected | 間接參照循環（max recursion） |
| `deep-nesting-array-5000`、`deep-nesting-dict-5000` | guardrail_expected | 5000 層巢狀（max nesting） |
| `deep-nesting-content-100000` | guardrail_expected | content stream 10 萬層 array、10 萬層 `q` |
| `huge-mediabox` | guardrail_expected | 1e7x1e7 pt、零面積、反向、整數溢位、`/UserUnit` 75000 |
| `huge-image-declared` | guardrail_expected | 宣告 100000x100000、寬 2^31-1、JPEG SOF 65535x65535、300000x300000 mask |
| `decompression-bomb-512mib` | guardrail_expected | 約 0.5 MB 的 Flate 展開為 512 MiB |
| `decompression-bomb-nested-flate` | guardrail_expected | 958 bytes 的雙層 Flate 展開為 512 MiB |
| `broken-embedded-fonts` | render_partial_ok | 隨機資料或截斷的字型程式、壞掉的 `W` 與 ToUnicode |
| `content-syntax-errors` | render_partial_ok | 未知運算子、operand 不足、未配對 BT/ET、q/Q 等 |

bomb 在產生時是逐 MiB 串流壓縮，不會在記憶體中展開。

## manifest 格式

`generated/manifest.json`（以及 commit 的 `manifest.quick.json`）頂層欄位：`schema`、`generator`（版本、seed）、`profile`、`options`、`environment`（Python、zlib 與各套件版本）、`system_fonts`（找到的字型檔名、face index、sha256）、`expected_values`、`summary`、`skipped`、`files`。

`files[]` 每筆：

| 欄位 | 說明 |
| --- | --- |
| `path` | 相對於 `generated/` 的路徑（`/` 分隔） |
| `category` | 分類 |
| `bytes`、`sha256` | 檔案大小與 SHA-256 |
| `pages` | 頁數；malformed 若頁數沒有明確定義則為 `null` |
| `encrypted`、`encryption` | 是否加密；加密參數（`algorithm`、`V`、`R`、`key_bits`、`user_password`、`owner_password`、`permissions`） |
| `password` | 開檔用的 user password（未加密為 `null`，owner-only 檔為空字串），方便 `fastpdf-bench` 等工具直接讀取 |
| `expected` | 預期行為（見下） |
| `description` | 說明（繁體中文） |
| `guardrails` | 對應的 §24／§25 項目 |
| `features` | 用到的 PDF 功能標籤（方便 benchmark 分組） |
| `producer` | `reportlab`、`rawpdf`（自寫 writer）或 `pypdf` |
| `inputs`（選用） | 用到的系統字型（檔名、face index、family、sha256） |
| `stats`（選用） | 例如線段數、bomb 展開後大小 |

`expected` 的值：

| 值 | 意義 |
| --- | --- |
| `open_ok` | 合法檔案，必須能開啟並渲染每一頁 |
| `password_required` | 需要 user password（見 `encryption.user_password`），空密碼必須被拒絕 |
| `recover_expected` | 已損壞但可修復（重建 xref、修正 `/Length` 等）；成熟的 reader 能開，FastPDF 若無法修復也必須乾淨地失敗 |
| `render_partial_ok` | 可開啟；部分頁面或物件損壞可略過，其餘必須渲染；不可 crash |
| `guardrail_expected` | 惡意輸入；應觸發 §25 的資源上限，同時維持記憶體與時間預算，其餘內容正常 |
| `open_error_expected` | 應以乾淨的錯誤拒絕（不可 panic、不可卡住） |

`verify.py` 會檢查所有檔案的 sha256，並用 pypdf 開啟非 malformed 檔案、比對頁數與密碼行為。malformed 檔案只在子程序中用 pypdf probe，結果僅供參考：pypdf 是參照實作，不是判準。

## Deterministic

- 每個 fixture 使用獨立的 RNG：`random.Random(f"20261004:{key}")`（字串 seed 經 SHA-512，不受 `PYTHONHASHSEED` 影響）。
- reportlab 使用 `invariant=1`（固定 CreationDate 與 `/ID`）；自寫 writer 的日期固定為 `D:20260101000000Z`，`/ID` 由 fixture key 的 MD5 決定。
- 影像只用 Pillow 合成，雜訊來自 seeded `randbytes`，不使用 C 層級的亂數。
- pypdf 加密時用到的 salt、IV、file key 原本取自 `secrets.token_bytes`；產生期間會暫時換成 seeded 版本，所以加密檔也能重現（僅限 fixture 用途）。
- fontTools subset 關閉 timestamp 重算；manifest 不含時間戳記與絕對路徑。
- 依賴：`generate.py` 的 PEP 723 metadata 釘選 `reportlab==5.0.1`、`pypdf==6.19.0`、`pillow==12.3.0`、`fonttools==4.66.1`、`cryptography==50.0.2`，並設 `exclude-newer`；完整的 transitive 依賴鎖定在 `generate.py.lock`。
- 已驗證：連跑兩次（不同 `PYTHONHASHSEED`）以及輸出到不同目錄，所有檔案的 sha256 都相同。
- 會影響 sha256 的外部因素：(1) 套件版本，包括 Pillow wheel 內的 libjpeg-turbo、Python 的 zlib；(2) 會嵌入或點陣化系統字型的 fixture，例如 Windows 更新後字型檔變了。`system_fonts[].sha256` 與 `files[].inputs` 可用來追查差異。

檢查方式：重新產生 quick profile 後執行 `git diff fixtures/manifest.quick.json`，應該沒有任何差異。

## 授權與 commit 政策

- **`generated/` 不 commit**：
  1. 體積大（quick 約 62 MB，full 再加 800 MB 以上），而且可以從程式完整重現。
  2. 部分 fixture 嵌入了系統字型的 subset，或用系統字型點陣化：微軟正黑體、新細明體、標楷體、Yu Gothic、MS Gothic（Microsoft、DynaComware 等廠商的商業字型）。這些字型的 OS/2 `fsType` 是 editable embedding，允許嵌入文件；但 repo 屬於公開散布的情境，為了避免授權疑慮，專案政策是不 commit 任何含系統字型 subset 或其點陣化結果的檔案。
- 產生器只在本機產生時**讀取**系統字型（`C:\Windows\Fonts`、`%LOCALAPPDATA%\Microsoft\Windows\Fonts`），字型本身不會複製到 repo；`fsType` 禁止嵌入或 subset 的字型會被自動略過。含系統字型 subset 的 PDF 建議只在本機使用，不要公開散布或附在 issue 上。
- reportlab 隨附的字型：Bitstream Vera（Bitstream Vera License，可自由散布）、callig15（作者聲明可自由使用與散布）。用於 `fonts/` 分類，與系統字型無關。
- `fixtures/manifest.quick.json` 只含檔名、雜湊、描述等 metadata，可以 commit。
- 產生器依賴（只用在開發工具，不會隨 FastPDF 發佈）：reportlab（BSD）、pypdf（BSD-3-Clause）、Pillow（MIT-CMU／HPND）、fontTools（MIT）、cryptography（Apache-2.0 或 BSD-3-Clause）；皆為 permissive 授權，符合 SPEC §37。
- 內容全為合成：英文段落由固定字表隨機組成，中日韓文為自行撰寫的短句，公文與醫療報告中的機關、人物、號碼、數值皆為虛構。

## `fixtures/local/`：真實世界 PDF（只在本機）

合成的 fixture 無法涵蓋所有真實世界的怪異情況，所以保留 `fixtures/local/` 讓開發者放自己的檔案。這個目錄已列在 `.gitignore`，**永遠不 commit**。適合放：

- 政府機關公開文件（公文、公報、法規、招標文件、掃描歸檔的 CCITT／JBIG2 檔）
- 醫療報告、保險文件（若是真實個資，請只放在自己的機器，不要分享、不要上傳到 CI 或 issue）
- 大型掃描書籍、CAD 匯出圖、設計稿、簡報匯出的 PDF
- 曾經讓其他 reader 當機或很慢的檔案

建議依照上面的分類建立子目錄，例如 `fixtures/local/traditional-chinese/`、`fixtures/local/cad/`。需要記錄預期行為或密碼時，可以自行放一份 `fixtures/local/manifest.local.json`，欄位沿用上面的 `files[]` 格式（至少 `path`、`category`、`expected`，視需要加 `pages`、`encryption`）。`fastpdf-bench` 之後應能同時接受多個 corpus 根目錄（`fixtures/generated`、`fixtures/local`、upstream 測試檔）。

## 把 `upstream/hayro` 的測試 PDF 當成本機 corpus

`upstream/` 是 audit 用的上游 clone，已列在 `.gitignore`。hayro 的測試檔位於：

```text
upstream/hayro/hayro-tests/
├── manifest_custom.json / manifest_pdfjs.json / manifest_pdfbox.json / manifest_pdfium.json / manifest_corpus.json
└── pdfs/
    ├── custom/   ← 渲染案例（約 280 個 PDF）
    ├── load/     ← 「載入不可 crash」的回歸檔（約 85 個，多數來自 fuzz；另有 .jb2／.jp2 原始檔）
    └── other/
```

使用原則：**只引用、不複製、不 commit**。

- 直接以路徑引用，例如之後的 `cargo run -p fastpdf-bench -- upstream/hayro/hayro-tests/pdfs/load/<file>.pdf`，或讓 bench 掃描整個目錄的 `*.pdf`。
- 想讓它出現在 `fixtures/local/` 底下時，用 junction 或 symlink，不要複製：

  ```powershell
  New-Item -ItemType Junction -Path fixtures\local\hayro -Target upstream\hayro\hayro-tests\pdfs
  ```

- 預期行為建議：`load/` 視為 malformed（`render_partial_ok`，重點是不 crash、不卡住）；`custom/` 視為 `open_ok`，manifest 中的 `first_page`／`last_page` 可用來限定渲染頁面。
- `hayro-tests/sync.py` 會從 `hayro-assets.dev` 下載更多 corpus（pdf.js、PDFBox、PDFium issue tracker 與大型公開 corpus）。這些檔案來自第三方，授權各自不同。要不要下載由開發者自行決定；下載後放在 `upstream/` 底下，同樣不 commit。本產生器沒有執行這個下載。

## 已知限制

- 沒有產生 JBIG2 與 JPEG 2000（JPX）影像，因為工具鏈中沒有 permissive 授權的 encoder。請改用 hayro `load/` 中的相關檔案或 `fixtures/local/`。
- 尚未涵蓋 AcroForm／XFA 表單、連結以外的 annotation、數位簽章、Tagged PDF、linearized（fast web view）檔案，以及 `xref-prev-cycle` 以外的一般 incremental update。
- Adobe CMYK JPEG 的顏色在不同 reader 之間慣例不一，該 fixture 主要測解碼路徑，不保證顏色。
- 非嵌入 CJK 字型的顯示取決於 reader 的系統字型替代。例如在本機用 PDFium 測試時，非嵌入的韓文字型畫不出韓文字（文字層本身是完整的）。
- reportlab 的 TrueType 嵌入一律是 simple font subset（≤256 字）；Type0／Identity-H 版本由本專案的 writer 以 fontTools subset 產生，並保留原 GID（loca／hmtx 維持完整長度）。
- reportlab 5.0.1 把 `MSung-Light` 對應到 `UniGB-UCS2-H`（字集不一致）。產生器已修正為 `UniCNS-UCS2-H`，另外保留一份舊行為的檔案 `cjk/reportlab-msung-unigb-cmap-mismatch.pdf` 作為真實世界案例。reportlab 5 也沒有 `MHei-Medium`。
- large-file 的影像來自 40 張不同 JPEG 組成的影像池；每頁仍是獨立的 XObject 與獨立的位元組。
