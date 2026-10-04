# Hayro Audit（M0）

- 審查對象：`D:\fastPDF\upstream\hayro`，HEAD `ced00dd0`（2026-10-03，「hayro-syntax: Detect reference cycles in object streams (#1389)」）。workspace 版本標示為 0.7.x（`hayro` 0.7.0、`hayro-syntax` 0.7.2），授權 `Apache-2.0 OR MIT`，MSRV 1.92。
- 對照：crates.io 上已發布的 `hayro 0.7.1`（git `1e5b8eb7`，2026-06-05）、`hayro-syntax 0.7.2`（git `34834627`，2026-05-28）；pdf-reader-gpui 目前使用 `hayro 0.5.0`。
- 環境：Windows 11 Pro、Ryzen 9 9950X、128 GB RAM、rustc/cargo 1.99.0 stable（高於 MSRV 1.92，無問題）。
- 方法：讀碼（下文以 `檔案:行號` 標示，路徑相對於 `upstream/hayro`）、`gh` 唯讀查 issue、在暫存資料夾寫實驗程式（path dependency 指向 upstream）實測。所有數字都是**粗略單次量測**；量測期間其他子 agent 同時在編譯，數字有雜訊（例如同一個 JPX 頁面在負載下量到 54–94 ms，負載降低後重量為約 30 ms）。
- 限制：沒有下載 hayro-tests 的 1,400+ 遠端測試 PDF（需從外部 R2 下載大量檔案），也沒有跑 upstream 的 render regression 測試；正確性只引用其方法與規模。studio-memory 中 fastPDF 專案沒有 approved memory 或 observation，本文件只依 repository 與實測。

---

## Summary

1. **Q6：建議保留 Hayro 作為 fallback／reference engine，而且它適合當 M1/M2 的第一個 engine adapter（信心：高，約 80%）。** 它是目前最完整的純 Rust PDF renderer 之一，所有 74 個 normal dependency 都是 permissive license，最小可執行檔約 5.8 MB，open／page size／render／encryption 都有可用 API，outline／links／text 可以在其上自建。但必須加上 guardrail 才能面對 hostile PDF（見 Risks）。
2. **版本**：pdf-reader-gpui 用的 0.5 → 目前 HEAD 之間有大量 breaking change（Device trait 改寫、`interpret()` 不再接受任意 iterator、`RenderCache`、vello_cpu 0.0.5 → 0.3）。**FastPDF 應 pin git rev（≥ `ced00dd0`），不要用 crates.io 的 0.7.1/0.7.2**：tile render 需要的 `render_into` 只在 HEAD（#1375，2026-09-25），多執行緒共用 `Pdf` 的 race／panic 修正也只在 HEAD（#1343，2026-10-03）。maintainer 表示下一版會是 minor bump（0.8）。
3. **Tile render 可行**：`hayro::render_into(page, cache, settings, render_settings, &mut RenderContext, Affine)` 接受任意 affine transform 與任意大小的 `RenderContext`，用 `translate(-tx,-ty) * scale * page.initial_transform(true)` 就能 render 任一 tile。實測 stitched 結果與整頁 render 幾乎完全一致（7 個檔案中 6 個 0 差異；向量檔有 136 px 曲線邊緣差異）。**代價**：每次呼叫都要重新 interpret 整頁內容（只有 rasterization 會依 viewport 裁切），所以應「一次 render 一整塊可見區域（tile batch）再切 tile」，而不是每個 tile 呼叫一次。1920×1080 viewport 在 1x–32x zoom 的成本幾乎固定（文字頁 1.3–3.4 ms、繁中頁 14–24 ms、JPX 頁約 30 ms）。
4. **Text extraction 可行（glyph 層級）**：自訂 `Device::draw_glyph_run` 可拿到每個 glyph 的 Unicode（ToUnicode → AGL glyph name → `uniXXXX`；CID 字型另有 UCS2 CMap fallback）與 transform。繁中測試頁 3,059 glyph 100% 有 Unicode，1.4 ms。缺：空白／斷詞／行偵測、ActualText、Type3 無 ToUnicode（#1331）、重疊假粗體去重、glyph 高度（ascent/descent）——都要在 adapter 自建。
5. **Threading**：`Pdf`、`Page`、`XRef` 是 `Send + Sync`；`RenderCache`／`InterpreterCache`／`Context` 是 `!Send`（`Rc<RefCell<…>>`），所以每個 worker thread 要有自己的 cache（字型會重複解析）。共用 `&Pdf` 平行 render 不同頁：64 頁 1x 由 27.9 ms（單執行緒）降到 11.3 ms（4 threads）、8.7 ms（8 threads）。vello_cpu 的 `multithreading` feature 未啟用；不需要，FastPDF 用自己的 worker pool 即可。
6. **Lazy loading**：`Pdf::new` 會完整讀 xref、解析**全部**頁面字典與 resources（不是 per-page lazy），但成本很低：2000 頁 4.1–11 ms；物件內容與 content stream 都是 lazy。800 MB 檔案若用 `Vec` 要先讀進記憶體（184 ms、+780 MB private），**用 `Arc<memmap2::Mmap>` 開檔只要 8.9 ms、working set 18 MB**。xref 壞掉時會全檔掃描（780 MB：0.35–0.74 s）。
7. **快取全部沒有上限**：decoded content stream（掛在 `Page` 上直到 `Pdf` drop）、decoded object stream、font cache、glyph outline cache、object cache；**沒有 decoded image cache**（每次 render／每個 tile 都重新解碼，JPX 頁約 30 ms 幾乎全是解碼）。
8. **健壯性**：近期修了很多 fuzz 問題，本機 364 個測試 PDF render 0 panic；但仍有**無法 catch 的 stack overflow**（10,000 層 `/Indexed` color space 鏈，HEAD 實測 process 直接中止）、**無 decompression／allocation 上限**（1 KB 級檔案可配置 1–2 GB）、**無運算預算／取消**（24 層 form XObject DAG、5.5 KB 檔案即 interpret 32.7 s，40 層等同無限）、超寬頁面 `RenderContext::new(65535, …)` panic。fuzz 只涵蓋 jpeg2000／jbig2／ccitt 三個 decoder，parser／interpreter／renderer 沒有 fuzz target。
9. **Q7 證據（hayro 端）**：PDF parse 不是瓶頸；主要成本是 (a) 文字以 path 方式逐 glyph 填色（沒有 glyph bitmap cache，繁中頁約 5 µs/glyph，佔 render 90%）、(b) 影像解碼（無快取、JPX 以外都全解析度解碼）、(c) 首次字型解析（LaTeX Type1 頁冷啟動 4.2 ms／6.5 ms）。純 rasterization（fine stage）在第 1 頁 1x 只有 0.06–1.4 ms（依頁面大小）。

---

## Architecture

### Crate 與責任

| crate | 非測試行數 | 責任 | 主要 dependency |
|---|---:|---|---|
| `hayro-syntax` 0.7.2 | 12,581 | PDF 語法層：xref（table/stream/hybrid、`Prev` 鏈、損壞時全檔重建）、物件解析（lazy、零拷貝 `Dict`/`Array`/`Stream` 借用原始資料）、object stream、filter（Flate、LZW、ASCII85/Hex、RunLength、DCT、JPX、JBIG2、CCITT）、加密（RC4/AES）、page tree、content stream tokenizer（typed／untyped iterator）、Info metadata | `flate2`(zlib-rs)、`zune-jpeg`、`hayro-jpeg2000`、`hayro-jbig2`、`hayro-ccitt`、`memchr`、`smallvec` |
| `hayro-interpret` 0.7.0 | 26,382（其中約 13.8k 行為產生的字型／編碼表） | 內容串流 interpreter：graphics state、色彩空間（ICC 透過 `moxcms`）、函式（Type 0/2/3/4）、shading/pattern、字型（Type1/TrueType/CFF/Type0/Type3，透過 `skrifa`）、文字定位、XObject、soft mask、optional content、annotation appearance stream；輸出到抽象的 `Device` trait | `skrifa`、`moxcms`、`hayro-cmap`、`kurbo`、`phf`、`yoke` |
| `hayro` 0.7.0 | 1,797 | `Device` 的 vello_cpu 實作（`Renderer`）：path/glyph/image/clip/mask/blend → `vello_cpu::RenderContext`；影像縮放（`pic-scale`）；`render()`／`render_into()` | `vello_cpu` 0.3、`pic-scale`、`fearless_simd`、`image`（看起來未被使用，`hayro/src/lib.rs:83` 的 `image::` 是本地 `mod image`） |
| `hayro-svg` 0.7.0 | 1,684 | 另一個 `Device` 實作：輸出 SVG 字串（FastPDF 不需要） | `xmlwriter`、`base64` |
| `hayro-jpeg2000` 0.4.0 | 9,553 | 純 Rust JPEG 2000 decoder（支援以目標解析度只解部分 resolution level） | `fearless_simd`、`moxcms`（image feature） |
| `hayro-jbig2` 0.3.0 | 8,755 | 純 Rust JBIG2 decoder（`MAX_DIMENSION = u16::MAX`，`hayro-jbig2/src/bitmap.rs:15`） | `hayro-ccitt`（MMR） |
| `hayro-ccitt` 0.3.0 | 1,057 | CCITT G3/G4 decoder（no_std） | — |
| `hayro-cmap` 0.1.0 | 1,936 | CMap 解析；內嵌 61 個預定義 CMap（brotli 壓縮約 250 KB，第一次使用時整包解壓並常駐，`hayro-cmap/src/bcmap/embedded.rs:8-43`） | `brotli`、`hayro-postscript` |
| `hayro-postscript` 0.1.0 | 980 | 給 CMap 用的 PostScript tokenizer 子集 | — |
| `hayro-write`、`hayro-demo`、`hayro-bench`、`hayro-tests`、`hayro-fuzz` | — | 重寫頁面（內部用）、WASM demo、benchmark、regression test、fuzz | — |

`unsafe`：除了 `hayro-syntax/src/page.rs:527`（把 `&XRef` transmute 成 `'static` 做 self-referential `CachedPages`）之外，所有 hayro 自己的 crate 在非測試程式碼中沒有 `unsafe`（`hayro`、`hayro-interpret`、decoder crates 都有 `#![forbid(unsafe_code)]`，例如 `hayro/src/lib.rs:33`、`hayro-interpret/src/lib.rs:28`）。但 `hayro-interpret` 無條件開啟 `hayro-syntax` 的 `unsafe` feature（`hayro-interpret/Cargo.toml`：`features = ["std", "images", "unsafe"]`），會引入 `flate2`/`zlib-rs`、`memchr`、zune-jpeg x86 SIMD 等內含 unsafe 的 dependency；`vello_cpu`、`fearless_simd`、`pic-scale` 也有 SIMD unsafe。

### 資料流

```text
bytes (Vec<u8> 或 Arc<T: AsRef<[u8]>>，例如 Arc<Mmap>)
  │  hayro_syntax::Pdf::new / new_with_password
  ▼
Pdf ── XRef（Arc；xref map 在 RwLock；decoded object stream 在 SegmentList）
  │      └ 解密器（Standard handler）
  └─ Pages（Vec<Page>，開檔時已全部建立）
        Page：media/crop box、rotation、Resources（7 個子字典）、lazy content stream（OnceLock）
  │
  │  hayro_interpret::interpret_page(page, &mut Context, &mut impl Device)
  │    Context：graphics state stack、bbox、InterpreterCache（font cache + object cache）
  │    逐一處理 TypedInstruction；Form XObject／pattern／soft mask／Type3 遞迴 interpret（深度上限 50）
  ▼
Device trait（draw_path / draw_rect / draw_glyph_run / draw_image / clip / transparency group / marked content）
  ├─ hayro::Renderer → vello_cpu::RenderContext（record：flatten + sparse strips，依 viewport culling）
  │                     └ render_with(PixmapMut, Resources, RasterizerSettings) → premultiplied RGBA8
  ├─ hayro_svg → SVG
  ├─ hayro_interpret::DummyDevice
  └─ FastPDF 自訂（text extraction、display list、計數／預算 wrapper…）
```

`Device` trait 定義在 `hayro-interpret/src/device.rs:7-48`：

```rust
pub trait Device<'a> {
    fn draw_path(&mut self, path: &BezPath, props: DrawProps<'a>, draw_mode: &DrawMode);
    fn push_clip_path(&mut self, clip_path: &ClipPath);
    fn push_clip_rect(&mut self, rect: &Rect) { /* default */ }
    fn push_transparency_group(&mut self, opacity: f32, mask: Option<SoftMask<'a>>, blend_mode: BlendMode);
    fn draw_glyph_run(&mut self, glyph_run: &GlyphRun<'_, 'a>, props: DrawProps<'a>, draw_mode: &DrawMode);
    fn draw_image(&mut self, image: Image<'a, '_>, props: ImageDrawProps<'a>);
    fn pop_clip(&mut self);
    fn pop_transparency_group(&mut self);
    fn draw_rect(&mut self, rect: &Rect, props: DrawProps<'a>, draw_mode: &DrawMode) { /* default */ }
    fn begin_marked_content(&mut self, _tag: &[u8], _mcid: Option<i32>) {}
    fn end_marked_content(&mut self) {}
}
```

設計上的重點：

- **interpret 與 rasterize 分離**：interpreter 只輸出 device 呼叫，影像是 lazy 物件（`RasterImage::with_rgba(callback, target_dimension)`，`hayro-interpret/src/types.rs:80-88`），由 device 決定要不要解碼；glyph outline 也是 lazy（`OutlineGlyph::outline()`）。所以 text extraction device 不會付出影像解碼與 outline 成本。
- **interpreter 不做 culling**：`Context` 的 bbox 只用來省略「完全覆蓋 bbox 的矩形 clip」與 `sh` 運算子的填色範圍（`hayro-interpret/src/context.rs:135-171`、`interpret/mod.rs` 的 `Shading` 分支），每次都會走完整個 content stream；culling 發生在 vello_common 的 flattener（`vello_common-0.3.0/src/flatten.rs:92-184`，viewport 外的線段被丟棄）。
- **Form XObject 每次 `Do` 都重新解碼 stream**（`hayro-interpret/src/x_object/form.rs:23-44`，`Stream::decoded()` 不快取，`hayro-syntax/src/object/stream.rs:159-166` 註明「calling it multiple times is expensive」）。
- 字型全部經 `skrifa`（TrueType/OpenType/CFF/Type1），自家的 `hayro-font` 已於 2026-04 移除（`9b06f93a` #1154、`357b0a8c` #1153）。

---

## Version Delta (0.5 → 0.7)

repository 沒有 CHANGELOG，以下由 git tags 與 diff 整理。

| 版本 | 日期 | git | 備註 |
|---|---|---|---|
| hayro 0.5.0 | 2026-01-08 | tag `hayro-v0.5.0` | pdf-reader-gpui 使用；vello_cpu 0.0.5、kurbo 0.12、skrifa 0.40、`hayro-font` |
| hayro 0.6.0 | 2026-04-12 | tag `hayro-v0.6.0` | 343 commits 之後；kurbo 0.13；新增 `RenderCache`／`InterpreterCache`；`interpret()` 改吃 `TypedIter`；`hayro-font` 改為 skrifa |
| hayro 0.7.0 | 2026-05-15 | tag `hayro-v0.7.0` | 再 8 commits；vello_cpu 0.0.8 |
| hayro 0.7.1（crates.io 最新） | 2026-06-05 | `1e5b8eb7` | 仍是舊版 Device API，沒有 `render_into` |
| HEAD | 2026-10-03 | `ced00dd0` | 0.7.1 之後 137 個 commits，大量 breaking change；maintainer 在 #1343 表示下一版是 minor bump（0.8） |

pdf-reader-gpui（0.5）移植到 HEAD 需要改的地方：

| 項目 | 0.5 | HEAD | commit／證據 |
|---|---|---|---|
| render 入口 | `hayro::render(page, &InterpreterSettings, &RenderSettings{x_scale, y_scale, width, height, bg_color}) -> Pixmap` | `render(page, &RenderCache, &InterpreterSettings, &RenderSettings{force_image_interpolation}, &PixmapSettings{x_scale, y_scale, bg_color})`；viewport 大小改用 `render_into(page, cache, is, rs, &mut RenderContext, Affine)` | `c9d699db` #1375；`hayro/src/lib.rs:195-304` |
| cache | 無 | `RenderCache<'a>`（0.6.0 起），invariant lifetime、`!Send` | `6e16bacf` #1164、`da31da96` #1165 |
| `Context::new` | `(transform, bbox, xref, settings)` | `(transform, bbox, &InterpreterCache, xref, settings)` | `hayro-interpret/src/context.rs:68-78` |
| `interpret()` | `ops: impl Iterator<Item = TypedInstruction>` | `ops: TypedIter<'_>`（具體型別）。pdf-reader-gpui 的 `extract_features` 用 `std::iter::from_fn` 包 iterator 追蹤目前 operator，**這招已不能用** | `b153a7b5` #1129（2026-03-28）；`interpret/mod.rs:231-236` |
| Device trait | `set_soft_mask`、`set_blend_mode`、`draw_path(path, transform, paint, PathDrawMode)`、`draw_glyph(glyph, transform, glyph_transform, paint, GlyphDrawMode)`、`draw_image(image, transform)`、`pop_clip_path` | 狀態併入 `DrawProps{transform, paint, soft_mask, blend_mode}`／`ImageDrawProps`；`draw_glyph_run(&GlyphRun, DrawProps, &DrawMode)`；`pop_clip`；新增 `push_clip_rect`、`draw_rect`、marked content；`DrawMode` 新增 `Invisible` | `d47a8f4f` #1245、`9c4890a0` #1324、#985、#967 |
| page transform | `hayro_interpret::PageExt::initial_transform` 回傳 `kurbo::Affine` | `Page::initial_transform(invert_y)` 是 hayro-syntax 的 inherent method，回傳 `hayro_syntax::transform::Transform`，用 `TransformExt::to_kurbo()` | `hayro-syntax/src/page.rs:371-410` |
| Pixmap | vello_cpu 0.0.5 `Pixmap::take()` → `Vec<Rgba8>` | vello_cpu 0.3：`data_as_u8_slice()`、`take_rgba8(ImageAlphaType)`；`RenderContext::flush()` + `render_with(target, &mut Resources, RasterizerSettings)`；premultiplied | `7708dcbc` #1385 |
| Unicode | `Glyph::as_unicode() -> Option<char>` | `-> Option<BfString>`（可多字元） | `hayro-interpret/src/font/mod.rs:102-107` |
| 其他新增 | — | `InterpreterSettings.cmap_resolver`（`embed-cmaps` feature）、`hayro-cmap`／`hayro-postscript` crate、`RenderSettings.force_image_interpolation`（#1388）、`DummyDevice`、object nesting limit、concurrency 修正 | |
| dependency | kurbo 0.12、skrifa 0.40、vello_cpu 0.0.5 | kurbo 0.13、skrifa 0.46、vello_cpu 0.3、fearless_simd 1.0；edition 2024、MSRV 1.92 | `Cargo.toml` |

結論：Device 實作（pdf-reader-gpui 的 `FeatureExtractor`）要整個重寫；render 呼叫端要改成持有 `RenderCache`；文字擷取改用 `draw_glyph_run` + `as_unicode()` 反而比 0.5 的「抓 `Tj` 原始 bytes」正確（0.5 的做法拿到的是未解碼的字元碼，不是 Unicode）。

---

## API Mapping to PdfEngine

對照 spec §6 草稿與 ADR 0002 的 `EngineDocument`。圖例：✅ 直接可用、🟡 需 adapter 自建或有缺口、❌ 不支援。

| FastPDF 操作 | hayro API（簽章） | 位置 | 狀態／缺口 |
|---|---|---|---|
| `open(DocumentSource)` | `Pdf::new(data: impl Into<PdfData>) -> Result<Pdf, LoadPdfError>`；`Pdf::new_with_password(data, password: &str)`；`PdfData: From<Vec<u8>>`、`From<Arc<T: AsRef<[u8]> + Send + Sync + 'static>>` | `hayro-syntax/src/pdf.rs:36-69`、`data.rs:12-51` | ✅ 可傳 `Arc<Mmap>` 零拷貝。錯誤只有 `Invalid` 與 `Decryption(MissingIDEntry / PasswordProtected / InvalidEncryption / UnsupportedAlgorithm)`（`crypto/mod.rs:37-47`），沒有細節訊息 |
| 密碼 | 同上；錯誤或空密碼回 `Decryption(PasswordProtected)`，UI 可據此要求輸入 | `xref.rs:1019-1035`、`crypto/mod.rs:109-200` | ✅ RC4 40/128、AES-128、AES-256（R5/R6）。🟡 R2–R4 只驗 user password（`crypto/mod.rs:162-175`），owner password 只在 R5/R6 有效；SASLprep 未實作（`crypto/mod.rs:604-607`）；只支援 `Standard` handler（`:116`）；**permissions（`/P`）有解析但沒有 API** |
| `page_count` | `pdf.pages().len()`（`Pages: Deref<Target = [Page]>`） | `page.rs:42-95` | ✅ O(1)（開檔時已建好） |
| `page_size`／`page_info` | `Page::media_box() -> Rect`、`crop_box()`、`intersected_crop_box()`、`rotation() -> Rotation`、`base_dimensions()`、`render_dimensions() -> (f32, f32)`（已含 /Rotate 交換寬高、零面積頁面 fallback A4）、`initial_transform(invert_y) -> Transform` | `page.rs:294-410` | ✅；🟡 `/UserUnit` 未處理（只有 key 定義，`object/dict.rs:980`）；Trim/Bleed/ArtBox 無 accessor，但可用 `Page::raw()` 自讀 |
| `render`（整頁、固定 scale） | `hayro::render(page: &'a Page<'a>, cache: &RenderCache<'a>, &InterpreterSettings, &RenderSettings, &PixmapSettings) -> Pixmap` | `hayro/src/lib.rs:230-266` | ✅ 但每次配置新的 `RenderContext` 與 `Pixmap`；寬高用 `(w * scale) as u16`（飽和轉換），≥ 65,533 px 會在 vello_common panic（見 Robustness） |
| `render`（任意 transform／tile／region） | `hayro::render_into(page, cache, &InterpreterSettings, &RenderSettings, ctx: &mut vello_cpu::RenderContext, transform: Affine)`，之後 `ctx.flush(); ctx.render_with(target: impl Into<PixmapMut>, &mut Resources, RasterizerSettings)` | `hayro/src/lib.rs:273-304`；`vello_cpu-0.3.0/src/render.rs:843-905` | ✅（僅 HEAD）。`PixmapMut::new(w, h, &mut [u8])` 可直接寫入呼叫端 buffer（buffer pooling）。🟡 每次呼叫都完整 interpret 整頁 |
| 輸出 pixel format | premultiplied RGBA8；`PixelFormat` 只有 `Rgba8` | `vello_common-0.3.0/src/pixmap.rs:20-32`、`vello_cpu-0.3.0/src/render.rs:96-101` | 🟡 GPUI 要 BGRA → adapter 自行 swizzle（白底不透明時 premultiplied 與 straight 相同） |
| 取消 | 無 | #1052（OPEN） | ❌ 見 Rendering Model「取消」 |
| `extract_text`／`text_layer` | 自訂 `Device::draw_glyph_run`；`Glyph::as_unicode() -> Option<BfString>`；`PositionedGlyph::transform() -> Affine`；`OutlineGlyph::advance_width() -> Option<f32>`、`font_data() -> Option<OutlineFontData>`；`DrawProps.transform` 為 CTM | `device.rs:7-48`、`font/mod.rs:102-216`、`font/outline.rs:13-28` | 🟡 glyph 層級可行，詳見 Text Extraction |
| `outline` | 無高階 API；`pdf.xref().root_id()`、`XRef::get::<Dict>(ObjectIdentifier)`、`Dict::get::<T>(key)`、`Dict::get_ref`、`Dict::obj_id`、`Array::iter::<T>()` | `xref.rs:410,514-520`、`object/dict.rs:66-120` | 🟡 adapter 自建（`/Outlines` First/Next 鏈、`/Dest`、`/A`、`/Dests` 與 `/Names` name tree、page ref → index 對照、PDFDocEncoding／UTF-16BE 字串解碼——hayro-syntax 沒有文字字串解碼 helper） |
| `links` | `page.raw().get::<Array>(b"Annots")` 自行解析 `/Subtype /Link`、`/A /URI`、`/Dest`、`/Rect`、`/QuadPoints` | `page.rs:352` | 🟡 adapter 自建；hayro 只會 render annotation appearance stream，不暴露 annotation 資料 |
| `metadata` | `pdf.metadata() -> &Metadata`（`title/author/subject/keywords/creator/producer: Option<Vec<u8>>`、建立／修改日期）、`pdf.version()`、`pdf.len()` | `pdf.rs:72-107`、`metadata.rs:7-43` | 🟡 字串是 raw bytes 需自行解碼；XMP 未解析；`/PageLabels` 未支援 |
| 非內嵌字型（含 CJK） | `InterpreterSettings.font_resolver: Arc<dyn Fn(&FontQuery) -> Option<(FontData, u32)> + Send + Sync>`；`FontQuery::Standard(StandardFont)`／`Fallback(FallbackFontQuery{post_script_name, font_family, weight, is_bold, is_italic, character_collection, …})` | `interpret/mod.rs:36,45-103`、`font/mod.rs:520-555`、`font/cid.rs:71-129` | ✅ 機制完整（`FontData = Arc<dyn AsRef<[u8]>>` 可傳 mmap 的系統字型、TTC index）。🟡 預設 resolver 把所有 fallback 對應到 14 個標準字型之一（`interpret/mod.rs:105-122`），**非內嵌的繁中字型會變成 Helvetica**；FastPDF 必須自己把 `AdobeCNS1` 等對應到 Windows 字型（例如 `msjh.ttc`、`mingliu.ttc`、`kaiu.ttf`） |
| warnings | `InterpreterSettings.warning_sink`（enum 有 `UnsupportedFont`、`ImageDecodeFailure`、`UnresolvedAnnotationAppearance`，但實際只有後兩者會被送出：`x_object/decode/mod.rs:51`、`interpret/mod.rs:213`） | `interpret/mod.rs:41,125-138` | 🟡 render 不回傳 `Result`；大部分錯誤以 `Option` 吞掉、只寫 log（`logging` feature） |
| `trim_memory` | 無 | — | 🟡 只能 drop `RenderCache` 並重開 `Pdf`（2000 頁重開約 5–10 ms） |

ADR 0002 的 `EngineCapabilities`：`region_render` ✅（HEAD）、`parallel_render` ✅（HEAD；每執行緒一份 cache）、`cooperative_cancel` ❌、`text_extraction` 🟡、`outline` 🟡（adapter 實作）、`links` 🟡（adapter 實作）、`encryption` ✅、`gpu` ❌。

---

## Lazy Loading

### `Pdf::new` 實際做的事

1. `find_version`：只看前 2,000 bytes（`pdf.rs:110-119`）。
2. `find_last_xref_pos`：從檔尾反向搜尋 `startxref`（`xref.rs:619-627`）。
3. `populate_xref_impl`：讀**所有** xref section（table、stream、hybrid `XRefStm`、整條 `Prev` 鏈，最多 256 段、有循環偵測，`xref.rs:723-767`），建立 `FxHashMap<ObjectIdentifier, EntryType>`。成本 O(物件數)，不讀物件本體。
4. `XRef::new`：解析 trailer、建立解密器、讀 catalog、檢查 `/OCProperties`、**立即解析 `/Info`**（`xref.rs:277-382`、`parse_metadata` `:1080-1107`）。
5. `CachedPages::new` → `resolve_pages`：**走完整個 page tree**，為每一頁建立 `Page`（讀 page dict、繼承的 MediaBox/CropBox/Rotate、`Resources::new` 會解析 7 個 resource 子字典），有 visited set 防循環（`page.rs:118-173,200-246,519-536`）。若 page tree 無法使用，退回 `new_brute_force`：走訪檔案中**所有物件**找 page dict（`page.rs:65-81`）。

真正 lazy 的部分：物件本體（`Dict` 在建立時只記錄 key offset，value 在 `get` 時才解析，`object/dict.rs:77-120`）、content stream（第一次 `page_stream()` 才解碼並以 `OnceLock` 快取，`page.rs:256-286`）、object stream（第一次需要其中物件時解碼並快取，`data.rs:93-112`）、字型、影像。

xref 指到錯誤 offset 時是**在 render 期間 lazy repair**：`XRef::get_with` 讀不到物件就呼叫 `repair()`，在持有 xref map **write lock** 的情況下全檔掃描（`xref.rs:470-483,565-579`）。也就是說某次 render 可能突然多花數百毫秒，且期間其他執行緒的物件查詢都會等待。

### 實測（`Pdf::new`，不含 render）

| 檔案 | 大小 | 讀檔 | `Pdf::new` | 記憶體 |
|---|---:|---:|---:|---|
| 合成 2000 頁、classic xref | 3.04 MB | 0.81 ms | 4.57 ms | 開檔後 working set +5.8 MB（約 2.9 KB/頁） |
| 合成 2000 頁、PDF 1.5 object stream + xref stream | 0.36 MB | 0.18 ms | 10.99 ms | +5.3 MB |
| 合成 2000 頁、每頁 400 KB 影像（780 MB），`Vec<u8>` | 780 MB | 183.8 ms | 4.12 ms | private 788 MB（整檔在記憶體） |
| 同上，`Arc<memmap2::Mmap>` | 780 MB | 0.08 ms | 8.85 ms | working set 18.3 MB、private 8.0 MB |
| 同上但 `startxref` 損壞（強制全檔重建），`Vec` | 780 MB | 188 ms | 353 ms | — |
| 同上，mmap | 780 MB | 0.07 ms | 743 ms | working set 790 MB（全檔被 page in，屬 file-backed） |
| 一般測試檔（0.06–2.2 MB） | — | <1 ms | 0.07–1.35 ms | — |
| AES-256 R6 加密（含密碼） | 0.06 MB | — | 7.68 ms | R6 key derivation |

2000 頁全部 content stream 解碼（合成小頁）只要 5.6 ms。用 mmap 依序 render 全部 2000 頁（1x）共 1,212 ms（0.6 ms/頁）。

### 對 800 MB／2000 頁的推估

- 正常 xref：`Pdf::new` 約 5–15 ms（隨物件數與 object stream 數量線性成長），**不隨檔案大小成長**；前提是用 mmap，否則光讀檔就要約 0.2 s（warm OS cache）到數秒（冷 cache／HDD），並佔用等量 RAM。
- 損壞 xref：全檔掃描，約 0.35–0.75 s／780 MB（約 1–2 GB/s），且 mmap 下會把整檔 page in。
- page tree 損壞：brute-force 會解析所有物件（含 object stream 解碼），可能到秒級。
- 開檔後常駐記憶體約 3 KB/頁 + xref map；但瀏覽過的頁面 content stream 會一直留在記憶體（見 Caches & Memory）。
- mmap 的注意事項：檔案被其他程式截斷會造成 access violation。Windows 上已 mapping 的檔案無法被截斷（`ERROR_USER_MAPPED_FILE`），但網路磁碟、可移除媒體仍有風險；upstream 也有人因「檔案可能在執行中被刪除」而另提 partial-read 方案（#1345，OPEN）。

---

## Rendering Model

### Pipeline

1. `render_into` 建立 `Context`（bbox = `(0,0,ctx.width,ctx.height)`），重設 `RenderContext` 的 state／mask／filter，push crop box clip（已套 transform），呼叫 `interpret_page`（`hayro/src/lib.rs:273-304`）。
2. `Renderer`（Device 實作）把每個 draw 轉成 vello_cpu 呼叫：路徑 → `fill_path`/`stroke_path`；glyph run → 每個 glyph 的 outline 以 `fill_path` 畫出（outline 快取在 `RenderCache`，`hayro/src/glyph.rs:50-63,123-146`）；影像 → 依目標尺寸解碼、必要時以 `pic-scale` 縮小、轉 premultiplied RGBA、包成 `Pixmap` 當 paint（`hayro/src/image.rs:191-500`）；soft mask → 另開一個與目前 context 同尺寸的 `RenderContext` 畫完轉 `Mask`（`hayro/src/mask.rs:60-115`）；tiling pattern → 另開子 context 畫一個 cell（上限 3000 px，`hayro/src/paint.rs:130`）；shading → 原生漸層或在 `bbox ∩ viewport` 範圍取樣成 texture（`paint.rs:38-52`）。
3. vello_cpu「record」階段（flatten + sparse strip，有 viewport culling）在 Device 呼叫當下就執行；`render_with` 才做 fine rasterization 與合成。
4. 輸出 premultiplied RGBA8；背景由 `RasterizerSettings.target_init` 決定（`render` 用 `PixmapSettings.bg_color`）。

### render 與 render_into 的設定

- `RenderSettings { force_image_interpolation: bool }`（`lib.rs:195-200`）
- `PixmapSettings { x_scale, y_scale, bg_color }`（`lib.rs:202-221`）
- `InterpreterSettings { font_resolver, cmap_resolver, warning_sink, render_annotations }`（`interpret/mod.rs:45-103`，`render_annotations` 預設 `true`；隱藏旗標只檢查 `Hidden` bit，`interpret/mod.rs:156`，沒檢查 `NoView`）
- `render_into` 的 `transform` 可以是任意 `kurbo::Affine`：scale、旋轉（使用者旋轉）、平移（tile offset）都在這裡組合。**一定要乘上 `page.initial_transform(true).to_kurbo()`**，它處理 y 軸翻轉、/Rotate 與 CropBox offset（`page.rs:371-410`）。

### Tile render 可行性

**做法 A：每個 tile 一次 `render_into`。**

```rust
let base = Affine::scale(scale) * page.initial_transform(true).to_kurbo();
ctx.reset_and_resize(tile_w, tile_h);
render_into(page, &cache, &is, &rs, &mut ctx, Affine::translate((-tx, -ty)) * base);
ctx.flush();
ctx.render_with(PixmapMut::new(tile_w, tile_h, &mut buf).unwrap(), &mut resources,
                RasterizerSettings { target_init: TargetInit::Clear(WHITE), ..Default::default() });
```

實測（page 1、4x、與同 scale 整頁 render 逐 pixel 比對；粗略單次）：

| 檔案 | 整頁 4x（interpret+record / raster） | 256 px tiles：數量／總時間／平均 | 512 px tiles：數量／總時間／平均 | 只 render 中央一個 tile（256 / 512） | 與整頁差異 |
|---|---|---|---|---|---|
| fonts_type1_latex（文字） | 2.53 / 4.56 ms | 130 / 30.0 ms / 0.23 ms | 35 / 10.7 ms / 0.31 ms | 0.62 / 0.75 ms | 0 px（最大 Δ1） |
| integration_coat_of_arms（17k 運算子向量圖） | 14.9 / 14.7 ms | 270 / 724 ms / 2.68 ms | 72 / 206 ms / 2.86 ms | 2.86 / 3.18 ms | 136 / 141 px，最大 Δ60（曲線邊緣，非 tile 邊界；推測為 culling flattener 的曲線細分差異） |
| animated-distributions（beamer） | 0.85 / 0.96 ms | 30 / 6.8 ms | 9 / 2.8 ms | 0.36 / 0.52 ms | 0 px |
| pattern_shading_type2_many（shading） | 4.80 / 0.66 ms | 10 / 44.6 ms | 3 / 13.8 ms | 4.62 / 4.96 ms | 0 px |
| image_cmyk_icc_jpg（CMYK JPEG + ICC） | 2.58 / 0.84 ms | 16 / 42.8 ms | 4 / 11.2 ms | 2.68 / 2.80 ms | 0 px |
| stream_jpx_6（9 張 JPEG 2000） | 28.4 / 7.7 ms | 108 / 2,866 ms | 30 / 744 ms | 24.7 / 24.9 ms | 0 px |
| pdftc_900k_0319_page_1（繁中 MingLiU） | 21.9 / 5.4 ms | 130 / 1,063 ms | 35 / 300 ms | 8.67 / 9.40 ms | 0 px |

結論：正確性沒問題（shading、soft mask、pattern、影像都會依 tile 重新計算；只有向量檔出現極少數 sub-pixel 差異，ADR 0003 的「tile 邊界 1 px 差異」驗收項目需保留）。但**每個 tile 都付一次整頁 interpretation 與影像解碼**：單 tile 成本 ≈ 整頁 interpret 成本（JPX 頁每個 tile 24.7 ms），tile 數一多就失控。

**做法 B（建議 v1）：以「可見區域」為單位 render，再切成 tile。** scheduler 把同一頁、同一 scale bucket、相鄰的 P0 tile 合併成一個矩形，用一個 `RenderContext`（例如 viewport 大小）一次 `render_into`，再把結果切進各 tile buffer。實測 1920×1080 區域置中、best of 3：

| 檔案 | 1x | 2x | 4x | 8x | 16x | 32x（整頁約當 MP） |
|---|---:|---:|---:|---:|---:|---:|
| fonts_type1_latex | 2.50 ms | 3.39 | 2.79 | 2.31 | 1.64 | 1.30（496 MP） |
| pdftc_900k_0319_page_1（繁中） | 23.8 ms | 23.8 | 21.6 | 16.3 | 14.7 | 13.9（496 MP） |
| integration_coat_of_arms | 11.2 ms | 10.1 | 7.98 | 6.50 | 5.77 | 5.78（1,043 MP） |
| stream_jpx_6（負載較低時重量） | 35.5 ms | 31.4 | 30.8 | 31.2 | 32.0 | 30.2（398 MP） |
| image_cmyk_icc_jpg | 3.52 ms | 3.31 | 3.67 | 4.34 | 4.63 | 4.53（41 MP） |

viewport 成本幾乎不隨 zoom 成長（甚至因 culling 變少），符合 spec §12「不要因為 zoom 到 600% 就 rasterize 整頁」。32x 時整頁會是 0.4–1 GPixel，`hayro::render` 根本做不到（u16 上限與記憶體），但區域 render 只要數毫秒。

**做法 C（後續最佳化）：自訂 display-list Device。** interpret 一次、把 path／glyph run／影像（解碼後快取）記錄在 page 座標，之後每個 tile 依 bbox 篩選後 replay 到 vello_cpu。這需要 fork `hayro` crate 的 `Renderer`（約 1.8k 行、Apache/MIT），並處理 `SoftMask<'a>`／`Paint<'a>` 帶 `'a` lifetime 的問題（display list 只能活在持有 `Pdf` 的 worker 內）。可讓 tile 成本與可見內容成正比，並一併解決影像重複解碼。

不可行的做法：vello_cpu 0.3 的 `RasterizerSettings.offset` 是 `(u16, u16)`，只能把 scene 往右下移，無法從一個 record 好的整頁 scene 中切出任意 tile（`vello_cpu-0.3.0/src/render.rs:106-125,808-842`）；且整頁 `RenderContext` 受 u16（≤ 65,532 px）限制，高 zoom 的 strip 記憶體也會很大。

### 尺寸限制

- `RenderContext`／`Pixmap` 寬高是 `u16`。`RenderContext::new(65535, h)` 會在 `vello_common::util::snap_to_tile_coordinates` 的 `unwrap()` panic（`vello_common-0.3.0/src/util.rs:237`，實測 70,000 pt 寬頁面 1x render 觸發）。`hayro::render` 用飽和 `as u16`（`lib.rs:238-241`），所以任何 scale 後 ≥ 65,533 px 的頁面都會 panic；例如 A0（2,384 pt）在 28x。FastPDF 用 tile／region 就不會碰到，但 adapter 仍要在呼叫前檢查 region 尺寸（ADR 0002 的 `GuardedDocument` 已規劃）。
- 影像：解碼後若寬或高 > 65,535 且顯示 scale ≥ 1，`Pixmap::from_parts(data, w as u16, …)` 長度不符會 panic（`hayro/src/image.rs:467-475`，`vello_common-0.3.0/src/pixmap.rs:108-150`）。DCT 有 `u16::MAX` 檢查（`hayro-syntax/src/filter/dct.rs:16-18`），JBIG2 也有，但 Flate 原始影像沒有。
- 影像只有 JPX 會依目標解析度部分解碼（`hayro-syntax/src/filter/jpx.rs:27`）；JPEG、Flate、CCITT、JBIG2 一律全解析度解碼再縮小，縮圖成本不會隨縮圖尺寸下降。

### 取消

沒有任何取消機制（#1052 OPEN，maintainer 認為「放到另一個 thread、太久就放棄」才是正解，但 Rust 無法安全終止 thread）。`interpret()` 從 0.6 起只接受具體型別 `TypedIter`（`b153a7b5`），不能再用自訂 iterator 提早結束。可用的折衷：

1. 每次 `render_into` 前後檢查 `CancelToken`（ADR 0002 已規劃）；`render_into` 完成後、`render_with` 前再檢查一次，可省掉 rasterization。
2. 包一層 Device wrapper：token 觸發後所有 draw 變 no-op。interpreter 仍會跑完 content stream 與字型解析，但影像解碼（發生在 device 端 `with_rgba`）、glyph outline 與 vello record 都會跳過，對影像頁很有效。
3. 向 upstream 提 PR：在 `InterpreterSettings` 加 `should_stop: Arc<dyn Fn() -> bool>`，於每個 operator 與每次巢狀 interpret 檢查。改動小，能同時解決 CPU DoS（見 Robustness）。
4. 不受信任的檔案用 process isolation（render 子 process，可 kill）。

### vello_cpu 多執行緒

hayro 啟用的 vello_cpu feature 是 `std, png, u8_pipeline`（`Cargo.toml` workspace dependencies），沒有 `multithreading`，`render` 與子 context 一律 `num_threads: 0`（`lib.rs:343-348`）。若 FastPDF（或任何 dependency）開啟 vello_cpu 的 `multithreading` feature（cargo feature 會統一），`RenderSettings::default()` 會變成 `num_threads = min(核心數 - 1, 8)`（`vello_cpu-0.3.0/src/render.rs:178-192`），而且每個 `RenderContext::new` 都會建立新的 rayon `ThreadPool`（`dispatch/multi_threaded.rs:117`）——`hayro::render` 每次呼叫都會建 thread pool。建議：不開 `multithreading`，平行度由 FastPDF 自己的 worker pool（spec §18：2–8 workers）提供；若要開，只能搭配 `render_into` 與重複使用的 `RenderContext`。

---

## Text Extraction Feasibility

可行，而且 upstream 的定位就是「只提供每個 glyph 的 Unicode 與位置，word／line 偵測不在範圍內」（maintainer 在 #452 的回覆）。官方範例：`hayro-interpret/examples/extract_html.rs`。

可取得的資訊（在 `draw_glyph_run(&GlyphRun, DrawProps, &DrawMode)` 中）：

- `glyph_run.glyphs()` → `&[PositionedGlyph]`，依 content stream 順序。
- `PositionedGlyph::transform()`：glyph 空間（1000 units/em）→ text space 的完整矩陣（含 font size、`Tz`、`Ts` rise、`Tm`）；乘上 `props.transform`（CTM）即得 Context 初始座標系中的位置。adapter 可以自己選 `Context::new` 的初始 transform，例如直接用 ADR 0002 的 page space（CropBox 左上原點、y 向下、未旋轉）。
- `Glyph::as_unicode() -> Option<BfString>`（`font/mod.rs:102-107`）：Type1/TrueType/CFF 依序用 ToUnicode → 編碼 glyph name 經 AGL → `uniXXXX`/`uXXXX`；Type0（CID）用 ToUnicode，非內嵌且沒有 ToUnicode 時改用 `Adobe-*-UCS2` CMap（`font/cid.rs:115-129`）；Type3 只有 ToUnicode。
- `OutlineGlyph::advance_width()`（1000 units/em 的字寬）、`font_data()`（raw 字型 bytes、PostScript name、weight、italic…），可用來估 glyph box。
- `DrawMode::Invisible`：`Tr 3` 的隱形文字（OCR 掃描 PDF 的文字層）仍會送進 device（`interpret/text.rs:115-119`）。
- `begin_marked_content(tag, mcid)`：可得 MCID（tagged PDF 結構）。

實測（自訂 TextExtractor device，冷 cache）：

| 檔案 | glyph | 有 Unicode | 時間 | 觀察 |
|---|---:|---:|---:|---|
| pdftc_900k_0319_page_1（繁中 MingLiU） | 3,059 | 3,059（100%） | 1.37 ms | 1,949 個 CJK 字；有重疊兩次的假粗體（「授權合約」重複）；`™` 被對到 U+0099 |
| font_vertical（直書繁中） | 92 | 92 | 0.69 ms | 有 PUA 字元（U+E78D…） |
| font_cid_2（簡中） | 65 | 65 | 0.56 ms | |
| fonts_type1_latex | 901 | 901 | 3.71 ms | TeX 不輸出空白 glyph，需依間距插入空白 |
| password_encrypted_aes_256 | 349 | 349 | 0.87 ms | |

本機 273 個可開啟的 custom 測試 PDF 全部掃過一次（每檔最多 50 頁），只有上述 3 個含 CJK。

缺口（adapter 要自己做）：

1. 空白與斷詞：依 glyph 間距、advance width 與字級推斷；行與段落：依 baseline 與方向分群；閱讀順序：不保證（content stream 順序）。
2. 去重：假粗體（同位置重畫）、陰影字。
3. glyph 高度／選取框：沒有 ascent/descent API，可用 em box 估計，或從 `font_data()` 以 skrifa 讀 `hhea`/`OS/2`（會多一個 dependency）。
4. `ActualText`（`/Span <</ActualText …>> BDC`）只拿得到 tag 與 MCID，拿不到 property dict 內容。
5. Type3 沒有 ToUnicode 時沒有 Unicode，字元碼也是 `pub(crate)`（#1331 OPEN）；TeX 特殊 glyph name 對應不完整（#1369 OPEN）。
6. `Tr 7`（只裁切）的文字不會送進 device（`interpret/text.rs:130`）。
7. 成本：text extraction 仍會解析字型（與 render 共用 `InterpreterCache` 就不必重複），但不會解碼影像或產生 outline，適合在背景 search worker 跑（spec §22）。

---

## Threading

型別的 `Send`/`Sync`（實驗程式以 autoref specialization 在 HEAD 上實測；`std::thread::scope` 共用 `&Pdf` 也能編譯）：

| 型別 | Send | Sync | 原因 |
|---|---|---|---|
| `Pdf`、`Page<'_>`、`Pages<'_>`、`XRef`、`PdfData` | ✅ | ✅ | `Arc` + `RwLock`/`Mutex`/`OnceLock`（`hayro-syntax/src/sync.rs:17-39`） |
| `InterpreterSettings` | ✅ | ✅ | resolver 都是 `Arc<dyn Fn + Send + Sync>` |
| `InterpreterCache<'_>`、`RenderCache<'_>`、`Context<'_>` | ❌ | ❌ | `font_cache: Rc<RefCell<…>>`（`context.rs:28-31`）、`outline_cache: Rc<RefCell<…>>`（`lib.rs:180-183`） |
| `vello_cpu::RenderContext` | ✅ | ❌ | |
| `vello_cpu::Pixmap` | ✅ | ✅ | |

- `RenderCache<'a>` 的 lifetime 綁在 `Page<'a>`／`Pdf` 的借用上且是 invariant（`render_into<'a>(page: &'a Page<'a>, cache: &RenderCache<'a>, …)`）。實務設計：每個 worker thread 在自己的 stack frame 裡持有 `Arc<Pdf>` 的借用與 `RenderCache`，迴圈接收 job；不需要 self-referential struct。換文件或 trim memory 時離開迴圈、drop cache。
- 平行 render 不同頁（共用 `&Pdf`、每執行緒各自 `RenderCache`，含 thread spawn）：

| 文件 | 1 thread | 2 | 4 | 8 |
|---|---:|---:|---:|---:|
| 合成 2000 頁文字，前 64 頁 @1x | 27.9 ms | 17.2 | 11.3 | 8.7 |
| 合成 2000 頁 + 每頁 400 KB 影像（mmap），前 64 頁 | 48.7 ms | 26.8 | 14.1 | 13.3 |

  4 threads 以後報酬遞減，與 spec §18（2–8 workers、latency 優先）一致。代價：每個 worker 各自解析一份字型、outline cache 也各一份。
- 鎖：每次物件查詢都會取 xref map 的 `RwLock` read lock（`xref.rs:537-541`）；object stream 解碼用 `Mutex` 保護 map（`data.rs:93-112`）；`InterpreterCache.object_cache` 是 `Arc<Mutex<…>>`（`cache.rs:8-49`），但因整個 cache `!Send`，實際上不會跨執行緒爭用。lazy repair 時持有 write lock 全檔掃描（見 Lazy Loading）。鎖都是 `.lock().unwrap()`（`sync.rs:64-91`）：**某個執行緒在持鎖時 panic 會讓鎖 poisoned，之後所有執行緒存取同一份 `Pdf` 都會 panic**，所以 catch 到 panic 後應丟棄該 `Pdf` 重開（ADR 0002 的 `is_degraded()` 方向正確）。
- **concurrency 修正只在 HEAD**：#1343（CLOSED 2026-10）回報共用 `Pdf` 時 object stream 物件會無聲變成 null、xref repair 會 panic。已發布的 `hayro-syntax 0.7.2` 程式碼確實有 race：`get_with` 先插入 map 再初始化 slot（讀者看到未初始化的 slot 回 `None`），`repair` 用 `try_put().unwrap()` 與 `assert!(!locked.repaired)`、`get_with` 用 `try_get().unwrap()`（0.7.2 的 `src/xref.rs:485-486,544`）。HEAD 已改為解碼後在鎖內發布（`data.rs:93-112`、commits `0829d581`、`d6fa1180`、`41d11844`）。我用「字型字典放在 render 時才解碼的 object stream」的合成檔（2000 頁 × 8 threads × 5 輪）在 HEAD 與 0.7.1 上都比對 0 差異——race window 很小，這個合成檔沒有觸發，但程式碼層面的 race 明確存在，回報者表示其實際檔案幾乎每次都出錯。

---

## Caches & Memory

| 快取 | 位置 | key | 上限 | 生命週期 | 備註 |
|---|---|---|---|---|---|
| decoded content stream | `Page.page_streams: OnceLock<Option<Vec<u8>>>`（`page.rs:194,256-286`） | 每頁一份 | **無** | 直到 `Pdf` drop | 多個 content stream 會串接複製；解碼時峰值約 2 倍 |
| decoded object stream | `Data.decoded: SegmentList<Vec<u8>>` + `Mutex<FxHashMap>`（`data.rs:64-112`） | object stream id | **無** | 直到 `Pdf` drop | |
| xref map | `RwLock<MapRepr>`（`xref.rs:642-675`） | ObjectIdentifier | O(物件數) | `Pdf` | |
| page 結構 | `Vec<Page>`（開檔建立） | — | O(頁數) | `Pdf` | 實測約 2.9 KB/頁 |
| font cache | `InterpreterCache.font_cache: Rc<RefCell<FxHashMap<u128, Option<Font>>>>`（`context.rs:28-31,347-370`） | font dict bytes 的 128-bit hash | **無** | `RenderCache` | 每執行緒一份 |
| object cache | `Cache(Arc<Mutex<FxHashMap<u128, Option<Box<dyn Any>>>>>)`（`cache.rs:8-49`） | 物件 bytes hash | **無** | `RenderCache` | 色彩空間、ICC transform、shading、函式等 |
| glyph outline cache | `RenderCache.outline_cache: Rc<RefCell<FxHashMap<u128, Rc<BezPath>>>>`（`lib.rs:180-193`、`glyph.rs:50-63`） | (glyph id, font) | **無** | `RenderCache` | 未縮放的 outline，不是 bitmap |
| soft mask | `Renderer.soft_mask_cache`（`lib.rs:93`、`mask.rs:9-27`） | mask hash | 單次 render | 單次 render | 每個 mask = 整個 context 大小的 8-bit 圖 |
| decoded image | **無** | — | — | — | 每次 draw 都解碼（`types.rs:80-88`、`image.rs:619-628`） |
| 內嵌 CMap bundle | `LazyLock<Bundle>`（`hayro-cmap/src/bcmap/embedded.rs:8-43`） | — | 固定 | process 生命週期 | 第一次用預定義 CMap 時整包解壓 |
| 標準字型（Foxit）、glyph list、AFM metrics | `include_bytes!`／產生的程式碼 | — | 固定 | binary | 約 240 KB 字型 |

實測記憶體行為：

- 小檔開檔後 working set 約 4–7 MB；render 第 1 頁 1x + 2x 後約 7–16 MB（峰值 8–27 MB，含 2x pixmap）。
- 1 GB flate bomb 的 content stream 在 `page_stream()` 之後 **private 1,027 MB 常駐**（峰值 2,086 MB），直到 drop `Pdf`。
- 依序 render 780 MB 文件全部 2000 頁（mmap）：峰值 working set 795 MB——主要是被 page in 的 file-backed 頁面，不是 private heap。

對 spec §15–§16（每個 cache 都要有 budget）的影響：hayro 內部快取沒有任何容量控制與觀測介面。adapter 能做的：

1. `trim_memory`／memory pressure：drop 該 worker 的 `RenderCache`；需要釋放 content stream／object stream 快取時，丟棄 `Pdf` 重新開檔（2000 頁約 5–10 ms，用 mmap 不必重新讀檔）。
2. 以每文件、每 worker 的「最近 N 頁」為單位週期性回收 `RenderCache`。
3. decoded image cache 與 glyph bitmap cache 要由 FastPDF 自己做（fork `Renderer` 或 display-list device），才能套上 spec §15 的 64 MB／32 MB budget。

---

## Robustness & Security

### 現有 guardrail（有效）

| 項目 | 位置 | 實測 |
|---|---|---|
| 物件巢狀深度上限 64（array/dict skip） | `hayro-syntax/src/object/mod.rs:18`（#1386，2026-10-02） | 10 萬層巢狀 array：該頁被丟棄、不 crash |
| 間接物件循環偵測（parent chain） | `object/indirect.rs:29-34`；object stream 版本 `xref.rs:585-591`（#1389，HEAD） | 自我引用的 form XObject 被擋下 |
| xref `Prev` 鏈上限 256 + 循環偵測 | `xref.rs:729-750` | |
| page tree visited set | `page.rs:124-147` | page tree 循環：正常 1 頁 |
| interpreter 巢狀深度上限 50（form/pattern/soft mask/Type3） | `hayro-interpret/src/context.rs:21,332-342` | |
| CMap 巢狀上限 16、漸層細分上限 10 | `hayro-cmap/src/lib.rs:356`、`gradient.rs:13` | |
| JPEG 尺寸 ≤ u16::MAX、JBIG2 尺寸與 symbol 數上限、JPX 多項上限 | `filter/dct.rs:16-18`、`hayro-jbig2/src/bitmap.rs:15`、`decode/symbol.rs:25`、`hayro-jpeg2000/src/j2c/codestream.rs:12-14` | |
| tiling pattern cell ≤ 3000 px | `hayro/src/paint.rs:130` | |
| 加密 `/Length` 越界修正 | #1271 | |

### 缺少的 guardrail（spec §25 對照）

| spec §25 項目 | hayro 狀態 | 證據 |
|---|---|---|
| decompression bomb | **無上限**：Flate 用 `read_to_end`（`filter/lzw_flate.rs:14-38`），filter 可串接 | 1.0 MB 檔 → 1 GB content stream，2.27 s、峰值 2.09 GB、1 GB 常駐；1.4 KB 檔（Flate×2）→ 512 MB，峰值 1.06 GB |
| giant bitmap／max decoded image dimension | **無**：CCITT 依 `/Columns × /Rows` 直接 `vec![0xFF; n]`（`filter/ccitt.rs:26,150`）；Flate 原始影像只受實際資料量限制 | 757 B 的 CCITT 宣告 40000×40000 → 一次配置 1.6×10⁹ bytes（峰值 working set 1.53 GB）、1.48 s；1.1 MB 的 20000×20000 RGB → 峰值 2.06 GB、2.21 s |
| giant page dimension | **無**：頁面大小不設限；`render` 以飽和 u16 配置整頁 Pixmap | 14,400 pt 頁面 1x → 829 MB pixmap、433 ms；≥ 65,533 px → vello_common panic |
| max recursion／nesting（跨物件） | **部分**：單一物件內有上限；但由多個間接物件串成的鏈（function、色彩空間）沒有深度上限 | **10,000 層 `/Indexed` 色彩空間鏈（688 KB）→ `thread 'main' has overflowed its stack`，process 中止（HEAD 實測）**；100、1,000 層正常。#1347 最後一則留言也指出 Type 3 function／`/Indexed`／`/Separation`／ICC `/Alternate` 鏈仍會 stack overflow，HEAD 只修了 object stream 循環部分 |
| CPU 運算預算／timeout | **無**：深度上限 50 擋不住扇出的 DAG | form XObject DAG（每層畫下一層 2 次）：16 層 0.17 s、20 層 2.4 s、24 層（5.5 KB）**32.7 s**（純 interpret、沒有 rasterize）、40 層 > 60 s 被 timeout 中止 |
| max object length | 無明確上限；`Length` 錯誤時 fallback 往後搜尋 `endstream`（`object/stream.rs:299-334`） | |

### Panic 政策

- 非測試程式碼的 `unwrap()`／`expect()`／`panic!`-類：`hayro` 7／0／4、`hayro-syntax` 51／1／10、`hayro-interpret` 10／0／3、`hayro-jpeg2000` 21／0／7（另有 11 個 `assert!`）、`hayro-jbig2` 3／0／3、`hayro-cmap` 8／0／1、`hayro-ccitt` 1／0／1（實驗腳本去掉 `#[cfg(test)]` 後計數）。另外大量 slice indexing 也可能 panic。
- upstream issue 有大量外部 fuzzer 回報的 panic（多數已修，例如 #52、#156、#388、#391、#506、#577、#585、#1258、#1261、#1273），仍 OPEN 的包括 vello 端的 #717（`Max. number of lines per path exceeded`）、#646、#373，以及 #404（大 xStep/yStep pattern）、#1259（memory allocation failure）。
- 本機 364 個測試 PDF（85 個 load 回歸檔 + 279 個 custom，每檔最多 render 50 頁）：297 個成功 render、63 個開檔失敗（56 個是刻意損壞的 `Invalid`、6 個需要密碼、1 個 `InvalidEncryption`）、4 個 0 頁；**0 panic、0 crash**，最大 working set 43 MB。
- 結論：FastPDF 必須 `panic = "unwind"` 並在每次 engine 呼叫外包 `catch_unwind`（ADR 0002 已規定）；panic 後丟棄 `Pdf`（鎖可能 poisoned）。stack overflow 與 allocation failure（`alloc` 失敗會 abort）**無法用 `catch_unwind` 攔截**：
  - render worker 用 `std::thread::Builder::stack_size` 開大 stack（例如 32–64 MB）可把 10,000 層鏈的門檻往上推，但不能根治；
  - 開檔前或 render 前不容易預先掃描這類鏈；
  - 真正的防線是 upstream 修正（#1347 留言中已附 patch 方向：object stream 成員加入 parent chain + function／color space 建構深度上限 32）或 process isolation。

### unsafe 與 fuzz

- hayro 自己的 `unsafe` 只有 `hayro-syntax/src/page.rs:527` 一處（`CachedPages` 的 self-referential `'static` transmute，依賴 `Arc<XRef>` 位址穩定與欄位 drop 順序）；其餘在 dependency（zlib-rs、memchr、zune-jpeg、vello_cpu、fearless_simd、pic-scale）。
- `hayro-fuzz` 只有 3 個 target：`fuzz_ccitt`、`fuzz_jbig2`、`fuzz_jpeg2000`（`hayro-fuzz/Cargo.toml`），且 JPX target 跳過 > 2500×2500 的影像以免 timeout。**`hayro-syntax`（parser）、`hayro-interpret`、`hayro`（renderer）、`hayro-cmap` 都沒有 fuzz target**；這些模組的問題目前靠外部 fuzzer（例如 qarmin/Automated-Fuzzer）回報。
- CI 只跑 `cargo test -p hayro-tests -- "load::"`（`.github/workflows/ci.yml`），render regression 不在 CI。

---

## Correctness & Test Corpus

- 規模：manifest 共 1,599 筆——custom 298（273 筆在 repo 內）、pdf.js 679、PDFBox 454、PDFium（Chromium issue tracker）127、PDF Association large-scale corpus 41；另有 85 個 load 回歸 PDF 與 31 個 codec 檔（.jp2/.jb2）。測試函式約為 render 1,525、load 124、svg 39（以 `grep -c "fn "` 粗估 `hayro-tests/tests/*.rs`）。repo 內實際 PDF 共 365 個（custom 279、load 85、other 1，約 24 MB）。
- 方法：**reference image 是 hayro 自己在 main 上產生的 baseline snapshot**，之後以 pixel diff > 0 判定失敗（`hayro-tests/tests/mod.rs:46-138`、README）。它是 regression test，不是對照 pdf.js／PDFium／Acrobat 的 ground truth；正確性的依據是「曾人工檢視過的 baseline」。
- 下載：`hayro-tests/sync.py` 從 `https://hayro-assets.dev/{custom,pdfjs,pdfbox,pdfium,corpus}/` 下載 PDF，並從 `https://hayro-assets.dev/fonts/` 下載 NotoSansCJK 等測試字型到 `downloads/`、`assets/`（都在 `.gitignore`）。
- 授權：這些 PDF 來自各專案 issue tracker 的附件與 CommonCrawl 衍生的 corpus，**個別檔案的著作權與授權未標示**。FastPDF 不可 commit，只能本機引用：建議在 `fixtures/external/hayro-tests/` 之類 gitignored 目錄執行 sync.py（或直接指向 upstream clone 內的 `downloads/`），benchmark／比較腳本以路徑設定引用。repo 內 `hayro-tests/pdfs/` 的 365 個檔案同樣沒有逐檔授權說明，也視為「只能本機引用」。
- 已知不支援（程式碼證據）：knockout group、非內嵌 CID 字型需外部字型（預設會退回標準字型）、外部檔案 stream（`/F`，`object/stream.rs:224-228`）、`/UserUnit`、Type3 的 clip 文字模式（`interpret/text.rs:154`）。`hayro/src/lib.rs:16-19` 與 hayro-syntax README 仍寫「不支援加密」，但程式碼與 render 測試（`render.rs:258-288`）顯示已支援——文件已過時。

---

## Build & Measurements

### Build

- 實驗程式：`scratchpad/agent-hayro/probe`（path dependency `hayro = { path = "D:/fastPDF/upstream/hayro/hayro", features = ["logging"] }` + `memmap2`、`log`），`cargo build --release` 成功（首次約 4.5 分鐘，含其他 agent 的編譯負載）。HEAD 在 rustc 1.99.0 編譯無錯誤。
- 最小可執行檔（只開檔 + render 第 1 頁，`lto = "fat"`、`codegen-units = 1`、`strip = true`）：**5.76 MB**（含內嵌字型與 CMap）。

### 量測方法

release build；每個數字是單次執行（viewport 為 best of 3）；計時用 `Instant`，記憶體用 `K32GetProcessMemoryInfo`（working set／peak／private）。量測同時有其他 cargo build 在跑。

### 第 1 頁結果

| 檔案（來源：`hayro-tests/pdfs/custom`） | 類別 | 大小 | `Pdf::new` | 1x cold／warm | 2x cold／warm | 1x interpret+record／raster | 純 interpret（no-op device） |
|---|---|---:|---:|---|---|---|---:|
| fonts_type1_latex | 純文字（LaTeX Type1） | 0.74 MB | 0.57 ms | 6.52／1.83 ms（612×792） | 7.64／2.95 ms | 1.44／0.32 ms | 4.2 ms（冷字型） |
| integration_coat_of_arms | 向量（17,201 個運算子） | 0.20 MB | 0.08 ms | 9.99／8.70 ms（917×1109） | 14.8／14.4 ms | 6.81／1.41 ms | 3.0 ms |
| pattern_shading_type2_many | shading | 0.53 MB | 1.35 ms | 7.20／4.94 ms | 6.44／5.16 ms | 4.73／0.14 ms | — |
| image_cmyk_icc_jpg | 影像（CMYK JPEG + ICC） | 2.23 MB | 1.18 ms | 10.2／2.86 ms（200×200） | 9.87／2.96 ms | 2.62／0.06 ms | — |
| stream_jpx_6 | 影像（9 張 JPEG 2000） | 1.61 MB | 0.09 ms | 33.0／31.6 ms（540×720） | 32.2／31.6 ms | 29.6／1.33 ms | 1.2 ms |
| image_jbig2_crash | 掃描（JBIG2） | 0.20 MB | 0.29 ms | 7.93／7.74 ms | 9.43／9.24 ms | 6.74／0.34 ms | — |
| image_ccit_4 | 掃描（CCITT） | 0.03 MB | 0.09 ms | 3.92／3.80 ms | 3.81／3.78 ms | 2.85／0.10 ms | — |
| pdftc_900k_0319_page_1 | 繁中（MingLiU，3,059 glyph） | 0.64 MB | 0.78 ms | 23.0／16.9 ms（612×792） | 24.8／19.5 ms | 15.9／0.48 ms | 1.5 ms |
| password_encrypted_aes_256（密碼 testpw） | 加密 AES-256 R6 | 0.06 MB | 7.68 ms | 2.75／1.54 ms | 3.41／2.59 ms | — | — |
| password_encrypted_rc4_40（密碼 testpw） | 加密 RC4 40-bit | 0.06 MB | 0.29 ms | 2.60／1.51 ms | 3.20／2.73 ms | — | — |

輸出 PNG 在暫存資料夾 `scratchpad/agent-hayro/out/`（`*_p1_1x.png`、`*_p1_2x.png`、`*_p1_4x_tiled512.png`），未放進 repo。

### 對 Q7 的解讀（hayro 端）

| 階段 | 證據 | 判斷 |
|---|---|---|
| PDF parse／開檔 | 2000 頁 4–11 ms；一般檔 < 1.5 ms；純 interpret（content stream + 字型對應）0.4–4.2 ms/頁 | 不是瓶頸；例外是 xref 損壞（O(檔案大小)）與 page tree 損壞 |
| fonts | LaTeX 頁冷 render 6.5 ms 中約 4.2 ms 是字型解析；warm 1.8 ms | 首次顯示有感；cache 是 per-thread，worker 越多重複解析越多 |
| image decode | JPX 頁 28–30 ms 中幾乎全是解碼，且每次 render、每個 tile 都重新解碼；JBIG2 約 7 ms | 影像頁的主要瓶頸；需要 decoded image cache 與依目標尺寸解碼 |
| rasterization（record） | 繁中頁 1x：interpret+record 15.9 ms，其中純 interpret 只有 1.5 ms → 約 14 ms 花在逐 glyph `fill_path`（約 5 µs/glyph，沒有 glyph bitmap cache） | 文字密集（尤其 CJK）頁的主要瓶頸 |
| rasterization（fine） | 第 1 頁 1x 0.06–1.4 ms、4x 0.7–15 ms（依頁面大小與內容） | 與面積成正比；tile／viewport render 後很小 |

### hayro-bench

`hayro-bench` 有三個 binary（README 註明仍是 WIP）：

- `render_bench`：對一個目錄下所有 PDF，以 hayro 與 PDFium（`pdfium-render 0.9`，`Pdfium::bind_to_system_library()`，需系統上有 pdfium 動態庫）各自「開檔 + render 全部頁面（scale 1、白底）」，`--iter N` 取平均，輸出表格與「hayro vs pdfium」百分比；`--save-bitmaps` 存 PNG。用法：`cargo run --release -p hayro-bench --bin render_bench -- <input-dir> [--backend pdfium|hayro|all] [--iter N] [--save-bitmaps]`。
- `hayro_syntax`：對 `hayro-tests/downloads` 與 `pdfs/custom` 量「`Pdf::new` + 走過所有頁面的 typed operator」與「只開檔」，列出最慢的 200 個。
- `jpx_decode_bench`：量某個 PDF 內 JPX 影像的解碼時間。

它量的是整份文件 wall time，沒有 first-page latency、peak RSS、tile／viewport、text extraction，不符合 spec §26 的需求；FastPDF 的 `fastpdf-bench` 可參考其 PDFium 對照做法，但需自建。

---

## License & Dependencies

- hayro 全部 crate：`Apache-2.0 OR MIT`。
- `cargo metadata --format-version 1 --filter-platform x86_64-pc-windows-msvc` 加 Python 統計，並以 `cargo tree -e normal` 校正（metadata 的 resolve 圖會列出只經 weak feature 出現、實際未啟用的 `glifo`）：`hayro`（default features）可達的 normal dependency 共 **74 個 crate（含 hayro 本身），全部為 permissive**，**沒有 GPL／AGPL／LGPL／MPL**。

| License（SPDX） | crate 數 |
|---|---:|
| MIT OR Apache-2.0（含寫法變體） | 45 |
| MIT | 6（`phf*`、`simd-adler32`、`synstructure`） |
| Unicode-3.0 | 4（`yoke`、`yoke-derive`、`zerofrom`、`zerofrom-derive`） |
| BSD-3-Clause OR Apache-2.0 | 3（`moxcms`、`pic-scale`、`pxfm`） |
| BSD-3-Clause | 2（`alloc-no-stdlib`、`alloc-stdlib`） |
| BSD-3-Clause AND MIT／BSD-3-Clause/MIT | 2（`brotli`、`brotli-decompressor`） |
| Zlib | 2（`foldhash`、`zlib-rs`） |
| MIT OR Apache-2.0 OR Zlib（含寫法變體） | 6（`miniz_oxide`×2、`zune-core`、`zune-jpeg`、`bytemuck`、`bytemuck_derive`） |
| Unlicense OR MIT | 2（`memchr`、`byteorder-lite`） |
| 0BSD OR MIT OR Apache-2.0 | 1（`adler2`） |
| (MIT OR Apache-2.0) AND Unicode-3.0 | 1（`unicode-ident`，proc-macro 用） |

  重複版本：`fearless_simd` 0.7.0（vello_common）與 1.0.0（hayro）、`miniz_oxide` 0.8.9 與 0.9.1、`syn` 2 與 3（後者只在編譯期）。`image` crate 在 `hayro` 中看起來未使用。

Attribution 要求（`THIRD_PARTY_LICENSES.md` 需要涵蓋）：

1. **hayro 的 NOTICE.md**（Apache-2.0 §4(d) 要求散布 Derivative Works 時保留 NOTICE 內容）：部分程式碼改寫自 **PDFBox**（標準字型編碼表、Type 0 函式 evaluator、常用 dictionary key 列表）、**pdf.js**（CalRGB/CalGray 轉換、flate decoder、AES/MD5/SHA/RC4 實作）、**png crate**（PNG predictor 解碼），三者皆 Apache-2.0。FastPDF 若只以 dependency 方式使用，選 MIT 授權即可，但保守起見仍把 NOTICE 內容放進 `THIRD_PARTY_LICENSES.md`。
2. **內嵌進 binary 的資料**（default features 會打包）：Foxit 標準字型（`hayro-interpret/assets/LICENSE_FOXIT`，BSD-3-Clause「Copyright 2014 PDFium Authors」，binary 散布需附版權聲明）、Adobe 預定義 CMap（`hayro-cmap/assets/LICENSE.txt`，BSD-3-Clause「Copyright 1990-2023 Adobe」）、Adobe Glyph List（`assets/glyphlist/glyphlist.txt`，BSD-3-Clause）、PDFBox 補充 glyph list（Apache-2.0）、Adobe Core 14 AFM metrics（`assets/font_metrics/MustRead.html`：可自由使用但須保留版權聲明；binary 內是由 AFM 產生的 `font/generated/metrics.rs`，保守起見附上該聲明）、CGATS ICC profile（CC0）。
3. BSD-3-Clause（brotli 相關、moxcms/pic-scale/pxfm 的 BSD 選項）、Zlib、Unicode-3.0 的版權聲明。
4. hayro-tests 用的 Liberation 字型（SIL OFL）與 NotoSansCJK 只在測試用，不進 FastPDF binary。

---

## Risks

| # | 風險 | 嚴重度 | 證據 | 緩解 |
|---|---|---|---|---|
| R1 | 無法攔截的 stack overflow（跨物件的 function／color space 鏈） | 高 | 10,000 層 `/Indexed` 鏈 HEAD 實測 process 中止；#1347 | worker 大 stack；向 upstream 送 patch（#1347 已有方向）；不受信任檔案的 process isolation 列入 roadmap |
| R2 | 無 decompression／allocation 上限，小檔可配置數 GB；OOM 時 abort | 高 | flate bomb、CCITT 40000²、20000² 影像實測 1–2 GB | 向 upstream 提 `ResourceLimits`（max decoded stream bytes、max image pixels）；adapter 層無法預先攔截，需 fork 或 PR |
| R3 | 無取消、無運算預算，hostile 檔可長時間占用 worker | 高 | form DAG 24 層 32.7 s、40 層實質無限；#1052 | Device no-op wrapper + render 前後檢查 token；upstream PR（`should_stop` callback）；watchdog 將卡住的 worker 標記並另開 worker |
| R4 | 必須跟 git main：crates.io 0.7.x 缺 `render_into` 與 concurrency 修正；main API 仍在變（0.8 會有 breaking change） | 中 | #1343、#1375；0.7.2 原始碼的 race／`try_put().unwrap()` | pin 精確 git rev；所有 hayro 型別封裝在 `fastpdf-engine-hayro`；升級時跑 fixture 與 benchmark |
| R5 | 快取無上限、無觀測；content stream 常駐到 `Pdf` drop | 中 | `page.rs:194`、bomb 實測 1 GB 常駐 | 週期性 drop `RenderCache`、重開 `Pdf`；自建 image／glyph cache 才能套 budget |
| R6 | tile 成本 = 整頁 interpretation；影像每次重新解碼 | 中 | JPX 頁每 tile 24.7 ms、全頁 tiles 2.9 s | 以可見區域 batch render；中期做 display-list device + decoded image cache |
| R7 | CJK 非內嵌字型預設變成 Helvetica | 中（台灣場景高） | `interpret/mod.rs:105-122`、`font/cid.rs:71-129` | 實作 Windows 字型 resolver（CNS1→微軟正黑體／細明體、GB1、Japan1、Korea1；TTC index） |
| R8 | 文字擷取只有 glyph 層級；Type3／ActualText 缺口 | 中 | #452、#1331、#1369 | adapter 自建 word/line、去重；必要時 upstream PR 暴露 char code |
| R9 | 超大頁面／影像寬高超過 u16 時 panic | 中（可 catch） | 70,000 pt 頁面實測 panic（`vello_common util.rs:237`）；`image.rs:467` | adapter 驗證 region 尺寸；只用 region render；catch_unwind |
| R10 | panic 後鎖 poisoned，整份文件不能再用 | 低–中 | `sync.rs:64-91` | panic 後重開 `Pdf`（ADR 0002 `is_degraded`） |
| R11 | 測試 corpus 為自我 baseline、parser/interpreter 無 fuzz、CI 不跑 render 測試 | 中 | `tests/mod.rs:46-138`、`hayro-fuzz`、`ci.yml` | FastPDF 自建 parser/interpreter fuzz（cargo-fuzz 於 WSL/Linux CI），以 PDFium/zpdf 交叉比對 |
| R12 | 單一主要維護者、PR 審查量大 | 低–中 | #1345 maintainer 表示 PR 積壓；commit 作者幾乎都是 LaurenzV | upstream PR 小而獨立；必要時短期 fork 個別 crate |

---

## Verdict

### Q6：是否有必要保留 Hayro fallback？

**是，建議保留（信心：高，約 80%）。** 理由：

1. 用途對應 spec §3：compatibility comparison、renderer fallback、correctness comparison、regression testing——hayro 的 coverage 來自 pdf.js／PDFBox／PDFium／PDFA corpus 共約 1,600 筆回歸測試，是純 Rust 生態中最廣的；本機 364 個測試檔 0 panic。
2. 成本低：純 Rust、單一 toolchain、Windows 原生編譯沒有問題；74 個 dependency 全部 permissive；最小 binary 約 5.8 MB；adapter 可以 feature-gate（`--features engine-hayro`）。
3. API 足以支撐 ADR 0002 的 `EngineDocument`：open（含密碼）、page_count／page_info、region render、glyph 層級文字、outline／links（自建於 hayro-syntax）。
4. 有足夠的平行度（`Pdf: Send + Sync`）與可接受的效能（第 1 頁 1x 冷 render 約 1–33 ms、2000 頁開檔 < 11 ms）。

保留的前提：fallback 路徑同樣要套上 Risks R1–R3、R9、R10 的緩解，否則「zpdf 失敗 → 改用 hayro」反而可能把 hostile 檔交給一個會 stack overflow 或無限運算的 engine。

### Hayro 適不適合當 M1／M2 的第一個 engine adapter？

**適合（信心：高，約 80%），而且是目前最務實的選擇**：pdf-reader-gpui 已經用 hayro，M2 的工作本質是把 hayro 隔離到 `fastpdf-engine-hayro`；`render_into` 讓 M5 的 tile／region render 可以直接在這個 adapter 上驗證 scheduler 設計。條件與建議做法：

1. **版本**：pin `hayro`／`hayro-interpret`／`hayro-syntax` 到同一個 git rev（≥ `ced00dd0`），不用 crates.io 0.7.x；0.8 發布後再升級。
2. **Worker 模型**：每個 render worker 一個 OS thread（大 stack），在 thread 內持有 `Arc<Pdf>`（mmap）與自己的 `RenderCache`，處理 `RenderRequest`；同一份 `Pdf` 可被多個 worker 共用。
3. **Render**：一律走 `render_into` + 呼叫端 `PixmapMut` buffer；scheduler 把相鄰可見 tile 合併成一次 region render 再切 tile；輸出前 RGBA→BGRA swizzle；region 尺寸先驗證（< 65,533 px，ADR 0002 `ResourceLimits`）。
4. **Guardrail**：`catch_unwind` + panic 後重開 `Pdf`；Device wrapper 實作取消與 draw-call 計數預算；`warning_sink` 接到 tracing。
5. **文字／outline／links**：text layer 用自訂 Device（與 render 共用字型 cache 的 worker 上執行）；outline、links、page labels、metadata 字串解碼在 adapter 內以 hayro-syntax 自建（估計數百行）。
6. **字型**：實作 Windows 系統字型 resolver（尤其 Adobe-CNS1），測試繁中非內嵌字型的政府文件。
7. **M1 benchmark 要量**：open（Vec vs mmap）、first page、region render（1x/2x/4x/8x）、image-heavy 頁、CJK 頁、text extraction、peak RSS，並保留 hostile fixture（bomb、DAG、鏈）作為 guardrail 回歸測試。
8. **建議送 upstream 的小 PR**（降低 fork 需求）：(a) function／color space 建構深度上限（#1347 已有 patch 方向）；(b) `InterpreterSettings` 的取消／預算 callback（#1052）；(c) decoded stream 與影像像素上限；(d) 暴露 char code（#1331）。

---

### 附錄：重現方式（暫存資料，未放進 repo）

暫存資料夾：audit session 的 scratchpad（`agent-hayro\`），不在 repo 內

- `probe/`：實驗程式（modes：`full`、`open-only`、`text-only`、`render-all`、`viewport`、`par-verify`、`count`、`ops`、`probe-types`；環境變數 `PROBE_MMAP=1`、`PROBE_PAGES=N`、`PROBE_LOG=1`、`PROBE_DIFF_DUMP=1`）。例：`hayro-probe.exe <pdf> <out_dir> <password> full`。
- `probe071/`：同樣的平行 render 驗證，改用 crates.io `hayro = "=0.7.1"`。
- `gen_big_pdf.py`、`gen_fontstm.py`：合成 2000 頁 PDF（classic 780 MB、object stream、壞 xref、字型在 lazy object stream）。
- `gen_hostile.py`、`gen_chain.py`、`gen_wide.py`：hostile PDF（flate bomb、巨大影像、CCITT、form DAG、色彩空間鏈、超寬頁面等）。
- `lic-check/`：license 統計（`meta.json`、`tree_normal.txt`、`lic.py`）與最小 binary 大小量測。
- `run_*.txt`：各次量測的原始輸出。
