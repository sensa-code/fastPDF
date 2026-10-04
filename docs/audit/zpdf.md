# zpdf Audit（M0）

- 對象：`D:\fastPDF\upstream\zpdf`（https://github.com/Xero-Team/zpdf），commit `fe0ed23`（`v0.14.0-1-gfe0ed23`，2026-09-28），MIT
- 範圍：SPEC §1、§3、§4、§6、§11–§19、§24–§25、§36–§37、§43–§44、§49 Q4–Q7
- 量測環境：Windows 11 Pro 10.0.26200、Ryzen 9 9950X（16C/32T）、128 GB RAM、RTX 5090（wgpu 自動選到 Vulkan backend）、rustc/cargo 1.99.0、release build
- 量測性質：**全部是粗略單次量測**，量測期間其他子 agent 同時在 build（CPU 競爭，誤差可能到 ±30%）。M1 的 `fastpdf-bench` 必須重測。
- 證據：程式碼位置寫成 `crate/src/file.rs:行號`（相對 `crates/`）；量測原始輸出、harness 原始碼、測試 PDF 產生器都在 scratchpad `agent-zpdf\`（不屬於 repo）。
- Studio Memory：`context_build(project=FastPDF)` 回傳 `insufficient_evidence`，目前沒有已核准的專案記憶，本文只依據 repo 與實測。
- Upstream 的 `CLAUDE.md`、`AI_POLICY.md`、`zpdf-skill` 內含給 AI 的指示，本 audit 只把它們當成資料記錄（見〈Maturity & Maintenance〉），沒有照著執行。

---

## Summary

**結論：zpdf 是功能面很廣、對惡意輸入相當耐打的純 Rust PDF 解析與 CPU rendering library。但以「互動式 Reader engine」的標準來看還不成熟。它可以當 FastPDF 的 primary engine 候選進入 M3 PoC，前提是由 `fastpdf-engine-zpdf` adapter 補齊缺口，並以 pin 住的 fork／vendor 方式依賴，不追 upstream。GPU backend 不適合 tile-based viewport rendering。**

關鍵發現（每項都附證據，細節見後面各章）：

1. **沒有高階 API**：render 一頁要由呼叫端串 8 個步驟（page → fonts → content bytes → annotations／OC／output intent → `ContentInterpreter` → `DisplayList` → backend）。CLI、GPUI viewer、wasm 各自重寫一份（`zpdf-cli/src/main.rs:1431-1597`、`zpdf-viewer-gpui/src/document.rs:135-281`）。
2. **開檔不 lazy 到底**：整個檔案必須進記憶體（`impl Into<Arc<[u8]>>`，沒有 path、mmap 或 streaming）。傳入 `Vec<u8>` 時轉成 `Arc<[u8]>` 會複製一次：530 MB 檔案 open 時 peak private 達 **1015 MB**。xref 全表一次解析。`Catalog::from_trailer` 會走完整棵 page tree，並 resolve 每個 page 物件（`zpdf-document/src/catalog.rs:20-71`）。實測 20,000 頁 open 65 ms、開檔後約 68 MB。
3. **Display list 可快取、可重播、可跨執行緒共用**：`DisplayList`、`FontCache`、`ImageCache` 都是 `Send + Sync`（實測 probe）。`PageRenderInfo.page_rect` 可以是任意子矩形，所以 **CPU tile rendering 不用改 upstream 就能做**，tile 和整頁裁切比對在 AA 等級內一致。但 display list 沒有 bounding box／spatial index，每個 tile 都要重播全部 command：CAD 頁面（62k commands）全部 tile 循序跑是整頁的 7–10 倍。**我在 adapter 側用公開欄位做 bbox culling 的原型，5 份文件的所有 tile 都和不做 culling 的結果 bit-identical**，CAD 全 tile 從 2087 ms 降到 237 ms。
4. **Threading**：`PdfDocument`、`PdfFile` 是 `Send`，不是 `Sync`。物件快取是 `RefCell`，upstream 自己註明「never shared across threads」（`zpdf-parser/src/lib.rs:44-47`），整個 workspace 內部完全沒有 threading。可行的模型是「單一 document 擁有者負責 parse／interpret，多個 worker 共用 `DisplayList` 平行 raster tile」（實測 16 執行緒，CAD 全 tile 21 ms）。
5. **沒有取消機制**：沒有 cancel token、progress callback 或可設定的 interpret 時間預算（固定 8 s，`zpdf-content/src/interpreter.rs:199`）。唯一的取消點是呼叫端自己跑 `begin_page/execute/end_page`，在 command 之間檢查。單一 command 最長實測：向量 0.07–2.7 ms，整頁掃描影像 48 ms（150 dpi）或 242 ms（576 dpi）。interpret 階段的影像解碼不能中斷（A4 300 dpi JPEG 每頁 53–60 ms）。
6. **快取只有「准入上限」，沒有淘汰**：預設值（object 512 MiB、image 1 GiB、font 256 MiB、soft mask 512 MiB）遠高於 SPEC §15。README 宣稱的「font cache LRU（256-font limit）」**與程式碼不符**：`FontCache` 的容量只是預先配置的提示，不會淘汰（`zpdf-font/src/lib.rs:1833-1873`）。decoded image 以 display list 為單位、保留全解析度，跨頁共用的同一張圖每頁都重新解碼（實測 3 頁 × 60 ms）。
7. **正確性**：發現一個實際 bug。`/Rotate` 90/180/270 搭配非零原點的 MediaBox/CropBox 時，內容會位移並被截掉（實測 26.9% 面積空白），所有內建呼叫端（CLI、GPUI viewer、winit viewer）都受影響。workaround 已驗證：`with_page_rotation` 後再呼叫 `with_content_translation(-x0, -y0)`。另外：整頁 raster 超過 64 MP 會**默默降低 scale**；shading 在 interpret 時以 ≤2048 px 固定解析度烘成影像；文字位置只到 span 等級。
8. **GPU backend（Q5）**：每個 process 自建 headless wgpu device，沒有 API 可以注入外部 device。每次 render 都重新配置 target texture、重新上傳**所有**頁內影像、重建 per-page glyph atlas。路徑 tessellation 和字形 raster 在 CPU 做，而且不做 culling。要拿到像素只能 readback，submit-only 模式直接丟掉像素。GPUI 在 Windows 是 D3D11，`surface()` 只支援 macOS，所以一定要「GPU→CPU→GPUI 再上傳」。實測 9 份文件中有 8 份 GPU tile 比 CPU tile 慢 1.4–23 倍（掃描頁 22.9 vs 2.5 ms，CJK 155 vs 6.6 ms；唯一例外是只有一張 200×200 pt 小圖的頁面）。context 初始化 0.34–0.74 s，GPU 路徑的 process private memory 為 125–650 MB（只用 CPU 時約 10–155 MB）。用 upstream 自己的 `zpdf compare` 比對，CPU 與 GPU 像素差異在密集 CJK 頁達 20.3%（README 宣稱 <1%）。
9. **健壯性**：zpdf repo 內 496 份惡意或損壞 PDF（各大 PDF 專案 bug tracker 收集來的）和 hayro 的 365 份測試 PDF 都跑過一次：**0 panic、0 timeout**，最長 8.6 s（觸發預算後中止）。但 `catch_unwind` 只包住 JPX decoder，fuzz 只涵蓋 parser 層。
10. **成熟度**：專案 2026-05-26 才建立，約 4 個月，總共 247 commits，主要作者佔 219 筆（bus factor ≈ 1）。約 12 萬行 Rust，+144,664 行。有 34 筆 commit 帶 Claude co-author trailer。crates.io 3.5 個月內發了 12 版，總下載約 1.4k。issue 只有 2 筆。9 月 commit 數明顯下降。

---

## Architecture

### Crate 依賴圖（normal dependency，內部 crate）

```text
zpdf-core  (ObjectId, PdfObject, Rect/Matrix(f64), Error, ParseLimits)            920 LOC
 ├─ zpdf-color        → core                       (+moxcms ICC)                 1,889
 ├─ zpdf-display-list → core                       (+smallvec)                     355
 ├─ zpdf-font         → core                       (+ttf-parser; CJK 表 ~16k 行) 24,359
 ├─ zpdf-parser       → core, color                (+flate2, zune-jpeg, aes/cbc/sha2) 11,751
 ├─ zpdf-image        → core, color                (+zune-jpeg, hayro-jpeg2000)   2,418
 ├─ zpdf-render       → core, display-list         (RenderBackend trait)            620
 ├─ zpdf-document     → core, parser, font         (+sha1/sha2, rsa, p256, p384) 17,435
 ├─ zpdf-content      → core, parser, document, display-list, font, image, color  11,627
 ├─ zpdf-render-cpu   → core, content, display-list, font, image, render (+tiny-skia, image) 4,313
 ├─ zpdf-render-wgpu  → core, display-list, render, font, image, content (+wgpu 30, lyon, tiny-skia, pollster) 7,976
 ├─ zpdf-writer       → core, parser, document, content (+rsa, p256, aes, getrandom, [ureq opt]) 15,230
 ├─ zpdf (facade)     → 上述全部 + writer；cpu-render(預設)/gpu-render 為 optional  6,034（多為 tests）
 ├─ zpdf-svg-export / zpdf-pptx-export（DisplayList → SVG / PPTX）
 ├─ zpdf-cli（bin `zpdf`）、zpdf-wasm、zpdf-benches
 └─ zpdf-viewer-gpui（publish=false；GPUI git rev d989c7c5）
```

（LOC 是該 crate 底下所有 `.rs` 檔的行數，含 tests。workspace 共 19 個 member，`fuzz/` 已 exclude。合計約 119.5k 行。）

注意 upstream `CLAUDE.md` 宣稱「render backends never depend on the parser」。實際上 `zpdf-render-cpu` 和 `zpdf-render-wgpu` 都依賴 `zpdf-content`，而 `zpdf-content` 又依賴 parser 與 document（見各 `Cargo.toml`）。原因是 Type3 字型會在 render 時對**每個字形實例**跑一次 `ContentInterpreter`（`zpdf-render-cpu/src/lib.rs:1953-1981`）。

### 各 crate 責任

| Crate | 責任 | 與 FastPDF 的關係 |
|---|---|---|
| zpdf-parser | lexer、xref（含 `/Prev`、`/XRefStm`、lazy repair scan）、object/ObjStm、filters（Flate/LZW/A85/AHx/RL/DCT/CCITT/JBIG2）、RC4/AES 解密 | 必要 |
| zpdf-document | catalog、page tree（含屬性繼承）、`PdfPage`、字型載入、annotation、outline、destination、page labels、metadata、forms、簽章驗證、PDF/A／UA 檢查 | 必要（含用不到的簽章 crypto） |
| zpdf-content | content stream tokenizer 與 interpreter，產生 `DisplayList`；影像解碼、shading raster、text span、search、table 偵測 | 必要 |
| zpdf-display-list | 扁平的 `Vec<RenderCommand>`，用 Push/Pop 表達 clip 與 blend group | tile 快取的核心 |
| zpdf-render-cpu | tiny-skia backend | 必要 |
| zpdf-render-wgpu | wgpu backend（lyon tessellation + glyph atlas + MSAA 4x + readback） | 評估後不建議（見 Q5） |
| zpdf-writer / pptx / svg / wasm / cli | 編輯、轉檔、工具 | Reader 不需要，但 facade 會無條件依賴 writer |

### 資料流

```text
bytes（整個檔案；Vec<u8> → Arc<[u8]>）
  └─ PdfFile::parse_with_password_and_limits      zpdf-parser/src/lib.rs:79-144
       find_startxref → parse_xref_and_trailer（整條 /Prev 鏈，全部 entry 進 HashMap）  xref.rs:62-91
       失敗或 /Root 無效 → recovery::scan_all_objects（全檔掃描）                        lib.rs:91-120
       resolve(ObjectId) → PdfObject（lazy，object_cache / objstm_cache 准入預算）      lib.rs:207-283
  └─ PdfDocument::open → Catalog::from_trailer（走完整棵 page tree）                  zpdf-document/src/lib.rs:124-138, catalog.rs:20-71
       page(i) → PdfPage{media_box, crop_box, rotate, resources, contents, annots}    page.rs:43-116
       load_page_fonts(&page) → FontCache（文件層 SharedFonts 共用已解析字型）          lib.rs:202-204
       page_content_bytes(&page) → Vec<u8>（所有 content stream 解碼後串接）           lib.rs:162-194
  └─ ContentInterpreter::new(effective_box).with_page_rotation().with_fonts().with_document()
       .with_images(&mut ImageCache).with_colors().with_annotations()…interpret(&bytes)
       → DisplayList{page_rect, commands}；影像在這一步以全解析度解碼成 premultiplied RGBA
         並放進 ImageCache；shading 在這一步烘成影像                                    interpreter.rs:524-948
  └─ RenderBackend::begin_page(PageRenderInfo{page_rect, scale, background})
       / execute(cmd)* / end_page()                                                   zpdf-render/src/lib.rs:241-263
       ├─ CpuRenderer（tiny-skia）→ RenderedPage{width, height, data: Vec<u8>}       zpdf-render-cpu/src/lib.rs:2415-2654
       └─ WgpuRenderer → GpuTexture（readback 後的 CPU bytes）或 Submission（不含像素） zpdf-render-wgpu/src/lib.rs:271-517
```

---

## API Mapping to PdfEngine

（SPEC §6 的 trait。「狀態」欄：✅ 可直接用／⚠️ 可用但有缺口／❌ 沒有）

| PdfEngine 需求 | zpdf 對應 API（簽章，位置） | 狀態 | 缺口／備註 |
|---|---|---|---|
| `open(DocumentSource)` | `PdfDocument::open(data: impl Into<Arc<[u8]>>) -> Result<Self>`（`zpdf-document/src/lib.rs:109`）；`open_with_password(data, &[u8])`（:120）、`open_with_limits(data, ParseLimits)`（:113）、`open_with_password_and_limits`（:124） | ⚠️ | 沒有 from path／mmap／`Read+Seek` streaming，必須整檔進記憶體。傳 `Vec<u8>` 會多複製一次（實測 530 MB 檔 peak 1015 MB）。adapter 應自己 `fs::read` 進 `Arc<[u8]>`（例如 `Arc::new_uninit_slice` + `read_exact`）以避免 2× peak |
| 加密 | 同上的 password 版本；`is_encrypted()`（:141），錯誤 `Error::WrongPassword` | ✅ | RC4、AES-128、AES-256（R5/R6） |
| `page_count` | `PdfDocument::page_count(&self) -> usize`（:145） | ✅ | 開檔時已走完整棵 page tree（不是讀 `/Count`） |
| `page_size` | `PdfDocument::page(&self, usize) -> Result<PdfPage>`（:149）→ `media_box`、`crop_box`、`rotate`（`page.rs:43-56`）、`effective_box()`（`page.rs:182`） | ⚠️ | 沒有只取尺寸的輕量 API，`page()` 會順便解析 resources 並 clone page dict。`width()/height()` 是 **MediaBox** 尺寸（`page.rs:170-176`），不是 CropBox，也沒考慮旋轉。`rotate` 是原始 i32，要自己正規化並交換寬高（wasm 版的寫法：`zpdf-wasm/src/lib.rs:95-104`）。不支援 `/UserUnit`。實測：2000 頁全部取尺寸 7 ms，20000 頁 91 ms |
| `render`（整頁） | 沒有單一函式。流程是 `load_page_fonts`、`page_content_bytes`、`ImageCache::new`、`page_annotations`、`oc_config`、`output_intent_cmyk_profile`，再 `ContentInterpreter::new(rect).with_page_rotation(rotate)….interpret(&bytes) -> DisplayList`（`interpreter.rs:524,746,869`），最後 `CpuRenderer::new().with_limits().with_fonts(&fc).with_images(&ic).render_display_list(&dl, scale)`（`zpdf-render-cpu/src/lib.rs:429-525`；trait 預設實作 `zpdf-render/src/lib.rs:249-263`） | ⚠️ | 呼叫端必須自己組裝，容易漏步驟（GPUI viewer 就沒有使用 `with_optional_content`）。必須避開 rotation bug（見〈Robustness & Security〉） |
| 任意 transform／scale | `PageRenderInfo { page_rect: Rect, scale: f32, background: Color }`（`zpdf-render/src/lib.rs:7-11`） | ⚠️ | 只支援等比 `scale` 加平移（page_rect 原點），不支援任意 affine、非等比縮放或視圖旋轉。旋轉只能在 interpret 時用 `with_page_rotation` 烘進去 |
| sub-rectangle／tile | 呼叫端自己 `begin_page(&PageRenderInfo{page_rect: tile_rect, scale, …})`，接著 `execute(cmd)*`、`end_page()` | ✅（實測可用） | `PageRenderInfo` **沒有從 facade re-export**，要直接依賴 `zpdf-render`。沒有 culling，每個 tile 要重播全部 command（見〈Rendering Model〉） |
| render 到呼叫端 buffer | 無 | ❌ | `begin_page` 每次都 `tiny_skia::Pixmap::new(w, h)`（`zpdf-render-cpu/src/lib.rs:2511`），`end_page` 回傳新的 `Vec<u8>`（:2630-2653，零複製交出）。每個 tile 都會配置一次 |
| pixel format | CPU：tiny-skia **premultiplied RGBA8**，左上原點，stride = w×4（`lib.rs:2633`）；背景預設不透明白（`zpdf-render/src/lib.rs:254-258`）。GPU：`Rgba8Unorm`（**linear，刻意不用 sRGB**，`zpdf-render-wgpu/src/context.rs:12-15`），顏色預乘（`transform.rs:81-96`） | ✅ | 交給 GPUI 前要轉成 BGRA（viewer 做法：`zpdf-viewer-gpui/src/document.rs:284-293`）。透明背景時要注意 premultiplied |
| display list 快取／重播 | `DisplayList { page_rect, commands: Vec<RenderCommand> }`（`zpdf-display-list/src/lib.rs:3-20`），`Clone + Send + Sync`，以 `FontId`／`ImageId` 參照外部 `FontCache`／`ImageCache` | ✅ | 不是完全與解析度無關：shading 已烘成 ≤2048 px 影像（`interpreter.rs:2645-2653`）。沒有 per-command bbox。`RenderCommand` 136 bytes、`PathElement` 56 bytes（座標是 f64，issue #16） |
| `extract_text` | `ContentInterpreter::with_text_sink(&mut Vec<TextSpan>)`（`interpreter.rs:795`）；`TextSpan { text, x, y, size, advance, mcid }`（`zpdf-content/src/text.rs:12-28`）；`spans_to_text(spans, line_tol)`（text.rs:49，XY-cut 閱讀順序）；`struct_ordered_text`（text.rs:444） | ⚠️ | 需要一次完整 interpret，可以和 render 共用同一趟。實測直排中文會變成一字一行。部分 LaTeX 行的空白推論失敗（例如 `Thequickbrownfox…`） |
| 文字選取／搜尋高亮 | `search_spans(&[TextSpan], &str, bool) -> Vec<SearchHit>`（`zpdf-content/src/search.rs:73`），`SearchHit { line, start, len, rects }`（:17-28） | ⚠️ | 位置只到 **span 等級**（baseline 原點加水平 advance）。沒有 per-glyph box、字高或旋轉資訊，每個字元的 x 範圍是內插出來的（search.rs:1-7、49-61），高亮框只是近似，不能跨行。GlyphRun 裡雖然有 per-glyph 位置，但沒有 Unicode 對應 |
| `outline` | `PdfDocument::outline(&self) -> Vec<OutlineItem>`（lib.rs:301）；`OutlineItem { title, dest: Option<Destination>, uri, open, children }`（`outline.rs:27-40`） | ✅ | 一次解析完，上限 65,536 項 |
| `links` | `PdfDocument::page_annotations(&self, &PdfPage) -> Vec<Annotation>`（lib.rs:210）；`Annotation { subtype, rect, flags, appearance, dest: Option<Destination>, uri, … }`（`annotation.rs:23`）；`Destination { page: Option<usize>, page_ref, view: DestView }`（`destinations.rs:82-93`） | ⚠️ | 沒有 QuadPoints。第一次呼叫會解析整個 AcroForm，並攤平 named destination（lib.rs:223-234） |
| metadata | `info() -> Option<DocInfo>`（lib.rs:324）、`xmp_metadata()`（:344）、`version()`（:157） | ✅ | |
| page labels | `page_labels() -> Option<PageLabels>`（lib.rs:334）；`PageLabels::label(usize) -> Option<String>`（`page_labels.rs:104`） | ✅ | |
| 取消／時間預算／progress | 無 cancel 或 progress API。interpret 固定 8 s（`interpreter.rs:199`，沒有公開 setter）。CPU render 可設 `with_render_budget(Option<Duration>)`（`zpdf-render-cpu/src/lib.rs:502`，預設 8 s，超時後回傳**部分結果**而不是錯誤） | ❌ | 只能由 adapter 自己跑 execute 迴圈，在 command 之間檢查取消（見〈Rendering Model〉）。interpret（含影像解碼）無法中斷 |
| thumbnail | 無 `/Thumb` API；用低 scale render 代替 | ⚠️ | 影像仍會以全解析度解碼 |
| 錯誤模型 | `zpdf_core::Error`（`zpdf-core/src/error.rs`），renderer 另有 `CpuRenderError`、`WgpuRenderError` | ✅ | 超時或超預算時常常是「默默截斷並回傳成功」，只寫 `tracing::warn`，呼叫端看不到 |

---

## Lazy Loading

**開檔時就做（eager）：**

| 動作 | 證據 | 成本 |
|---|---|---|
| 整個檔案進記憶體 | `PdfFile.data: Arc<[u8]>`（`zpdf-parser/src/lib.rs:35`）；CLI 和 viewer 都用 `std::fs::read` | 530 MB 檔：read 137–190 ms，steady private 516 MB；`Vec` 轉 `Arc` 的瞬間 **peak 1015 MB** |
| 由檔尾 `rposition` 找 `startxref` | `xref.rs:389-419` | 一般情況很便宜 |
| 解析整個 xref 進 `HashMap`（含整條 `/Prev` 與 `/XRefStm`） | `xref.rs:62-91` | O(物件數) |
| xref 壞掉時全檔掃描重建 | `lib.rs:91-120`、`recovery.rs` | O(檔案大小) |
| 驗證 `/Root`，並建立 decryptor | `lib.rs:139-142, 722-751` | 小 |
| **走完整棵 page tree，resolve 每個 page leaf**（快取住）；找不到時退回「全檔掃 `/Type /Page`」 | `catalog.rs:20-71, 111-201` | O(頁數)；page dict 若在 ObjStm 內，會在開檔時解碼該 ObjStm |

**按需（lazy）：** 物件 resolve 與快取、ObjStm 解碼、content stream 解碼、字型解析（頁面層級，文件層 `SharedFonts` 共用）、影像解碼（interpret 時）、AcroForm 和 named destination（第一次查詢時）、outline、structure tree、系統字型索引（第一次碰到非內嵌字型時掃 `%WINDIR%\Fonts`，`zpdf-font/src/system.rs:302-356`）。

**實測（粗略單次，`harness open`）：**

| 檔案 | 檔案大小 | open | 全部頁面尺寸 | 開檔後 private | 第一頁 prepare + render 96 dpi |
|---|---|---|---|---|---|
| syn 2,000 頁（文字，非內嵌 Helvetica） | 0.57 MB | 5.75 ms | 7.1 ms | 8.6 MB | 12.9 ms（主要是首次建立系統字型索引並載入替代字型）+ 0.7 ms |
| syn 20,000 頁 | 5.9 MB | 65.4 ms | 91 ms | 68 MB | 13.0 + 0.8 ms |
| syn 2,000 頁 + 每頁 260 KB 影像 | 530 MB | 104–139 ms（另有 read 137–190 ms） | 8–15 ms | 516 MB（peak 1015 MB） | 13.6–28.9 + 3–4 ms |

結論：對 SPEC §11 的情境（2000 頁、800 MB）而言，**parse 不是瓶頸**，開檔後第一頁可以在約 0.3 s 內畫出來。真正的問題是「整檔 I/O + 整檔常駐記憶體 + 瞬間 2×」，以及 page tree O(頁數) 的全走訪。後者在 2 萬頁以內還可以接受。mmap 或 streaming 需要改 `PdfFile` 的資料抽象（fork）。

---

## Rendering Model

### CPU（tiny-skia）

- 每頁 `begin_page` 配置新的 `Pixmap`，並填背景（`lib.rs:2436-2527`）。路徑以 device space 建 tiny-skia path，fill／stroke 時帶目前的 clip mask。超出 64× pixmap 尺寸的 path 會跳過，以免 tiny-skia panic（`lib.rs:537-563`）。
- **字形沒有任何快取**：每個字形實例都會 `ttf_parser::Face::parse` 加 `outline_glyph` 再 `fill_path`（`zpdf-font/src/lib.rs:572-603`、`zpdf-render-cpu/src/lib.rs:1825-1878`）。upstream 自己量過，outline 擷取約佔字形時間的 12%，raster 才是主要成本（`docs/performance/PERFORMANCE.md` §13.3）。
- 影像：bilinear 放大；縮小 0.5× 以下時用 box filter，並做 per-page 快取（上限 64 MiB，`lib.rs:347`）。
- clip 是整張 raster 大小的 mask，有像素工作量和位元組預算（`MAX_CLIP_PIXEL_WORK` 2 Gpx、`MAX_CLIP_MASK_BYTES` 128 MiB，`lib.rs:341-345`）。超過預算的 clip 會**直接略過**，結果比正確畫面多畫一些內容。
- blend group／soft mask／knockout／overprint 都有實作，各有記憶體預算（`MAX_BLEND_SURFACE_BYTES` 512 MiB，`lib.rs:348`）。
- **64 MP 上限會默默改 scale**：整頁超過 `max_page_pixels` 時，會把 scale 等比縮到放得下為止，只有 `warn!`（`lib.rs:2479-2509`）。實測 coat of arms 在 576 dpi 時 scale 8.0 被改成 7.9282，A0 CAD 在 scale 4 時被改成約 2.82。呼叫端必須檢查回傳的尺寸。

### GPU（wgpu）

- `GpuContext::new_headless()`（`context.rs:130-305`）自建 `wgpu::Instance::default()` 和 HighPerformance adapter，`required_limits` 是 downlevel 加上最大 texture 尺寸，MSAA 4x，並 hook `on_uncaptured_error` 以免 panic。**沒有公開 constructor 可以包外部 device**：`device_error`、`identity` 是私有欄位，`with_context` 只吃 `GpuContext`（`lib.rs:203-214`）。
- 每頁流程：`execute` 時在 CPU 上做 lyon tessellation（device-pixel 空間，tolerance 0.1 px，沒有 culling，`path.rs:16-21`）。字形在 CPU 上用 tiny-skia raster 進 **per-page** 2048² R8 atlas（`glyph_atlas.rs:1-37`），旋轉、斜體或 atlas 滿了的字形就退回向量 fill。`finish_page` 時**每頁新建** target、stencil 和 readback buffer（`lib.rs:614-620`），並**每頁重新上傳所有被參照的影像**（`lib.rs:694-702`）。
- 輸出：`render_display_list` 用 `map_and_strip` 搭配 `poll(wait_indefinitely)` 同步 readback（`target.rs:213-256`）。`render_display_list_submitted` 不 readback，但「the pixels are dropped when the page ends」（`lib.rs:252-290`），**沒有任何 API 能取得 `wgpu::Texture`**。
- upstream 自己的 winit viewer 也是：headless device 先 readback，再 `queue.write_texture` 上傳到 surface device（`zpdf-render-wgpu/examples/simple-viewer.rs:65-132`）。

### Tile 可行性（CPU）

實測（`harness bench/tiles`，512×512 tile）。「tile vs crop」是同一 scale 下 tile 與整頁 render 裁切的比對：

| 文件 | 整頁 288 dpi | 單 tile 288 dpi | 單 tile 576 dpi | tile vs crop（>8/255 的像素） |
|---|---|---|---|---|
| LaTeX Type1 文字（372 cmds） | 14.3 ms | 2.9 ms | 1.7 ms | 0.04% |
| coat of arms 向量（855 cmds） | 147 ms | 2.2 ms | 3.0 ms | 0.09% |
| 掃描 A4 300 dpi JPEG（1 cmd） | 66 ms | 2.2 ms | 2.5–3.0 ms | 0% |
| CJK 2,080 字非內嵌（52 runs） | 73 ms | 8.2–8.5 ms | 6.6 ms | 0.01% |
| CAD A0 62k strokes | （被 64 MP 上限改 scale） | 36–37 ms | 34 ms | — |

**每個 tile 的固定成本 ∝ command 數**：每個 tile 都要重播整個 display list。CAD 頁每個 tile 約 35 ms，即使 tile 內什麼都沒有也一樣。

**Adapter 側 culling 原型**（只用公開欄位，先為每個 paint command 算保守的 page-space bbox，tile 外的 paint 就跳過，clip 和 group 的 push/pop 一律照常執行）：

| 文件（scale） | tile 數 | 整頁 | 全 tile 1 執行緒，無 culling → 有 culling | 有 culling 時 8／16 執行緒 | culling 前後差異 |
|---|---|---|---|---|---|
| CAD A0（1.5） | 70 | 206 ms | 2087 → **237 ms** | 34 / 21 ms | 0/70 個 tile 有差異 |
| CJK（4） | 35 | 76 ms | 271 → 114 ms | 17 / 13 ms | 0/35 |
| coat of arms（4） | 72 | 161 ms | 289 → 174 ms | 40 / 25 ms | 0/72 |
| pdftc 實際文件，152 張影像（4） | 35 | 75 ms | 111 → 85 ms | 14 / 13 ms | 0/35 |
| LaTeX（4） | 35 | 16 ms | 56 → 18 ms | 4 / 4 ms | 0/35 |

結論：**CPU 路徑可以做 SPEC §12 的 tile rendering，而且不需要改 upstream**。tile 結果和不做 culling 時 bit-identical，tile 和整頁裁切之間只有 AA 等級的差異。CJK 的 culling 粒度是「一行 run」，若在 fork 中把 run 切成 per-glyph culling，效果還會更好。

**取消粒度**（在 command 之間檢查，`harness cmdtime`，150 dpi 整頁）：CAD 最長 0.068 ms／command；coat of arms 最長 2.7 ms；CJK glyph run 最長 1.3 ms；整頁掃描影像 48.5 ms（576 dpi 時 242 ms）。改成 tile 之後，影像 command 的成本只和 tile 面積成正比，約 2–3 ms。interpret 階段無法中斷（掃描頁 53–60 ms，病態頁面最長 8 s）。

---

## Threading

**Send/Sync probe**（以 `impls` 技巧在 compile time 判斷，harness `probe`）：

| 型別 | Send | Sync |
|---|---|---|
| `PdfDocument`、`PdfFile` | ✅ | ❌（`RefCell`/`Cell`/`OnceCell` 快取，`zpdf-parser/src/lib.rs:44-60`；`SharedFonts` 也是 `RefCell`，`zpdf-document/src/lib.rs:71`） |
| `PdfPage`、`FontCache`、`DisplayList`、`RenderCommand`、`ImageCache`、`IccCache`、`TextSpan` | ✅ | ✅ |
| `ContentInterpreter<'_>` | ❌ | ❌（借用 `&PdfFile`） |
| `CpuRenderer<'_>`、`WgpuRenderer<'_>`、`GpuContext` | ✅ | ✅ |

- engine 內部沒有任何 thread、rayon 或 Mutex。唯一的例外是系統字型的全域 `OnceLock<Mutex<…>>`（`zpdf-font/src/system.rs:302-328`）。upstream 的 batch bench 自己也寫：「the workspace contains no threading at all … a parsed document cannot be shared across threads today」（`zpdf-benches/benches/batch.rs:1-25`）。
- 因為 `PdfDocument: !Sync`，同一份文件無法被多個 thread 同時 interpret。用 `Mutex` 包住會讓 parse／interpret 全部序列化。
- **實測方案 A：每個 thread 各開一份 document**（共用 `Arc<[u8]>`，零複製）。CJK 16 頁、150 dpi：1 執行緒 1024 ms、2 執行緒 560、4 執行緒 349、8 執行緒 295 ms，但 peak private 從 55 MB 漲到 **458 MB**（每份 document 各自有快取，也各自載入 CJK 系統字型）。
- **實測方案 B：單一擁有者 interpret，多個 worker 共用 DisplayList 平行 raster tile**（見上表，16 執行緒時 CAD 全 tile 21 ms）。**建議採方案 B**：一個「document actor」thread 擁有 `PdfDocument`，負責 parse 和 interpret；tile worker pool（依 SPEC §18 測 2/4/6/8）共用 `Arc<PreparedPage{DisplayList, FontCache, ImageCache}>`。

---

## Caches & Memory

| 快取 | 位置 | 生命週期 | 預設上限 | 淘汰 | 外部控制／可觀測性 |
|---|---|---|---|---|---|
| object_cache（含 stream 原始 bytes 的複本） | `zpdf-parser/src/lib.rs:47, 272-283` | 文件 | 512 MiB（`zpdf-core/src/limits.rs:87`） | 無，只有准入；滿了以後改成每次重新 parse | 開檔時的 `ParseLimits`；沒有 getter |
| objstm_cache | `lib.rs:53, 589-596` | 文件 | 256 MiB | 無 | 同上 |
| SharedFonts（已解析字型） | `zpdf-document/src/lib.rs:71-106` | 文件 | `max_font_cache_bytes` 256 MiB | 無，文件存活期間一直保留 | `ParseLimits`；沒有 getter |
| `FontCache`（每頁或每個 display list） | `zpdf-font/src/lib.rs:1833-1944` | 跟 DL 一樣 | 准入上限 | **無**（ID 必須穩定）。README「LRU 256-font」不實 | `len()`、`bytes_used()` |
| `ImageCache`（解碼後 RGBA） | `zpdf-image/src/lib.rs:7-79` | 跟 DL 一樣 | 1 GiB（以 capacity 計） | 無；只有手動 `remove` | `with_image_cache_limit`、`bytes_used()` |
| image_obj_cache、shading_cache | `interpreter.rs:60-65` | 單次 interpret | — | — | **不跨頁**：3 頁共用同一張 JPEG 時，每頁都會再解碼一次（實測 59.7、59.6、61.0 ms） |
| CPU 縮小影像快取 | `zpdf-render-cpu/src/lib.rs:80, 347` | 單次 render | 64 MiB | 每頁清空 | — |
| CPU soft mask 平面 | `lib.rs:74` | 單次 render | 512 MiB | 每頁清空 | `ParseLimits` |
| CPU 字形快取 | 無 | — | — | — | 每個字形實例都重新 outline |
| GPU glyph atlas | `glyph_atlas.rs:1-37` | 單次 render | 2048² R8 | 每頁丟棄 | — |
| GPU 影像 texture | `zpdf-render-wgpu/src/lib.rs:694-702` | 單次 render | `max_gpu_texture_bytes` 1 GiB | 每次 render 重新上傳 | — |
| 系統字型索引與字型檔 | `zpdf-font/src/system.rs:302-328` | process | 索引無上限；檔案以 `Weak` 快取 | — | `ZPDF_FONT_DIRS` |

重點：

- 預設上限加總可達數 GB，必須在開檔時用 `ParseLimits` 改成 FastPDF 的預算（SPEC §15：font 32 MB、image 64 MB…）。即使這樣，也只是「准入上限」：沒有 LRU、沒有 soft/hard limit 回呼，也沒辦法從外部要求淘汰。SPEC §16 的 MemoryBudgetManager 只能在 adapter 層以「丟棄整個 PreparedPage」的粒度實作。
- 影像一律以全解析度解碼：A4 300 dpi 掃描頁的 RGBA 是 34.8 MB，96 dpi 的縮圖也一樣（PR #17 `with_image_downscale` 從 2026-08-05 開到現在還沒 merge）。
- stream 解析時會複製兩次原始 bytes（`to_vec` 一次、`Arc::from` 一次，`zpdf-parser/src/object_parser.rs:135-138`），而且被 object_cache 保留。圖多的大檔瀏覽一陣子後，記憶體會逼近「檔案大小 + 快取上限」。
- GPU 路徑：同一頁連續 render 50 次，private 穩定在約 425 MB（readback）或 445–653 MB（submit-only），沒有 leak，但常駐成本很高。只用 CPU 的 process 是 10–155 MB。

---

## Robustness & Security

**預算與上限（`ParseLimits` 預設值，`zpdf-core/src/limits.rs:68-96`）：** 物件巢狀深度 100、單一 stream 256 MiB、累計解碼 256 MiB（防 decompression bomb）、影像 1e8 像素、頁面 operator 1e6、字串 16 MiB、ObjStm／recovery 物件數 5e6、operand stack 1 萬、q/Q 深度 256、marked content 深度 128、blend group 深度 16、page raster 64 MP、GPU texture 1 GiB。

另外：
- interpret 有 8 s 時間預算、50 萬 command、400 萬 operator 的上限（`interpreter.rs:199-211, 690-709`）。
- CPU render 有 8 s 時間預算，以及 clip、blend 的位元組和像素工作量預算（`zpdf-render-cpu/src/lib.rs:325-348, 2533-2564`）。
- page tree 深度 64、頁數上限 100 萬（`page.rs:11-20`）；ref 鏈最長 32（`zpdf-parser/src/lib.rs:215`）。
- shading raster 上限 64 MP（`shading.rs:14`）；outline 65,536 項；name tree 10 萬節點。

**Panic 政策：**
- 非測試程式碼（每個檔案 `#[cfg(test)]` 之前）的統計：`unwrap()` 34 處（其中 CLI 19 處），`expect(` 23 處，`panic!` 2 處（都在 benches），`unreachable!` 9 處，`todo!/unimplemented!` 0 處。
- 自家程式碼 0 個 `unsafe`，但沒有任何 crate 宣告 `#![forbid(unsafe_code)]`。
- `catch_unwind` 只包住 hayro-jpeg2000（`zpdf-image/src/lib.rs:704-713, 771`）。zune-jpeg、ttf-parser、tiny-skia 等第三方元件的 panic 會一路傳到呼叫端。
- 隱性 panic（slice index）沒有統計。**FastPDF 必須自己在 engine 邊界用 `catch_unwind` 隔離**（SPEC §24），必要時改用子行程。

**Fuzz：**
- `fuzz/` 是獨立的 cargo-fuzz crate，5 個 target 都是 parser 層：lexer、object_parser、filters、content_tokenizer、parse_pdf。
- CI 每晚對每個 target 跑 15 分鐘（`.github/workflows/fuzz.yml`），最近幾晚都是綠的。
- **字型解析、interpreter、影像解碼後續處理、renderer 沒有被 fuzz。**

**自行實測：**
- zpdf repo 內 `tests/failed` 共 496 份惡意或損壞 PDF（來源有 Ghostscript、PDFBox、poppler、pdf.js、PDFium 等專案的 bug tracker）：306 份成功 render 第 1 頁、190 份回傳乾淨的錯誤（137 份 page tree 無可用頁、32 份 xref 無效、20 份不是 PDF、1 份 raster 尺寸溢位），**0 panic、0 timeout**。中位數 77 ms，p95 305 ms，最長 8.6 s（觸發 8 s 預算）。注意這是 upstream 拿來調校過的語料，不算獨立測試。
- hayro 的 365 份測試 PDF（獨立語料）：319 份成功、46 份乾淨錯誤（全部在 hayro 的 `load/` crash 回歸集），0 panic。
- 附帶發現：`tests/failed` 在 git 中有 497 份，其中 `batch3/MOZILLA/MOZILLA-579714-2.zip-2.pdf` 在 clone 當下（07:37:31）就被 Windows Defender 判定為 `Exploit:Win32/Pidief.BH` 並隔離（`Get-MpThreatDetection`），所以實測是 496 份，`git status` 顯示該檔已刪除，但不是本 audit 造成的。FastPDF 的惡意 PDF 測試語料（SPEC §27）在 Windows 開發機上要考慮 Defender 隔離問題，例如放在排除路徑，或只在 CI 上使用。

**正確性 bug（新發現）：**
- `with_page_rotation`（`interpreter.rs:746-774`）把旋轉矩陣烘進 CTM，並把 page_rect 改成以 (0,0) 為原點，但沒有處理 effective box 的原點偏移。
- 用 `MediaBox [100 100 712 892]` 做測試：Rotate 0 時 98.5% 是正確的藍色；Rotate 90/180/270 時 26.9% 是空白，內容位移並被截掉。
- 所有內建呼叫端都中招：CLI `main.rs:1485-1492`、GPUI viewer `document.rs:171-177`、winit viewer `simple-viewer.rs:81-87`。
- 已驗證的 workaround：旋轉不為 0 時接著呼叫 `with_content_translation(-x0, -y0)`，四個方向都恢復成 98.5%。這個函式的註解自己提到 CropBox 原點的用途（`interpreter.rs:776-786`），但沒有任何呼叫端使用。

**其他觀察：**
- 系統字型 fallback 第一次使用時會掃描 `%WINDIR%\Fonts`（實測約 12–23 ms，含載入 CJK TTC）。
- 預設 dependency 帶入 `rsa`（簽章驗證），建議用 cargo-audit 確認 RustSec 狀態，該 advisory 主要影響私鑰運算。
- optional feature `zpdf-writer/timestamp` 會帶入 `ureq`、`rustls`、`ring`、`webpki-roots`，形成網路依賴。

---

## Maturity & Maintenance

| 指標 | 數值（證據） |
|---|---|
| 第一個 commit | 2026-05-26「feat(init):init the project.」（`git log --reverse`）；GitHub repo 建立於同一天 |
| commit 數 | 247；每月：5 月 6、6 月 61、7 月 85、8 月 74、9 月 21（**9 月明顯下降**），最後一筆 2026-09-28 |
| 程式碼量 | 約 119.5k 行 Rust；累計 +144,664 / −9,748 行 |
| contributor（`git shortlog -sne`） | YUZHEthefool 211 + 同一人的 alias Thefool 8 = 219（89%）；dependabot 18；其他 4 位人類共 8 筆、xero-team-bot 2 筆 → **bus factor ≈ 1** |
| release | tag v0.4.0 到 v0.14.0 共 12 個（2026-06-11 到 2026-09-22）。crates.io 也是 12 版，總下載 1,359，reverse dependency 只有 2 個外部 crate（pdfbull、bevy_document_extend） |
| GitHub | 14 stars、6 forks；issue 史上只有 2 筆（#16 PathElement 記憶體 open、#43 fuzz crash closed）；PR 43 筆（dependabot 24、作者 8、外部貢獻者 10、xero-team-bot 1）；外部 PR #15、#17 從 8 月擱置至今 |
| CI | fmt、clippy `-D warnings`、ubuntu 加 windows 的 build 與 test、lavapipe 跑 GPU oracle、每晚 fuzz、bench perf gate；最近幾次都綠 |
| API 穩定性 | 3.5 個月 12 版，有 breaking change（例如 `FontCache::with_capacity` deprecated、wgpu 29→30）。**沒有 commit `Cargo.lock`**，加上 workspace 有 GPUI git dependency，連單純 build CLI 都要先 clone zed（約 350 MB）、zed 的 wgpu fork、font-kit、xim-rs 才能完成 resolve |

**AI 輔助開發的跡象與影響：**

- `CLAUDE.md` 給 Claude Code 的指引，其中寫著「As an AI agent, do not author the human note yourself … never commit or open a PR autonomously」。
- `AI_POLICY.md` 要求每個 commit/PR 附上以母語寫的 `## Human note`，並禁止 autonomous agent 開 PR 或 issue。
- `zpdf-skill` 是 git submodule（沒有 checkout），指向另一個 repo，內有 `SKILL.md` 和 `references/`，讓 Claude Code、Codex、opencode 等工具操作 zpdf CLI。
- `.gitignore` 排除了 `/.claude`。
- 有 34 筆 commit 帶 `Co-Authored-By: Claude Opus 4.8 (1M context)` 或 `Claude Fable 5`。
- `docs/performance/PERFORMANCE.md` 有「待用户决策／用户选 pipelining」這類 agent session 決策紀錄。

上述指示屬於 upstream 的資料，本 audit 只記錄，沒有照做。

影響：
- 程式碼大多由 AI 產生，單人審查，4 個月約 12 萬行。測試與文件看起來很完整，但審查深度與長期維護能力存疑。我在半小時內就找到一個所有呼叫端都受影響的旋轉 bug；README 也有與程式碼不符的宣稱（LRU font cache、GPU 與 CPU 差異 <1%、「render backends never depend on the parser」）。
- 如果 FastPDF 要把修正回饋 upstream，必須由人類寫 Human note，而且不能由 agent 自動開 PR（這和我們「不在 GitHub 寫入」的規則一致）。
- 授權來源：MIT，沒有發現複製 GPL 程式碼的跡象（未逐檔比對）。

---

## Build & Measurements

### Build（Windows，MSVC）

| 項目 | 結果 |
|---|---|
| `cargo build --release -p zpdf-cli`（第一次） | 成功。總時間 14m41s，但其中約 14 分鐘在等 package cache lock 和下載（多個 agent 同時跑 cargo）；實際編譯約 45 s |
| 同指令，全新 target dir、離線 | **21.7 s**；`zpdf.exe` **7,080,448 bytes（6.75 MiB）** |
| `-p zpdf-cli --features gpu`（全新 target dir） | 44 s；`zpdf.exe` 14,013,952 bytes（13.4 MiB） |
| `-p zpdf-viewer-gpui`（25 分鐘時限內） | **成功**，109 s（沿用 CLI 的 target dir）；`zpdf-viewer-gpui.exe` 25,638,400 bytes（24.5 MiB）。有一個 future-incompat 警告（proc-macro-error2）。GPUI rev `d989c7c5`，日期 2026-06-09，gpui 0.2.2。沒有實際啟動 GUI |
| 純 Rust | 預設與 gpu feature 的 dependency graph 都沒有 `cc` 或 `cmake`；`timestamp` feature 會帶入 `ring`（需要 C toolchain） |

### Render 實測（粗略單次；harness 為進程內量測，CLI 為整個 process）

harness `bench`：150 dpi，GPU 為 RTX 5090／Vulkan，GPU 數字取第 2、3 次（已暖機）：

| PDF（類型） | open | prepare 冷（fonts / interpret） | CPU 整頁 | GPU 整頁 + readback | GPU submit-only | GPU tile 576 dpi | CPU tile 576 dpi | GPU vs CPU 差異（>16/255，`zpdf compare`） |
|---|---|---|---|---|---|---|---|---|
| hayro `fonts_type1_latex`（文字，Type1） | 1.9 ms | 5.5（5.1 / 0.3） | 7.8 ms | 10.9–11.3 | 7.5–8.3 | 12.7 | 1.7 | 2.8% |
| hayro `integration_coat_of_arms`（向量） | 0.14 | 5.6（0 / 3.7） | 60 | 17.5–21.0 | 13–18 | 18.3 | 3.0 | 0.68% |
| hayro `image_cmyk_icc_jpg`（200×200 pt 小圖，CMYK JPEG + ICC） | 4.1 | 11.5（0 / 11.4） | 1.3–1.5 | 0.6–0.8 | 0.2–1.3 | 0.9 | 2.1 | （>8：0%） |
| 自製掃描 A4 300 dpi JPEG（影像） | 0.48 | 53（0 / 53） | 47–48 | 36–39 | 30–34 | 22.9 | 2.5–3.0 | 0.19% |
| 自製 CJK 正體 2,080 字（非內嵌） | 0.11 | 23.3（22.8 / 0.5） | 43 | 46–47 | 44 | **155** | 6.6 | **20.3%**（MAE 9.8） |
| hayro `font_cid_2`（CJK 內嵌直排） | 0.54 | 0.6 | 1.4 | 6.5–6.8 | 3.5–4.9 | 3.1 | 0.17 | （>8：0.3%） |
| 自製 CAD A0（62k strokes） | 0.36 | 59（content 10.7 / 47.7） | 436–665 | **失敗**：需要 1331 MiB，超過預設 1 GiB | 失敗 | 49 | 34 | — |
| hayro `pattern_shading_type2_many` | 6.7 | 11.6 | 2.1 | 2.5–2.8 | 1.6–1.8 | 3.0 | 1.5 | （>8：8.0%） |
| hayro `pdftc_900k_0907`（實際文件，152 張影像） | 0.37 | 46（17.1 / 28.8） | 31 | 15.6–17.4 | 9.4–15.5 | 13.0 | 3.6–4.0 | 1.8% |

- GPU context 初始化：每個 process 0.34–0.74 s。GPU 路徑的 private memory 為 125–650 MB，只用 CPU 時是 10–155 MB。
- CLI（`zpdf render --stats`，process 牆鐘時間，含啟動、open、interpret、render、PNG 編碼）：CPU 181–274 ms；wgpu 557–744 ms，其中 wgpu 回報的 `wall` 375–575 ms 主要是建 device，`gpu pass` 只有 0.02–0.64 ms。
- 輸出圖檔：scratchpad `agent-zpdf\out\`（`*_cpu_150dpi.png`、`*_gpu_150dpi.png`、`*_tile_s4.png`、`*_gpu_tile_s8.png`、`cli_*_{cpu,wgpu}.png`、`rot*.png`、`rotfix*.png`）。

---

## License & Dependencies

依 `cargo metadata --format-version 1 --filter-platform x86_64-pc-windows-msvc` 加 Python 走訪 normal dependency closure（各情境都是獨立的最小 crate，避免 workspace feature unification 影響結果）：

| 情境 | package 數（含 zpdf 自家 crate） | 授權分佈 | 非 permissive／需審查 |
|---|---|---|---|
| `zpdf`（預設：cpu-render） | 125 | MIT OR Apache-2.0 83、MIT 21、BSD-3／BSD-2／Zlib／Unlicense OR MIT／0BSD、Unicode-3.0（含 `(MIT OR Apache-2.0) AND Unicode-3.0`） | **無** |
| `zpdf` + gpu-render | 192 | 同上，再加 wgpu/naga（MIT OR Apache-2.0）、ISC 1 | **無**（帶入 `ash`、`libloading`、`renderdoc-sys`、`windows 0.62`） |
| + `zpdf-writer/timestamp` | 140 | 加上 `ring`（Apache-2.0 AND ISC）、`rustls`、`ureq` | `webpki-roots`（CDLA-Permissive-2.0，資料授權，需附 notice） |

- 預設 graph 就帶入 `rsa`、`p256`、`p384`（`zpdf-document` 的簽章驗證）以及 `getrandom`（`zpdf-writer`，由 facade 無條件依賴，`crates/zpdf/Cargo.toml`）。FastPDF 若改成直接依賴 `zpdf-document`、`zpdf-content`、`zpdf-render-cpu`，可以去掉 writer，但去不掉簽章相關的 crypto（除非 fork 時改成 feature gate）。
- **網路依賴**：只有 `zpdf-writer` 的 `timestamp` feature（`ureq` 3 加 rustls、ring、webpki-roots，`crates/zpdf-writer/Cargo.toml`）。FastPDF 要零網路，不可開啟這個 feature。預設與 gpu feature 都不含任何 HTTP 或 TLS crate。
- 整體授權結論：預設與 gpu 情境沒有 GPL、AGPL、LGPL、MPL，商業化友善（SPEC §37）。仍需建立 `THIRD_PARTY_LICENSES.md`（Unicode-3.0、BSD、Zlib 需要附 notice）。

---

## Risks

| # | 風險 | 嚴重度 | 證據 | 緩解 |
|---|---|---|---|---|
| R1 | 單一維護者、AI 大量產碼、API 變動快、9 月起降速 | 高 | 〈Maturity & Maintenance〉 | pin 在 `fe0ed23` 的 fork 或 vendor；只挑選需要的修正 cherry-pick；保留 Hayro |
| R2 | 整檔常駐記憶體，加上 open 時 2× peak | 中 | 530 MB 檔 peak 1015 MB | adapter 直接讀進 `Arc<[u8]>`；長期在 fork 中加 mmap 抽象 |
| R3 | 沒有取消機制，interpret 不能中斷 | 中 | 〈API Mapping〉、cmdtime 實測 | 自己跑 execute 迴圈並檢查 token；預取時控制 interpret 併發；fork 加 cancel hook |
| R4 | 快取只有准入上限、預設值很大、影像以全解析度解碼且不跨頁共用 | 中高 | 〈Caches & Memory〉 | 調小 `ParseLimits`；在 adapter 層做 PreparedPage LRU；在 fork 中做跨頁 decoded image cache 與 decode-to-scale（參考 PR #17） |
| R5 | 旋轉加非零原點的 bug；64 MP 默默改 scale；shading 固定解析度 | 中 | 〈Robustness & Security〉、CLI 警告輸出 | adapter 加 `with_content_translation`；只走 tile 路徑（不會碰到 64 MP）；回歸測試 |
| R6 | 每個 tile 都重播全部 command（沒有 bbox） | 中 | 〈Rendering Model〉 | adapter 側 culling（已驗證 bit-identical）；fork 中做 per-glyph 粒度 |
| R7 | GPU backend 不適合 tile，而且記憶體大 | 高（若選 GPU） | Q5 | M3 只用 CPU；GPU 列為之後研究項目 |
| R8 | 第三方 decoder 的 panic 沒有被隔離 | 中 | 只有 JPX 有 `catch_unwind` | engine 邊界加 `catch_unwind`；把 panic 視為「該頁失敗」 |
| R9 | 文字幾何只到 span 等級 | 中（影響選取與搜尋 UX） | `text.rs:12-28`、`search.rs` | M7 前在 fork 中加 per-glyph Unicode 與 box |
| R10 | build 可重現性：沒有 lockfile，workspace 會拉 GPUI git dependency | 低中 | build log | vendor 時用我們自己的 lockfile，只納入需要的 crate |
| R11 | Fuzz 只涵蓋 parser 層 | 中 | `fuzz/Cargo.toml` | FastPDF 自己對 interpret 加 render 做 fuzz（M1 之後） |

---

## Verdict

### Q4：zpdf 現況是否真的適合 interactive PDF Reader？

**答：部分適合。可以當作 render core，但不能當作 Reader engine 直接使用。信心度：中高。**（依據是程式碼閱讀加上本機實測；還沒有跑大型真實語料或長時間 session。）

- 適合的部分：
  - 功能涵蓋很廣：加密、ObjStm/XRefStm、JBIG2、JPX、CCITT、ICC、blend、soft mask、annotation、OCG、CJK CMap 加系統字型 fallback。
  - 對惡意輸入很耐打：861 份語料 0 panic、0 hang。
  - 純 Rust、MIT、build 很快（CLI 21.7 s）。
  - 一般頁面的 CPU 速度可以接受：文字頁 8 ms、實際文件 31 ms、向量頁 60 ms（150 dpi）。
  - **DisplayList 可快取、可在多執行緒間共用，tile rendering 不用改 upstream 就能做。**
- 不適合的部分：
  - 沒有高階或 lazy 的開檔 API（整檔進記憶體，開檔就走完整棵 page tree）。
  - 沒有取消、progress 或可設定的時間預算。
  - `PdfDocument: !Sync`，engine 內部完全沒有 threading。
  - 快取不會淘汰，而且不跨頁共用影像。
  - 文字幾何太粗。
  - 有確認的 rotation bug。
  - bus factor ≈ 1。
- 建議：M3 照計畫做 `fastpdf-engine-zpdf`，adapter 負責：
  1. 自己讀檔進 `Arc<[u8]>`，並設定 FastPDF 版的 `ParseLimits`；
  2. document actor thread 加上 `PreparedPage{DisplayList, FontCache, ImageCache}`，以 byte 預算做 LRU；
  3. tile 以 `begin_page/execute/end_page` render，搭配 bbox culling 和逐 command 的取消檢查；
  4. 旋轉時套用 `with_content_translation` workaround；
  5. 在 engine 邊界做 `catch_unwind`。
  同時以 pin 住的 fork 依賴 upstream。

### Q5：zpdf GPU backend 是否適合 tile-based viewport rendering？

**答：不適合（以目前架構而言）。信心度：高。**

- 架構面：
  - 自建 headless device，沒有 API 能注入或共用外部 device。
  - 每次 render 都重新配置 target、重新上傳所有影像、重建 per-page atlas。
  - tessellation 和字形 raster 都在 CPU 上，而且不做 culling。
  - 結果只能透過阻塞式 readback 取得；submit-only 會丟掉像素。
- 整合面：GPUI（rev d989c7c5）在 Windows 是 D3D11 renderer（`gpui_windows/src/directx_devices.rs`）；`surface()`／`PaintSurface` 只有 macOS（`gpui/src/elements/surface.rs:9-38`）。所以 wgpu（Vulkan/DX12）到 GPUI 的零複製 texture 共享，必須同時 fork zpdf（暴露 texture）和 GPUI（加上 D3D11 shared-handle 匯入），流程一定是「GPU render → readback → GPUI 再上傳」。zpdf 自己的 GPUI viewer 和 winit viewer 也都是這樣做。
- 實測：
  - 9 份測試文件中有 8 份 GPU tile 比 CPU tile 慢（1.4–23 倍），唯一例外是只有一張小圖的頁面。
  - context 初始化 0.34–0.74 s。
  - GPU 路徑的 process private memory 為 125–650 MB（只用 CPU 時約 10–155 MB）。
  - A0 頁面在 150 dpi 就超過預設 1 GiB GPU 預算而失敗。
  - CPU 與 GPU 輸出差異在 CJK 頁達 20.3%，同一頁混用兩種 backend 會出現接縫。
- 真正的 GPU 工作只有 0.02–0.6 ms，所以 GPU 的潛力在於「GPU 常駐」的設計：保留每頁 geometry 與影像 texture、glyph atlas 跨頁保留、culling、和 UI 共用 device、不做 readback。這等於重新設計 backend，而且受限於 GPUI 在 Windows 是 D3D11。不列入 M3–M5。

### 對 Q6（是否保留 Hayro fallback）的證據

建議**保留**，至少保留到 M4 有數據為止：

- zpdf 只有 4 個月、bus factor 1；本次就找到一個 rotation bug，README 也有不實宣稱。
- Hayro 可以當 differential testing 的參考實作（SPEC §3）。zpdf 本身已依賴 hayro 系列的 `hayro-jpeg2000`。
- zpdf 在 hayro 語料上的失敗全部集中在 `load/` crash 集，不構成「zpdf 比較差」的證據。正確性需要在 M4 用像素比對決定。

### 對 Q7（最可能的瓶頸）的證據

| 候選 | 證據 | 判斷 |
|---|---|---|
| PDF parse | 一般檔 open ≤7 ms；20k 頁 65 ms；530 MB 檔 read + open 約 0.25–0.33 s（另有 2× 記憶體 peak） | 一般情況不是瓶頸；超大檔的瓶頸是 I/O 與記憶體 |
| fonts | 第一頁字型載入 5–23 ms（Type1 解析、系統字型索引、CJK TTC）；CPU 每個字形約 20 µs（2,080 字 CJK 頁 43 ms）；沒有字形快取 | **文字頁的主要成本** |
| image decode | 掃描頁每頁 53–60 ms，全解析度 35 MB RGBA，同一張圖每頁重新解碼，interpret 時無法中斷 | **掃描或影像 PDF 的首要瓶頸** |
| rasterization | CAD 62k strokes 在 150 dpi 要 436–665 ms；高倍率整頁和面積成正比（coat of arms 576 dpi 要 469 ms）；不做 culling 的全部 tile 是整頁的 7–10 倍 | 向量密集頁的主要瓶頸；改用 tile 加 culling 加多執行緒後可降到 21–34 ms |
| GPU upload | GPU 路徑每次 render 都重新上傳影像（掃描頁 tile 22.9 ms vs CPU 2.5 ms）；GPUI 端仍要上傳一次 RGBA | 只在選用 GPU 或大量 tile 上傳時才是問題；CPU tile 到 GPUI 的上傳仍需在 M5 量測 |
| texture cache／GPUI layout／scroll | 不屬於 zpdf 的範圍。參考：zpdf GPUI viewer 在 UI thread 同步 render，sidebar 每 frame 為所有頁面建立元素（`viewer.rs:1134-1154, 1489-1505`） | 交給 GPUI audit |

### zpdf-viewer-gpui 架構摘要（UI 參考）

- `app.rs`：開窗前先 `load_document`，會 `fs::read` 整個檔案，並對**所有頁面**呼叫 `pdf.page(i)` 建立 summary（`document.rs:87-128`）。大檔會延遲視窗出現。
- 單頁檢視：`Render::render` 裡呼叫 `prefetch_nearby_pages()`，在 **UI thread 上同步** render 目前頁和前後各一頁（`viewer.rs:984-1022, 1132-1134`）。
- 固定 144 DPI，放大靠圖片縮放（`ObjectFit::Fill`），所以會模糊；沒有 tile。
- 快取只保留前後各一頁，上限 384 MiB（`viewer.rs:21-28`）。
- GPU render 失敗時退回 CPU；`GpuContext` 跨頁重用。
- 交給 GPUI 的方式：RGBA→BGRA swizzle，接著 `RenderImage`，再 `img()`（`document.rs:284-293`），每次換頁都要重新上傳整頁。
- 另有 ink、印章、浮水印、annotation 編輯並存檔（`zpdf-writer`），超出 Reader 範圍。
- 結論：**只適合拿來參考「zpdf 加 GPUI 怎麼接」**，不適合作為 FastPDF UI 的架構範本（違反 SPEC §12–§14、§18）。
