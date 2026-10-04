# zpdf upstream issue 草稿

對象：[Xero-Team/zpdf](https://github.com/Xero-Team/zpdf)，commit `fe0ed23`（v0.14.0-1-gfe0ed23，2026-09-28）。
來源：M0 audit（`docs/audit/zpdf.md`）、M3 adapter 實作（`crates/fastpdf-engine-zpdf/`）與本次追查。

證據的來源各不相同：

- #1、#2、#3、#5、#6：文中的重現程式碼（含附錄 helper）在 Windows 11 上只對 zpdf 編譯並實際執行過，數字照抄輸出。
- #7：本次只用 zpdf API 量測。
- #4：依 M3 adapter 的測試（用 zpdf-writer 產生的加密檔）和程式碼閱讀。
- #8、#9 的記憶體數字與 #11 的 GPU 數字：沿用 M0 audit 的實測。

**本文件只是草稿，沒有在 GitHub 建立任何 issue，也沒有做任何寫入。要不要送出由使用者決定。** 送出前請注意：

- upstream 的 `AI_POLICY.md` 要求 issue 由人自己描述問題，禁止 autonomous agent 開 issue，也不建議貼 AI 產生的文字。非英語母語者可以用 AI 翻譯，但建議先用母語寫原文，再把英文譯文放在 blockquote 裡。因此請把下面的英文當參考資料，用自己的話改寫後再手動送出。
- 一個 issue 只放一個問題（bug report 模板的要求）。欄位對應 `.github/ISSUE_TEMPLATE/` 的 bug report、feature request、documentation 三種表單。
- 送出前先在最新的 `main` 上重跑重現步驟，並搜尋是否已有相同 issue。
- 重現程式碼共用的 helper 放在文末〈附錄：重現用 helper〉，所有 crate 都 pin 在同一個 rev。

## 索引

| # | 標題 | 類型 | 嚴重度 | FastPDF 的處理 |
|---|---|---|---|---|
| 1 | Rotated pages whose box does not start at the origin are shifted, and their annotations disappear | Bug | 高 | adapter 已繞過（`prepare.rs`：`with_content_translation` 加上平移 annotation `/Rect`） |
| 2 | Unused WinAnsiEncoding codes are not mapped to `bullet`; ReportLab list bullets render as "ù" | Bug | 中高 | adapter 已繞過（`fonts.rs`） |
| 3 | `Times-Bold` / `Times-Italic` / `Times-BoldItalic` fall back to regular Times New Roman on Windows | Bug | 中 | adapter 已繞過（`fonts.rs`） |
| 4 | Opening a password-protected PDF without a password succeeds and renders garbage | Bug | 高 | adapter 已繞過（`password.rs`：攔截 tracing 訊息） |
| 5 | Tiled CPU rendering shows seams where hairlines cross tile edges | Bug | 中 | adapter 已繞過（`hairline.rs`、4 px raster margin） |
| 6 | Pages above `max_page_pixels` are silently rendered at a lower scale | Bug | 中 | 只 render tile，不會碰到上限 |
| 7 | The image downscale cache is cleared on every `begin_page`, so every tile re-downscales whole images | Bug（效能） | 中 | 無 |
| 8 | Accept shared byte buffers (e.g. `Arc<dyn AsRef<[u8]> + Send + Sync>`) to avoid copying whole files | Feature | 中 | 開檔時複製一次 |
| 9 | Make `PdfDocument` `Sync` | Feature | 中 | interpret 用 mutex 序列化，raster 平行 |
| 10 | Cancellation and budget hooks for interpretation and rendering | Feature | 中高 | 只能在 display-list command 之間取消 |
| 11 | README overstates the font cache policy and the CPU/GPU parity | Documentation | 低 | — |

第 2、3 點是本次追查 FastPDF fixture `small-text/three-pages-platypus-times.pdf` 時確認的 zpdf bug，不是 fixture 的問題。用 `fastpdf-bench diff ... --engine hayro,zpdf --page 1` 比對時，hayro 畫出項目符號 • 和斜體頁尾；zpdf 把項目符號畫成「ù」，頁尾（`Times-Italic`）也變成正體。

---

## 1. Rotated pages whose box does not start at the origin are shifted, and their annotations disappear

- **Template**：Bug report；**Affected area**：Content interpretation and text extraction
- **zpdf version or commit**：`fe0ed23` (v0.14.0-1-gfe0ed23)
- **Environment**：Windows 11 Pro 10.0.26200 x86_64, rustc 1.99.0, zpdf crates used individually (no extra features), CPU renderer

### Problem summary

`ContentInterpreter::with_page_rotation` bakes the `/Rotate` matrix into the base CTM and resets the page rect to `(0, 0, w, h)`, but it never subtracts the origin of the visible box. On a rotated page whose CropBox/MediaBox does not start at `(0, 0)`, the whole content is shifted and partly cut off. Every built-in front-end calls it this way (`zpdf-cli` `main.rs:1485-1486`, `zpdf-viewer-gpui` `document.rs:171-172`, `zpdf-render-wgpu/examples/simple-viewer.rs:81-82`).

`with_content_translation(-x0, -y0)` fixes the page content, but annotation appearances are still drawn at the wrong place, because `paint_annotations` starts from `base_ctm`, i.e. the bare rotation without the translation (`interpreter.rs:2950`). They end up off the page.

Pages like this are common in practice (AutoCAD and other CAD exports, cropped scans).

### Minimal reproduction

```rust
// Helpers: see the appendix.
let objects = vec![
    obj("<< /Type /Catalog /Pages 2 0 R >>"),
    obj("<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
    obj("<< /Type /Page /Parent 2 0 R /MediaBox [100 100 300 300] /Rotate 90 \
         /Contents 4 0 R /Annots [5 0 R] >>"),
    stream("", b"0 0 1 rg 100 100 200 200 re f"), // blue: fills the whole page
    obj("<< /Type /Annot /Subtype /Square /Rect [100 250 150 300] /AP << /N 6 0 R >> >>"),
    stream("/Type /XObject /Subtype /Form /BBox [0 0 50 50]", b"1 0 0 rg 0 0 50 50 re f"),
];
let doc = PdfDocument::open(pdf(&objects))?;
let page = doc.page(0)?;
let (dl, fonts, images) = interpret(&doc, &page, /* translate */ false);
let out = raster(&dl, &fonts, &images, dl.page_rect, 1.0);
// Count blue (0,0,255), red (255,0,0) and white pixels of `out`.
```

### Expected behavior

The same picture as with `/Rotate 0`, only turned: about 93.8% blue and a 50×50 pt red square (6.2%) in the corner where the rotation moves the page's top-left corner.

### Actual behavior

200×200 px raster at scale 1:

| `/Rotate` | as the built-in front-ends call it | plus `with_content_translation(-100, -100)` |
|---|---|---|
| 0 | blue 93.8%, red 6.2% (correct) | — |
| 90 / 180 / 270 | blue 25.0%, **white 75.0%**, **red 0%** | blue 100%, **red 0%** (annotation gone) |

### Logs, backtrace, or sample PDF

- `crates/zpdf-content/src/interpreter.rs:746-773` (`with_page_rotation`), `:783-786` (`with_content_translation`, whose doc comment already mentions the CropBox-origin use case), `:2950` (`paint_annotations` uses `base_ctm`).

### Additional context

**Possible fix.** Fold the box origin into the base matrix in `with_page_rotation`, i.e. `base = rotation · translate(-x0, -y0)`, so content and annotations share it, and update the callers. Alternatively, `ContentInterpreter::new` could always normalize the page rect to the origin.

**Workaround (what FastPDF does).** Call `with_content_translation(-x0, -y0)` after `with_page_rotation` whenever the total rotation is not 0, and shift every annotation `/Rect` by `(-x0, -y0)` before `with_annotations`.

---

## 2. Unused WinAnsiEncoding codes are not mapped to `bullet`; ReportLab list bullets render as "ù"

- **Template**：Bug report；**Affected area**：Fonts, images, and color
- **zpdf version or commit**：`fe0ed23`
- **Environment**：Windows 11 Pro 10.0.26200 x86_64, rustc 1.99.0; non-embedded standard fonts substituted with Arial / Times New Roman

### Problem summary

ISO 32000-1 (Annex D.2, notes to the Latin character set table) says that in WinAnsiEncoding all unused codes above octal 40 map to the bullet character. zpdf's `WIN_ANSI_ENCODING` leaves the six codes that Windows-1252 does not define (0x7F, 0x81, 0x8D, 0x8F, 0x90, 0x9D) unmapped. A unit test even asserts `WIN_ANSI_ENCODING[129] == None` (`encoding.rs:1196`).

For such a code `LoadedFont::code_to_gid` returns `None`, and the interpreter then uses the character code as a raw glyph id (`interpreter.rs:4677-4683`). With a substituted system font that id has no relation to the PDF code: glyph 127 of Arial and Times New Roman is `ugrave`.

ReportLab writes list bullets (`ListFlowable(..., bulletType="bullet")` with a standard font) as `(\177) Tj` in a WinAnsiEncoding font, so every such bullet renders as "ù". hayro maps these codes to `bullet` (its `win_ansi.rs` follows PDFBox) and renders "•".

### Minimal reproduction

```rust
let objects = vec![
    obj("<< /Type /Catalog /Pages 2 0 R >>"),
    obj("<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
    obj("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Contents 4 0 R \
         /Resources << /Font << /F1 5 0 R >> >> >>"),
    stream("", b"BT /F1 48 Tf 20 30 Td (\\177) Tj ET"),
    obj("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>"),
];
let doc = PdfDocument::open(pdf(&objects))?;
let fonts = doc.load_page_fonts(&doc.page(0)?);
let (_, f1) = fonts.get_by_name("F1").unwrap();
assert_eq!(f1.encoding.as_ref().unwrap().glyph_name(0x7F), None); // expected Some("bullet")
assert_eq!(f1.code_to_gid(0x7F), None);       // the interpreter then draws GID 127
assert_eq!(f1.code_to_gid(0xF9), Some(127));  // GID 127 of the Arial substitute is "ù"
```

### Expected behavior

Codes 0x7F, 0x81, 0x8D, 0x8F, 0x90 and 0x9D render (and extract as text) like code 0x95: "•".

### Actual behavior

They are drawn as the glyph whose id equals the code: 0x7F becomes glyph 127 of the substitute font ("ù" with Arial / Times New Roman). Text extraction (`decode_to_string`) drops 0x7F and returns C1 control characters (U+0081, ...) for the others.

### Logs, backtrace, or sample PDF

- `crates/zpdf-font/src/encoding.rs:236` (`WIN_ANSI_HIGH`), `:1196` (test).
- `crates/zpdf-font/src/lib.rs:913-930` (`decode_to_string` for simple fonts).
- `crates/zpdf-font/src/lib.rs:696` (`code_to_gid`; its doc says callers fall back to the code as GID).
- `crates/zpdf-content/src/interpreter.rs:4677-4683` (raw-GID fallback).

### Additional context

**Possible fix.**
1. Map the six unused codes to `bullet` in the WinAnsi table. `/Differences` still overrides them.
2. Optional: for a substituted (non-embedded) font, fall back to `.notdef` (GID 0) instead of the raw code. A system font's glyph order is unrelated to PDF character codes, so a raw GID never draws the intended glyph.

**Workaround (what FastPDF does).** When a page's content shows one of these codes, the adapter reloads the page's WinAnsi-based simple fonts with `font_loader::load_single_font_dict`, calls `Encoding::apply_difference(code, "bullet")` for codes the font leaves unmapped, and builds a new `FontCache` under the same resource names. Fonts that form XObjects load for themselves are not covered.

---

## 3. `Times-Bold` / `Times-Italic` / `Times-BoldItalic` fall back to regular Times New Roman on Windows

- **Template**：Bug report；**Affected area**：Fonts, images, and color
- **zpdf version or commit**：`fe0ed23`
- **Environment**：Windows 11 Pro 10.0.26200 x86_64（`C:\Windows\Fonts\times.ttf`, `timesbd.ttf`, `timesi.ttf`, `timesbi.ttf` installed）

### Problem summary

The system-font index also registers every file stem as a key (`system.rs:442`), so `times.ttf` is indexed as `times`. For `Times-Bold`, `find_system_font` builds candidates from most to least specific: `timesbold`, `timesbold#b`, `times#b`, then **`times`** (the "a regular face beats no face" retry), and only after that the alias `timesnewroman#b`. The unstyled `times` key hits the file-stem alias of the regular face first. As a result all non-embedded Times styles render with the regular face (italic text comes out upright, bold comes out regular). Helvetica and Courier are not affected, because no file stem equals `helvetica` or `courier`.

### Minimal reproduction

```rust
use std::sync::Arc;
use zpdf_font::system::{find_system_font, SubstituteHints};

let find = |name: &str| find_system_font(name, SubstituteHints::default(), None).unwrap();
let roman = find("Times-Roman");
for styled in ["Times-Bold", "Times-Italic", "Times-BoldItalic"] {
    assert!(Arc::ptr_eq(&find(styled).data, &roman.data)); // passes: regular face
}
assert!(!Arc::ptr_eq(&find("TimesNewRoman,Bold").data, &roman.data)); // timesbd.ttf
```

On the test machine the three styled names resolve to `times.ttf` (1,190,736 bytes), while `TimesNewRoman,Bold`, `TimesNewRoman,Italic` and `TimesNewRoman,BoldItalic` resolve to `timesbd.ttf`, `timesi.ttf` and `timesbi.ttf`.

### Expected behavior

`Times-Bold` → `timesbd.ttf`, `Times-Italic` → `timesi.ttf`, `Times-BoldItalic` → `timesbi.ttf`.

### Actual behavior

All three resolve to `times.ttf`.

### Logs, backtrace, or sample PDF

- `crates/zpdf-font/src/system.rs:78-112` (candidate order), `:442` (file-stem alias).

### Additional context

**Possible fix.** Try every styled candidate (including the aliases) before any unstyled retry. Alternatively, do not let a file-stem alias satisfy a request that asks for a style.

**Workaround (what FastPDF does).** For a substituted `Times-Bold` / `-Italic` / `-BoldItalic`, the adapter reloads the font from a copy of its dictionary with `/BaseFont /TimesNewRoman,Bold` (`,Italic`, `,BoldItalic`). zpdf maps that name to the same standard-14 metrics and to the right face. The workaround is enabled only when `find_system_font` is observed to return the regular face for the styled name.

---

## 4. Opening a password-protected PDF without a password succeeds and renders garbage

- **Template**：Bug report；**Affected area**：PDF parsing and filters
- **zpdf version or commit**：`fe0ed23`
- **Environment**：Windows 11 Pro 10.0.26200 x86_64, rustc 1.99.0

### Problem summary

There is no way to learn that a document needs a user password. `PdfDocument::open` (empty password) never fails for an encrypted file:

- **RC4 / AES-128 (V ≤ 4).** When the empty password does not authenticate against `/U`, `Decryptor::from_encrypt_dict` keeps the "best-effort" key and only logs `tracing::warn!("encryption key did not validate against /U ...")` (`crypt.rs:169-185`). Every stream then decrypts to garbage.
- **AES-256 (V5).** Validation failure with an empty password returns `BuildResult::Degrade` (`crypt.rs:131`), and the document opens without any decryption (`lib.rs:182`).

`Error::WrongPassword` is returned only when a *non-empty* password fails. A viewer therefore cannot decide to show a password prompt; it renders noise instead.

### Minimal reproduction

1. Encrypt any PDF with a user password, e.g. `qpdf --encrypt user owner 128 --use-aes=y -- in.pdf aes128.pdf` and `qpdf --encrypt user owner 256 -- in.pdf aes256.pdf`.
2. `let doc = PdfDocument::open(std::fs::read("aes128.pdf")?)?;`, which returns `Ok`.
3. `doc.is_encrypted()` is `true`; interpreting page 0 produces garbage (V ≤ 4) or undecrypted content (V5). Only a `tracing` warning is emitted, and only for V ≤ 4.

### Expected behavior

`open` with no or an empty password returns an error such as `Error::PasswordRequired` when the empty password does not authenticate and the file has a valid `/U` (V ≤ 4) or fails V5 validation. Alternatively, the document exposes its authentication state, e.g. `PdfFile::encryption_status() -> NotEncrypted | Authenticated | Unverified | NotDecrypted`. The lenient best-effort path could stay for malformed files without `/U`, or become an explicit opt-in.

### Actual behavior

`Ok(doc)` with garbage content. No API reports that the key is unverified.

### Logs, backtrace, or sample PDF

- `crates/zpdf-parser/src/crypt.rs:89` (`from_encrypt_dict`), `:131` (V5 → `Degrade`), `:169-185` (best-effort key and warning).
- `crates/zpdf-parser/src/lib.rs:182` (`Degrade` → no decryptor).

### Additional context

**Workaround (what FastPDF does).** It installs a temporary `tracing` subscriber around `open` and turns the "did not validate against /U" event into `PasswordRequired`. For V5 it checks `doc.file().decryptor().is_none()` on an encrypted document. This depends on the exact log text, so a supported API would be much safer.

---

## 5. Tiled CPU rendering shows seams where hairlines cross tile edges

- **Template**：Bug report；**Affected area**：CPU rendering
- **zpdf version or commit**：`fe0ed23`（tiny-skia 0.12.0）
- **Environment**：Windows 11 Pro 10.0.26200 x86_64, rustc 1.99.0

### Problem summary

Rendering a page as tiles (one `begin_page` per tile, with `PageRenderInfo::page_rect` set to the tile's sub-rectangle) does not reproduce the full-page raster along tile borders when the page contains hairlines. zpdf clamps every stroke to at least one device pixel (`zpdf-render-cpu/src/lib.rs:758`), and tiny-skia strokes lines of at most one pixel with its hairline rasterizer. That rasterizer steps a *clipped* segment from the clipped end point. The same segment therefore lands on different pixels in rasters that clip it differently, and a steep thin line shows a visible jog of up to a few pixels at the tile edge. (Reproducible with tiny-skia alone, so tiny-skia may also deserve an issue.)

### Minimal reproduction

```rust
// 40 steep zero-width lines on a Letter page.
let mut content = String::new();
for i in 0..40 {
    let x = 15.0 * i as f64;
    content += &format!("0 w 0 0 0 RG {x} 0 m {} 792 l S\n", x + 200.0);
}
// One-page PDF with MediaBox [0 0 612 792] and this content (appendix helpers).
let doc = PdfDocument::open(one_page("/MediaBox [0 0 612 792]", content.as_bytes()))?;
let page = doc.page(0)?;
let (dl, fonts, images) = interpret(&doc, &page, false);
let scale = 2.0;
let full = raster(&dl, &fonts, &images, dl.page_rect, scale); // 1224 x 1584
// Render 256x256 px tiles: tile (tx, ty) covers page-space
//   x0 = tx / s, y1 = 792 - ty / s,
//   Rect::new(x0, y1 - (th - 1e-3) / s, x0 + (tw - 1e-3) / s, y1)
// and compare each tile with the matching crop of `full`.
```

### Expected behavior

Each tile equals the crop of the full-page raster, up to anti-aliasing noise.

### Actual behavior

403 pixels in 24 of the 35 tiles differ from the full-page raster by more than 64/255, with a maximum difference of 255 (the line is drawn in a different column).

### Additional context

**Possible fixes.**
- When rasterizing a sub-rectangle, stroke one-pixel segments that cross the raster edge through tiny-skia's regular path filler instead of the hairline rasterizer (e.g. width 1.0 + ε). The path filler is window-independent.
- Alternatively, rasterize with a guard margin of a few pixels. This helps, but does not remove the problem alone.
- Possibly report the clipping behavior to tiny-skia.

**Workaround (what FastPDF does).** The adapter renders each region with a 4 px margin and draws hairline segments that cross its 256 px tile grid 1.01 px wide. Measured cost at 96 dpi: none on a dense polyline map, +15% / +25% (full page / 512 px tiles) on a hairline mesh, 2.4× / 1.9× on a floor plan made of long hairlines. Widening every hairline instead cost 1.3–3.2×.

---

## 6. Pages above `max_page_pixels` are silently rendered at a lower scale

- **Template**：Bug report；**Affected area**：CPU rendering
- **zpdf version or commit**：`fe0ed23`
- **Environment**：Windows 11 Pro 10.0.26200 x86_64, rustc 1.99.0

### Problem summary

When `page_rect × scale` exceeds `ParseLimits::max_page_pixels` (default 64,000,000), `CpuRenderer::begin_page` shrinks `self.scale` until the raster fits and continues. It only logs `tracing::warn!` (`zpdf-render-cpu/src/lib.rs:2486-2509`). `end_page` then returns a `RenderedPage` of a different size than requested, with nothing else marking that the scale changed. A caller that places the result by its requested scale (zoom, printing, tiles) draws it at the wrong size.

### Minimal reproduction

```rust
let doc = PdfDocument::open(one_page("/MediaBox [0 0 2384 3370]",
                                     b"0 0 1 rg 100 100 2184 3170 re f"))?; // A0
let page = doc.page(0)?;
let (dl, fonts, images) = interpret(&doc, &page, false);
let out = raster(&dl, &fonts, &images, dl.page_rect, 4.0);
println!("{}x{}", out.width, out.height);
```

### Expected behavior

Either `Err(CpuRenderError::LimitExceeded(..))`, or the clamping is opt-in and the result reports the effective scale (e.g. `RenderedPage::scale`).

### Actual behavior

The requested raster is 9536×13480. zpdf returns 6728×9511 (effective scale 2.8221 instead of 4) and reports success.

### Additional context

**Workaround (what FastPDF does).** It renders only tiles, each far below the limit, and checks every returned size against the request.

---

## 7. The image downscale cache is cleared on every `begin_page`, so every tile re-downscales whole images

- **Template**：Bug report (performance)；**Affected area**：CPU rendering
- **zpdf version or commit**：`fe0ed23`
- **Environment**：Windows 11 Pro 10.0.26200 x86_64, rustc 1.99.0, release build

### Problem summary

When an image is drawn below 0.5× per axis, `CpuRenderer` box-filters the **whole** image to the device scale before sampling (`zpdf-render-cpu/src/lib.rs:1719-1760`) and caches the result in `downscaled_images` (64 MiB). That cache is cleared in both `begin_page` (`:2469`) and `end_page` (`:2640`). For tiled rendering, where each tile is its own `begin_page`, every tile at the same scale repeats the full-image downscale, even when the tile covers a few pixels of the image. Scanned documents viewed zoomed out pay this cost on every tile.

### Minimal reproduction

Scanned A4 page, one 2480×3508 gray JPEG, release build, best of 5 runs:

| scale | full page | one 32×32 px tile |
|---|---|---|
| 0.25 | 7.4 ms | 7.3 ms |
| 1.0 | 19.3 ms | 15.3 ms |

(Tile `page_rect` = a 32/scale pt square in the page's top-left corner; same display list, fonts and images.)

### Expected behavior

The cost of a tile scales with the tile, and repeated tiles of one page at one scale reuse the downscaled image.

### Additional context

**Possible fixes.**
- Keep `downscaled_images` across `begin_page` calls for the renderer's lifetime, bounded by its existing byte limit, ideally with LRU eviction.
- Or make it a shareable cache (like `ImageCache`) that callers can keep next to the display list.
- Or downscale only the source rectangle that maps into the current raster.

---

## 8. Accept shared byte buffers (e.g. `Arc<dyn AsRef<[u8]> + Send + Sync>`) to avoid copying whole files

- **Template**：Feature request；**Affected area**：PDF parsing and filters
- **zpdf version or commit**：`fe0ed23`

### Problem or use case

`PdfDocument::open(data: impl Into<Arc<[u8]>>)` (`zpdf-document/src/lib.rs:109`) and `PdfFile::parse` (`zpdf-parser/src/lib.rs:64`) store the file as `Arc<[u8]>` (`zpdf-parser/src/lib.rs:35`). Any other owner of the bytes must copy the whole file, and converting a `Vec<u8>` into `Arc<[u8]>` reallocates as well. That includes a memory map, `Arc<Vec<u8>>` and `bytes::Bytes`, as well as an application-level shared buffer that the viewer also uses for saving and re-opening. Measured: opening a 530 MB file from a `Vec<u8>` peaks at 1015 MB private memory.

### Proposed behavior

Accept a shared, read-only byte source, for example `Arc<dyn AsRef<[u8]> + Send + Sync>` (or a small `PdfBytes` type built from it), and keep `impl Into<Arc<[u8]>>` as a convenience. `memmap2::Mmap`, `Vec<u8>` and `bytes::Bytes` all implement `AsRef<[u8]>`. The parser only needs `&[u8]` slices internally, so the change is mostly at the storage boundary. It also opens the door to memory-mapped opening of very large files.

### Alternatives considered

`bytes::Bytes` (adds a dependency); a custom trait with `fn bytes(&self) -> &[u8]`.

### Additional context

FastPDF's documents already own their bytes as `Arc<dyn AsRef<[u8]> + Send + Sync>`, so the zpdf adapter has to copy every file once at open.

---

## 9. Make `PdfDocument` `Sync`

- **Template**：Feature request；**Affected area**：Document model and navigation
- **zpdf version or commit**：`fe0ed23`

### Problem or use case

`PdfFile` keeps its object cache, ObjStm cache and repair table in `RefCell` / `Cell` / `OnceCell` (`zpdf-parser/src/lib.rs:44-60`), and `PdfDocument` keeps `SharedFonts` in a `RefCell` (`zpdf-document/src/lib.rs:71`). Both types are therefore `!Sync`. An interactive viewer wants to interpret the visible page and prefetch its neighbours at the same time. Today it must either serialize all interpretation behind a mutex, or open the document once per thread. Measured on a 16-page CJK document at 150 dpi: with 8 per-thread documents, wall time dropped from 1024 ms to 295 ms, but peak private memory rose from 55 MB to 458 MB, because every copy has its own caches and its own system-font loads.

### Proposed behavior

Make the caches thread-safe (`Mutex` / `RwLock` / `OnceLock`, possibly sharded), or split an immutable `Sync` core (xref, object store, decoded ObjStms, shared fonts) from the per-interpretation state.

### Additional context

The display list, `FontCache`, `ImageCache` and `CpuRenderer` are already `Send + Sync`, which makes parallel rasterization easy. FastPDF interprets each page once under a mutex and rasterizes tiles from the shared display list on several threads. Only interpretation is serialized.

---

## 10. Cancellation and budget hooks for interpretation and rendering

- **Template**：Feature request；**Affected area**：Content interpretation and text extraction / CPU rendering
- **zpdf version or commit**：`fe0ed23`

### Problem or use case

An interactive viewer cancels work constantly, e.g. when the user scrolls past a page or changes the zoom. zpdf offers no way to stop an interpretation or a render that is already running:

- The interpreter's wall-clock budget is the private constant `INTERPRET_BUDGET` (8 s, `interpreter.rs:198`); the deadline has no public setter. Interpretation includes image decoding, so an adversarial page can block a worker for up to 8 s.
- When a budget trips, the page is silently truncated: `tracing::warn!` only, and `InterpretStats` has no "truncated" flag. A caller can only infer it from `total_ns` or the command count. `CpuRenderer::with_render_budget` likewise returns a partial page as success.
- Caches have admission limits only. There is no way to ask a document to drop caches under memory pressure.

### Proposed behavior

- `ContentInterpreter::with_interrupt(&'a AtomicBool)` (or `&'a dyn Fn() -> bool`), checked in `over_budget()`, which the top-level loop and the recursive form/pattern emitters already poll (`interpreter.rs:684-707`), and in long image or shading loops. The same hook for `CpuRenderer`, between commands and inside image and shading work.
- A public deadline / time-budget setter on the interpreter.
- Report truncation, e.g. `InterpretStats::truncated: Option<Reason>` and a `partial` flag on the rendered page.
- A cache-trimming entry point, e.g. `PdfDocument::trim_caches(level)`.

### Additional context

FastPDF runs the `execute` loop itself and checks its cancel token between display-list commands. Interpretation cannot be interrupted, and truncation is inferred from the stats.

---

## 11. README overstates the font cache policy and the CPU/GPU parity

- **Template**：Documentation；**Location**：`README.md` lines 10, 25, 125; `docs/CHANGELOG.md:392`
- **zpdf version or commit**：`fe0ed23`

### What is wrong or missing?

1. **Font cache.** The README and the changelog describe "font cache LRU eviction (256-font limit)". The code has no eviction. `FontCache::with_capacity` is deprecated and its documentation explains that evicting entries would invalidate display-list font IDs (`zpdf-font/src/lib.rs:1864-1872`). Only byte-based admission limits remain (`try_insert_with_limit`).
2. **CPU/GPU parity.** The README says the GPU renderer "matches the CPU renderer within <1% pixels". Measured with zpdf's own `compare` command at 150 dpi (RTX 5090, Vulkan), the share of pixels differing by more than 16/255 was:

   | Document | Differing pixels |
   |---|---|
   | Non-embedded CJK text | 20.3% (MAE 9.8) |
   | LaTeX Type1 text | 2.8% |
   | `pattern_shading_type2_many` | 8.0% (threshold 8/255) |
   | Real-world document with 152 images | 1.8% |
   | `coat_of_arms` vector art | 0.68% |

### Suggested correction

Describe the actual font cache policy (per-document shared fonts plus per-page admission limits, no eviction). Qualify the parity claim with the corpus and threshold behind it, or publish the comparison.

---

## 附錄：重現用 helper

所有 crate 都 pin 在 `fe0ed23f25fa0b8b75a7edcc14d5f881a9a7b16f`：`zpdf-core`、`zpdf-document`、`zpdf-content`、`zpdf-display-list`、`zpdf-font`、`zpdf-image`、`zpdf-render`、`zpdf-render-cpu`。

```rust
use zpdf_content::interpreter::ContentInterpreter;
use zpdf_core::Rect;
use zpdf_display_list::{Color, DisplayList};
use zpdf_document::{PdfDocument, page::PdfPage};
use zpdf_font::FontCache;
use zpdf_image::ImageCache;
use zpdf_render::{PageRenderInfo, RenderBackend};
use zpdf_render_cpu::{CpuRenderer, RenderedPage};

fn obj(s: &str) -> Vec<u8> {
    s.as_bytes().to_vec()
}

fn stream(dict: &str, data: &[u8]) -> Vec<u8> {
    let mut out = format!("<< {dict} /Length {} >>\nstream\n", data.len()).into_bytes();
    out.extend_from_slice(data);
    out.extend_from_slice(b"\nendstream");
    out
}

/// Minimal PDF writer: `objects[i]` becomes object `i + 1`; object 1 is the catalog.
fn pdf(objects: &[Vec<u8>]) -> Vec<u8> {
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (i, body) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes());
    for offset in offsets {
        out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objects.len() + 1)
            .as_bytes(),
    );
    out
}

/// One page without resources: `page` is spliced into the page dictionary.
fn one_page(page: &str, content: &[u8]) -> Vec<u8> {
    pdf(&[
        obj("<< /Type /Catalog /Pages 2 0 R >>"),
        obj("<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        obj(&format!("<< /Type /Page /Parent 2 0 R {page} /Contents 4 0 R >>")),
        stream("", content),
    ])
}

/// Interprets a page the way zpdf-cli does (`translate` adds the origin fix).
fn interpret(doc: &PdfDocument, page: &PdfPage, translate: bool) -> (DisplayList, FontCache, ImageCache) {
    let mut fonts = doc.load_page_fonts(page);
    let content = doc.page_content_bytes(page).unwrap();
    let annotations = doc.page_annotations(page);
    let mut images = ImageCache::new();
    let bbox = page.effective_box();
    let mut interpreter = ContentInterpreter::new(bbox).with_page_rotation(page.rotate);
    if translate {
        interpreter = interpreter.with_content_translation(-bbox.x0, -bbox.y0);
    }
    let dl = interpreter
        .with_fonts(&mut fonts)
        .with_document(doc.file(), &page.resources)
        .with_images(&mut images)
        .with_annotations(&annotations)
        .interpret(&content);
    (dl, fonts, images)
}

/// Renders the page-space rectangle `rect` of a display list at `scale`.
fn raster(dl: &DisplayList, fonts: &FontCache, images: &ImageCache, rect: Rect, scale: f32) -> RenderedPage {
    let mut renderer = CpuRenderer::new().with_fonts(fonts).with_images(images);
    renderer
        .begin_page(&PageRenderInfo { page_rect: rect, scale, background: Color::rgba(1.0, 1.0, 1.0, 1.0) })
        .unwrap();
    for command in &dl.commands {
        renderer.execute(command).unwrap();
    }
    renderer.end_page().unwrap()
}
```
