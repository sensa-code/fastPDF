# hayro upstream issue 草稿

對象：[LaurenzV/hayro](https://github.com/LaurenzV/hayro)，commit `ced00dd0`（`ced00dd082e6a7eda8561d4ac0f7fc3828af2ac7`，2026-10-03，workspace 0.7.x，vello_cpu 0.3.0）。
來源：M0 audit（`docs/audit/hayro.md`）、Hayro adapter 實作（`crates/fastpdf-engine-hayro/`）、M4 比對（`docs/engine-comparison.md`）、B-5 記憶體 benchmark（`docs/benchmarks/b5-memory.md`）。

**#1 與 #7 已經有修正 patch**（`patches/hayro-0001-*.patch`、`patches/hayro-0002-*.patch`，本地分支 `fastpdf/transparency-fixes`，base `ced00dd0`），可以直接送 PR 而不只是開 issue。各 issue 的〈Proposed fix〉小節說明改了什麼，驗證與送 PR 前的步驟見〈修正 patch：驗證與送 PR 前的步驟〉。FastPDF 本身仍 pin 在 `ced00dd0`，沒有用這些 patch。

證據：

- 每個重現步驟都在 Windows 11 Pro 10.0.26200 x86_64、rustc 1.99.0 上，對 `ced00dd0` 的 upstream clone（path dependency、預設 features：`embed-fonts`、`embed-cmaps`、`simd`）實際執行過，數字照抄輸出。
- PDF 用附錄 A 的 Python（只用標準函式庫）產生；render 用附錄 B 的 Rust harness（只用 hayro 公開 API，在 8 MiB stack 的 thread 上執行，等同 Linux main thread 的預設值）。
- 尖峰記憶體是 Windows 的 `PeakWorkingSet64`／`PeakPagedMemorySize64`，每 20 ms 取樣。
- #2 的加密檔用 pypdf 6.19.0 產生（附錄 C）。
- #9 的每執行緒記憶體來自 FastPDF adapter 的 process private bytes 量測（B-5），不是 hayro 單獨的量測。

**本文件只是草稿：沒有在 GitHub 建立任何 issue 或 PR，沒有 fork、沒有 push，也沒有做任何寫入。要不要送出由使用者決定。** 送出 issue 前請注意（送 PR 另見〈送 PR 前的步驟〉）：

- 先在最新的 `main` 重跑重現步驟。hayro 更新很快（例如 #1386、#1387、#1389 都是 10/2–10/3 合併的），有些問題可能已經修掉。
- 先搜尋既有 issue。下表的「相關 issue」是 2026-10-04 用 `gh issue list --search` 唯讀查到的結果。
- hayro 沒有 issue template，也沒有 AI policy；但 maintainer 對大量 AI 產生的內容態度保留（參考 #1195），建議用自己的話精簡改寫英文內容，一個 issue 只放一個問題，重現用的小 PDF 直接附在 issue。
- 每個 issue 都寫著 helpers 在 issue 結尾：送出時把附錄 A（Python helper）、附錄 B（Rust harness；#8 另加 resolver wrapper）貼在該 issue 最後，#2 改貼附錄 C；或直接附上產生好的 PDF。

## 索引

| # | 標題 | 類型 | 嚴重度 | 相關 issue | FastPDF 的處理 |
|---|---|---|---|---|---|
| 1 | Alpha soft masks whose group has no `/CS` are dropped: the masked content is painted fully opaque | Bug | 高 | — | **patch 0001**（未送出） |
| 2 | The owner password is rejected for revision 2–4 security handlers (RC4, AES-128) | Bug | 中高 | — | 無 |
| 3 | Stack overflow on long chains of indirect color spaces (follow-up to #1347) | Bug（robustness） | 高 | #1347 最後一則留言 | adapter 靜態掃描擋下 + 64 MiB thread stack |
| 4 | No limit on decoded stream size or on up-front image allocations | Feature（security） | 高 | #1259、#273、#1382 | adapter 預先做 bounded inflate、宣告尺寸檢查 |
| 5 | Data point for #1052: a 6 KB Form XObject DAG takes over 30 s to render — please consider a work budget too | 留言（#1052） | 中高 | #1052 | adapter preflight 有時間預算的 interpretation |
| 6 | `render()` panics when the page is 65,533 px or more wide or tall | Bug | 中 | — | adapter 把 target 限制在 16,384 px |
| 7 | Knockout transparency groups (`/K true`) are composited like normal groups | Feature | 中 | README 已列為未支援 | **patch 0002**（未送出；部分支援，見該節） |
| 8 | Non-embedded CJK fonts silently fall back to Helvetica, so the text disappears | Docs／Feature | 中高（CJK 使用者） | README 已列為未支援 | adapter 的 font resolver 依 character collection 對應 Windows 字型 |
| 9 | Tiled rendering: images are decoded again on every `render_into` call, and `RenderCache` has no size bound | Feature（效能／記憶體） | 中 | #1375 | adapter：block 合併、decode budget、閒置 5 s 釋放執行緒 |

第 1、7 點對應 M4 比對中 `transparency/softmask-groups.pdf` 的差異（Hayro 把「Alpha soft mask (group ca 0.4)」畫成不透明方塊、knockout group 當成一般 group）。追查後發現第 1 點的原因不是 alpha mask 本身，而是 mask group 沒有 `/CS`；FastPDF fixture 的產生方式見 `tools/fixtures/fxlib/cat_vector.py` 的 `tr_softmask_groups`（`uv run tools/fixtures/generate.py --only transparency --out <暫存目錄>`；不加 `--out` 會覆寫共用的 `fixtures/generated/manifest.json`）。issue 裡改用下面的最小重現檔。

---

## 1. Alpha soft masks whose group has no `/CS` are dropped: the masked content is painted fully opaque

- **Type**：Bug
- **Version**：`ced00dd0`；Windows 11 x86_64, rustc 1.99.0, default features

~~~markdown
### Summary

`SoftMask::new` (`hayro-interpret/src/x_object/soft_mask.rs:89-92`) requires the
transparency group dictionary of the mask's `/G` to have a `/CS` entry, for both mask
types, and returns `None` otherwise. ISO 32000 only requires `/CS` for luminosity masks
(the group attributes of a mask's `G` must have `CS` only when `S` is `Luminosity`,
Table 144); an alpha mask only uses the group's alpha, so producers commonly omit it.

When `SoftMask::new` returns `None` the mask is silently dropped and the content it
should mask is painted without it: a 40% translucent shape becomes an opaque rectangle.
No warning reaches `warning_sink`.

### Reproduction

Helpers (`pdf()`, `page()`, `stream()`, render harness): at the end of this issue.

```python
objs = page(b"/SM gs 0 0.3 0.8 rg 0 0 200 200 re f",
            b"<< /ExtGState << /SM << /SMask << /Type /Mask /S /Alpha /G 5 0 R >> >> >> >>")
group = b"<< /S /Transparency >>"    # add /CS /DeviceRGB to get the correct result
objs[5] = stream(b"/Type /XObject /Subtype /Form /BBox [0 0 200 200] /Group " + group +
                 b" /Resources << /ExtGState << /GA << /ca 0.4 >> >> >>",
                 b"0 g /GA gs 50 50 100 100 re f")
open("smask-alpha-no-cs.pdf", "wb").write(pdf(objs))
```

```
cargo run --release -- smask-alpha-no-cs.pdf 1 100,100 20,20
```

### Expected

The blue square is visible only where the mask group painted (the 100x100 center),
at 40% opacity: pixel (100,100) is about (153, 184, 235), pixel (20,20) is white.
This is what hayro renders when the group has `/CS /DeviceRGB`:

```
pixel 100,100 = [153, 184, 234, 255]
pixel 20,20 = [255, 255, 255, 255]
```

### Actual

```
200x200 in 425.2µs, non-white pixels: 40000
pixel 100,100 = [0, 77, 204, 255]
pixel 20,20 = [0, 77, 204, 255]
```

### Possible fix

Only resolve `/CS` for `/S /Luminosity`. For a luminosity mask without `/CS`, falling
back to a device color space seems better than dropping the mask. A warning when a
soft mask is dropped would make such cases visible.
~~~

### Proposed fix（patch 0001）

- **Patch**：`patches/hayro-0001-keep-soft-masks-whose-group-has-no-color-space.patch`（本地分支的 commit `da45b370`，base `ced00dd0`）。和 0002 互相獨立，可以單獨套用、單獨送 PR。
- **改了什麼**：`hayro-interpret/src/x_object/soft_mask.rs`（+21／−5 行）。soft mask 的 group 沒有 `/CS`、`/CS` 無法解析，或連 `/Group` 字典都沒有時，mask 不再被丟掉。色彩空間只用來解讀 luminosity mask 的 `/BC`：沒有 `/CS` 時依 `/BC` 的元件數推定 DeviceGray／DeviceRGB／DeviceCMYK，推定不了就沿用原本的黑色 backdrop。
- **測試**：新增 `hayro-tests/pdfs/custom/mask_no_cs.pdf`（render 與 SVG 兩種 snapshot，manifest＋`tests/render.rs`＋`tests/svg.rs`）。左半是 alpha mask（group 沒有 `/CS`，畫一個 ca 0.5 的圓）→ 半透明的藍色圓；右半是 luminosity mask（group 沒有 `/CS`、`/BC [1]`，畫一個黑圓）→ 紅色方塊中間一個圓洞。修正前兩邊都是不透明方塊。
- **hayro 測試**：見〈修正 patch：驗證與送 PR 前的步驟〉。既有 snapshot 沒有任何像素差異。
- **FastPDF 驗證**：和 0002 一起驗證，見〈修正 patch：驗證與送 PR 前的步驟〉。`transparency/softmask-groups.pdf` 的 alpha soft mask 從不透明方塊變成 40% 的藍色圓盤，和 zpdf 一致。

PR 描述草稿（最後一段請在跑完完整測試後改成實際結果）：

~~~markdown
**hayro-interpret: Keep soft masks whose group has no color space**

A soft mask was dropped when the transparency group in its `G` entry had no `CS`
entry (or one that could not be parsed), so the content it should mask was painted
fully opaque. The specification only requires `CS` for the group of a luminosity
mask, and alpha masks commonly leave it out.

The color space is now optional. It is only used for the backdrop color `BC` of
luminosity masks; without one, `BC` is interpreted in the device color space with
the same number of components.

Test: `mask_no_cs` (render and SVG). Left: an alpha mask whose group paints a circle
with opacity 0.5. Right: a luminosity mask with `/BC [1]` whose group paints a black
circle. Before: two opaque squares. After: a half-transparent blue disc and a red
square with a round hole.

No existing snapshot changed in the custom, load, SVG and write tests.
<!-- Replace with the result of the full suite (sync.py) before submitting. -->
~~~

---

## 2. The owner password is rejected for revision 2–4 security handlers (RC4, AES-128)

- **Type**：Bug
- **Version**：`ced00dd0`
- 備註：`docs/engine-comparison.md` 提到的「R2–R4 只接受 user password」就是這一點。

~~~markdown
### Summary

For revisions 2-4, `decryption_key` (`hayro-syntax/src/crypto/mod.rs:164-177`)
derives the file key from the supplied password with Algorithm 2 and then only checks
it as a *user* password (Algorithm 6). Algorithm 7 (authenticating the owner password)
is not implemented, so opening an RC4 or AES-128 document with its owner password fails
with `DecryptionError::PasswordProtected`. Revisions 5 and 6 accept both passwords.

### Reproduction

Documents with user password `user` and owner password `owner`, generated with pypdf
(script at the end of this issue), opened with `Pdf::new_with_password(data, password)`:

| algorithm | V/R | `"user"` | `"owner"` | `"wrong"` |
|---|---|---|---|---|
| RC4-40 | 1/2 | Ok(1) | Err(Decryption(PasswordProtected)) | Err(Decryption(PasswordProtected)) |
| RC4-128 | 2/3 | Ok(1) | Err(Decryption(PasswordProtected)) | Err(Decryption(PasswordProtected)) |
| AES-128 | 4/4 | Ok(1) | Err(Decryption(PasswordProtected)) | Err(Decryption(PasswordProtected)) |
| AES-256 | 5/5 | Ok(1) | Ok(1) | Err(Decryption(PasswordProtected)) |
| AES-256 | 5/6 | Ok(1) | Ok(1) | Err(Decryption(PasswordProtected)) |

### Expected

The owner password opens the document for every revision, as in other readers.

### Possible fix

Algorithm 7 of ISO 32000-1 (7.6.3.4): compute an RC4 key from the owner password
(Algorithm 3, steps a-d: pad, MD5, 50 extra MD5 rounds for R >= 3, truncate to the key
length), decrypt `/O` with it (R2: once; R3/R4: 20 times, XOR-ing each key byte with
the iteration counter 19..0), and authenticate the result as the user password with the
existing code path. Related: the `// TODO: Convert to PDFDocEncoding` at
`crypto/mod.rs:463` means non-ASCII passwords cannot match for these revisions either.
~~~

---

## 3. Stack overflow on long chains of indirect color spaces (follow-up to #1347)

- **Type**：Bug（robustness / denial of service）
- **Version**：`ced00dd0`
- 備註：#1347 已關閉。它最後一則留言（2026-10-03，main @ `41d11844`）已經回報「10 000 個 `/Indexed`、Type 3 function 串成的鏈」仍會 stack overflow；`ced00dd0`（#1389）修了 object stream 的自我參照，但長鏈仍然重現。建議另開新 issue 並引用該留言，避免在已關閉的 issue 下追加。

~~~markdown
### Summary

After #1386, #1387 and #1389, deeply nested direct objects and self-references no longer
crash, but a long chain of *distinct* indirect objects still recurses once per link
while hayro builds color spaces: `ColorSpace::new` / `new_inner`
(`hayro-interpret/src/color/mod.rs:164-235`) recurses for the `/Indexed` base,
`/Separation` and `/DeviceN` alternates, the `/Pattern` base and ICC `/Alternate`, with
no depth limit (`Function::new` for Type 3 `/Functions` is the same pattern, as noted
in the last comment of #1347).

A 688 KB file with 10,000 `/Indexed` color spaces, each based on the next, overflows an
8 MiB stack and aborts the process. A stack overflow cannot be caught with
`catch_unwind`, so any application rendering untrusted PDFs can be killed by it.

### Reproduction

Helpers (`pdf()`, `page()`, `stream()`, render harness): at the end of this issue.

```python
n = 10_000
objs = page(b"/CS0 cs 0 sc 10 10 100 100 re f", b"<< /ColorSpace << /CS0 5 0 R >> >>")
for i in range(n):
    base = b"%d 0 R" % (6 + i) if i < n - 1 else b"/DeviceRGB"
    objs[5 + i] = b"[/Indexed " + base + b" 0 <000000>]"
open("indexed-chain-10000.pdf", "wb").write(pdf(objs))
```

| links | 8 MiB stack | 64 MiB stack |
|---|---|---|
| 1,000 | renders in 4.2 ms | — |
| 10,000 | `thread '<unknown>' has overflowed its stack` (process aborted, `STATUS_STACK_OVERFLOW` on Windows) | renders in 183 ms |

### Possible fix

Thread a depth counter through color space (and function) construction and give up
beyond a small limit, like `MAX_OBJECT_NESTING_DEPTH` in hayro-syntax; no real document
needs more than a handful of levels.
~~~

---

## 4. No limit on decoded stream size or on up-front image allocations

- **Type**：Feature request（security）
- **Version**：`ced00dd0`
- 相關：#1259（open，149 bytes 的檔案造成 allocation failure）、#273、#1382。

~~~markdown
### Summary

Filters decode into memory without any size limit (`hayro-syntax/src/filter/lzw_flate.rs:14-38`
uses `read_to_end`), and some decoders allocate the declared image size before looking
at the data (`hayro-syntax/src/filter/ccitt.rs:20-26` and `:150` allocate
`Columns x max(Rows, Height)` bytes). A small file can therefore make a viewer allocate
gigabytes. If the allocation fails the process aborts, which `catch_unwind` cannot
contain, and before that the machine starts paging.

### Reproduction

Helpers (`pdf()`, `page()`, `stream()`, render harness): at the end of this issue.

```python
import zlib
z = zlib.compressobj(9)               # 1 GiB of spaces -> 1.04 MB file
data = b"".join(z.compress(b" " * (1 << 20)) for _ in range(1024))
data += z.compress(b"\n0 g 10 10 50 50 re f") + z.flush()
objs = page(b"")
objs[4] = b"<< /Filter /FlateDecode /Length %d >>\nstream\n%s\nendstream" % (len(data), data)
open("flate-bomb.pdf", "wb").write(pdf(objs))

objs = page(b"q 100 0 0 100 50 50 cm /Im0 Do Q", b"<< /XObject << /Im0 5 0 R >> >>")
objs[5] = stream(b"/Type /XObject /Subtype /Image /Width 40000 /Height 40000 "
                 b"/BitsPerComponent 1 /ColorSpace /DeviceGray /Filter /CCITTFaxDecode "
                 b"/DecodeParms << /K -1 /Columns 40000 /Rows 40000 >>", b"\x00" * 16)
open("ccitt-40000.pdf", "wb").write(pdf(objs))       # 755 bytes
```

| file | result | time | peak working set | peak commit |
|---|---|---|---|---|
| `flate-bomb.pdf` (1.04 MB) | renders (1 GiB content stream decoded) | 1.0 s | 2,086 MiB | 3,081 MiB |
| `ccitt-40000.pdf` (755 B) | `ImageDecodeFailure` after allocating 1.6 GB | 0.2 s | 1,531 MiB | 1,530 MiB |

### Request

Configurable limits, for example in `InterpreterSettings` or as load options:
maximum decoded bytes per stream and maximum image pixels, checked while decoding
(Flate/LZW output, image buffers) and reported as warnings or errors instead of
allocating. `Vec::try_reserve` for large buffers would turn the remaining failures into
recoverable errors.
~~~

---

## 5. Data point for #1052: a 6 KB Form XObject DAG takes over 30 s to render — please consider a work budget too

- **Type**：在 #1052（cooperative cancellation，open）下留言，不另開 issue。
- 備註：maintainer 在 #1052 表示會自己接手實作（2026-09-29）。這則留言只補一個 cancellation token 解決不了的情境與數字。

~~~markdown
A data point for this feature, plus a related request.

Even with a stop token, a viewer has to pick a timeout per page. A deterministic
*work budget* (a maximum number of operators or draw calls per page, checked at the same
injection points as the stop token) would let applications reject pathological pages
without a timer, and the result would not depend on machine speed.

Example: Form XObject `k` draws Form `k+1` twice. The nesting limit
(`MAX_NESTED_INTERPRETATION_DEPTH = 50`, `hayro-interpret/src/context.rs:21`) does not
help because the depth is only 24; the work is 2^24 leaf fills.

```python
depth = 24                                   # 5.8 KB file
objs = page(b"/F0 Do", b"<< /XObject << /F0 5 0 R >> >>")
for k in range(depth):
    if k < depth - 1:
        body = b"q 0.5 0 0 0.5 0 0 cm /F Do Q q 0.5 0 0 0.5 100 100 cm /F Do Q"
        res = b"<< /XObject << /F %d 0 R >> >>" % (6 + k)
    else:
        body, res = b"0 g 0 0 200 200 re f", b"<< >>"
    objs[5 + k] = stream(b"/Type /XObject /Subtype /Form /BBox [0 0 200 200] /Resources " + res, body)
open("form-fanout-24.pdf", "wb").write(pdf(objs))
```

`hayro::render` at `ced00dd0` (helpers at the end of this comment): depth 20 takes 1.8 s;
depth 24 takes 33-36 s with a peak working set of 1.2 GiB.
~~~

---

## 6. `render()` panics when the page is 65,533 px or more wide or tall

- **Type**：Bug
- **Version**：`ced00dd0`, vello_cpu 0.3.0 / vello_common 0.3.0
- 備註：根因一半在 vello（`RenderContext::new` 沒有檢查尺寸）。可以考慮同時在 linebender/vello 回報 vello 的部分。

~~~markdown
### Summary

`hayro::render` computes the pixmap size with saturating casts,
`(width * x_scale) as u16` (`hayro/src/lib.rs:238-241`), and passes it to
`RenderContext::new`. vello_cpu panics for a width or height of 65,533-65,535 because
`snap_to_tile_coordinates` rounds up to a multiple of 4 with
`checked_next_multiple_of(4).unwrap()` (`vello_common-0.3.0/src/util.rs:237`). Every
page whose rendered size exceeds 65,532 px is clamped to 65,535 by the cast and then
panics; there is no error path.

A 14,400 pt page (the largest size without `/UserUnit`) at scale 5 triggers it.

### Reproduction

Helpers (`pdf()`, `page()`, `stream()`, render harness): at the end of this issue.

```python
open("wide-page.pdf", "wb").write(pdf(page(b"0 g 10 10 100 50 re f", size=(14400, 100))))
```

```
cargo run --release -- wide-page.pdf 5
thread '<unnamed>' panicked at vello_common-0.3.0\src\util.rs:237:59:
called `Option::unwrap()` on a `None` value
   4: vello_common::util::snap_to_tile_coordinates
   5: vello_cpu::coarse::bucketer::CommandBucketer::new   (bucketer.rs:195)
   7: vello_cpu::dispatch::single_threaded::SingleThreadedDispatcher::new
   8: vello_cpu::render::RenderContext::new_with           (render.rs:216)
   9: vello_cpu::render::RenderContext::new                (render.rs:197)
  10: hayro::render                                        (hayro/src/lib.rs:238)
```

At scale 4 (57,600 px) the page renders fine.

### Expected

No panic: either a documented maximum with an error, or clamping to a size vello
accepts (and not silently clamping larger sizes, which produces a wrong image).
~~~

---

## 7. Knockout transparency groups (`/K true`) are composited like normal groups

- **Type**：Feature request（README 已說明 knockout 未支援；這裡提供最小測試檔與數字）
- **Version**：`ced00dd0`

~~~markdown
### Summary

The README lists knockout groups as unsupported; this is a minimal test case for when
they get implemented. `FormXObject::new` only records whether a `/Group` exists
(`hayro-interpret/src/x_object/form.rs:34`) and `Device::push_transparency_group`
(`hayro-interpret/src/device.rs:21-26`) has no isolated/knockout parameters, so a
`/K true` group renders exactly like a normal one.

(Side note: the crate-level docs of `hayro`, `hayro/src/lib.rs:16-19`, still list
encrypted PDFs as unsupported.)

### Reproduction

Helpers (`pdf()`, `page()`, `stream()`, render harness): at the end of this issue.

Two 50% squares, red then blue, overlapping in a transparency group:

```python
form = stream(b"/Type /XObject /Subtype /Form /BBox [0 0 200 200] "
              b"/Group << /S /Transparency /K true >> /Resources << /ExtGState << /H << /ca 0.5 >> >> >>",
              b"/H gs 1 0 0 rg 20 20 100 100 re f 0 0 1 rg 80 80 100 100 re f")
objs = page(b"/G Do", b"<< /XObject << /G 5 0 R >> >>")
objs[5] = form
open("group-knockout.pdf", "wb").write(pdf(objs))
```

```
cargo run --release -- group-knockout.pdf 1 100,100 40,160 160,40
```

### Expected

In a knockout group the blue square replaces the red one where they overlap, so the
overlap shows 50% blue over white, the same color as the blue-only area:
pixel (100,100) about (128, 128, 255).

### Actual

Identical output to `/K false`; the overlap shows blue composited over red:

```
pixel 100,100 = [127, 63, 191, 255]
pixel 40,160 = [255, 127, 127, 255]
pixel 160,40 = [127, 127, 255, 255]
```
~~~

### Proposed fix（patch 0002）

- **Patch**：`patches/hayro-0002-support-knockout-groups.patch`（commit `eb8753b1`，base `ced00dd0`）。和 0001 互相獨立。
- **改了什麼**（+64／−9 行，不含測試）：
  - `hayro-interpret`：`FormXObject` 讀取 group 的 `/K`；knockout group 改用新的 `Device::push_knockout_group` 推入，pop 仍用 `pop_transparency_group`。新方法的預設實作直接呼叫 `push_transparency_group`，所以 hayro-svg、範例程式、Type3／pattern 的包裝 device、FastPDF adapter 的 device 都不用改（FastPDF adapter 原封不動就能用 `[patch]` 編譯）。
  - `hayro`：`Renderer` 記錄 blend stack 上每個 group 是不是 knockout；直接畫在 knockout group 裡的 path 與 glyph 用 `Compose::Copy`。group 是畫在一開始透明的 layer 裡，所以這就是「和 group 的初始 backdrop 合成」，反鋸齒邊緣由 vello_cpu 依 coverage 內插，等於規格中依 shape 加權平均。不透明物件的結果和原本完全相同，只有半透明物件會改變。
- **測試**：新增 `hayro-tests/pdfs/custom/group_knockout.pdf`（manifest＋`tests/render.rs`）：條紋背景上四個 group（`/I`、`/K` 四種組合），每個畫紅、綠、藍三個 50% 的圓。修正前四組一模一樣；修正後兩個 knockout group 裡後畫的圓取代先畫的圓，重疊處只看得到最後一個圓。另外確認 0002 單獨套在 `ced00dd0` 上也能編譯、測試通過。
- **hayro 測試與 FastPDF 驗證**：同 0001（既有 snapshot 無差異；FastPDF 的 knockout 兩組和 zpdf 一致）。

還沒涵蓋（行為和現在一樣），以及完整支援的設計與工作量估計：

| 項目 | 現況 | 完整做法 | 估計 |
|---|---|---|---|
| knockout group 裡的影像與巢狀 group | 仍以 SrcOver 疊在先前物件上（它們各自是一個 layer；用 `Copy` 合成會把整個 layer 範圍清空） | 取得元素的 shape（影像＝影像範圍或 stencil；巢狀 group＝把內容畫成 mask），先以 `DestOut` 依 shape 擦掉先前物件，再正常疊上元素。不需改 vello_cpu；巢狀 group 要多畫一次 | 2–3 天（含測試） |
| knockout group 裡帶 soft mask（ExtGState `/SMask`）的物件 | soft mask 被 vello_cpu 乘進 coverage，等於當成 shape；規格（`AIS false`）是 opacity | 先用路徑的 shape 做 `DestOut`，再帶 mask 以 SrcOver 畫；每個這種物件多畫一次 | 0.5–1 天 |
| 非 isolated 的 knockout group 搭配非 Normal 的 blend mode | 和 hayro 目前所有 group 一樣當成 isolated | 需要 non-isolated group：layer 以 backdrop 初始化、合成時扣掉 backdrop 的貢獻；vello_cpu 目前只有 isolated layer，要先改 vello_cpu | 1–2 週（含 vello_cpu） |
| 頁面 `/Group` 的 `/K`、文字 knockout 參數 `TK` | 忽略 | 頁面 group 用 `push_knockout_group` 包住頁面內容；`TK` 要在 text object 層級處理 | 各約 0.5 天，實務上少見 |
| hayro-svg | 忽略 knockout（預設實作） | SVG 沒有對應的合成模式，需要 filter 或預先合成 | 不建議 |

PR 描述草稿（同樣補上完整測試的結果）：

~~~markdown
**hayro: Support knockout groups**

Objects in a transparency group with `/K true` were composited over each other like
in any other group (the README lists knockout groups as unsupported). In a knockout
group, each object is composited with the initial backdrop of the group instead, so
it replaces the objects painted before it wherever it paints.

- hayro-interpret reads `K` from the group attributes of form XObjects and pushes
  knockout groups with a new `Device::push_knockout_group`. Its default
  implementation calls `push_transparency_group`, so existing devices (hayro-svg,
  the Type3/pattern wrapper devices, downstream implementations) keep working
  unchanged.
- hayro draws paths and glyphs that are painted directly into a knockout group with
  `Compose::Copy`. The group is rendered into a layer that starts out transparent, so
  this composites them with the initial backdrop; vello_cpu interpolates by coverage
  at anti-aliased edges, which matches the shape-weighted average from the spec.
  Opaque objects render exactly as before.

Not covered yet (unchanged behavior): images and nested groups inside a knockout
group are still composited over the earlier objects, since they are separate layers
and compositing a whole layer with `Copy` would clear its entire area; for objects
in a knockout group, a soft mask from the graphics state acts as shape instead of
opacity; non-isolated knockout groups with non-normal blend modes (isolation isn't supported
yet); `K` of the page group and the text knockout flag `TK`. hayro-svg ignores the
attribute.

API: I added a provided method so that this is not a breaking change and can go
into a 0.8.x release. If you prefer, the group attributes (knockout, and later
isolated) could instead be passed to `push_transparency_group` in the next breaking
release; happy to change it.

Test: `group_knockout`: four groups (all `I`/`K` combinations), each with three
overlapping 50% circles over a striped backdrop. Before: all four look the same.
After: in the knockout groups the later circles replace the earlier ones.
~~~

---

## 8. Non-embedded CJK fonts silently fall back to Helvetica, so the text disappears

- **Type**：Documentation + feature request
- **Version**：`ced00dd0`
- 備註：README 已說明不支援非內嵌 CID font，但 hayro 已經有 `FontQuery::Fallback` 與 `character_collection`，只差文件與預設行為。FastPDF 的作法見 `crates/fastpdf-engine-hayro/src/fonts.rs`。

~~~markdown
### Summary

For a non-embedded CID font hayro sends `FontQuery::Fallback` with the
`character_collection` (e.g. Adobe-CNS1) to the font resolver, which is great. The
default resolver (`InterpreterSettings::default`, `hayro-interpret/src/interpret/mod.rs:105-122`)
answers every fallback query with `pick_standard_font()`, i.e. the Latin-only Foxit
Helvetica/Times/Courier, so CJK text renders as nothing, and nothing reaches
`warning_sink`.

Such files are very common in Taiwan, mainland China, Japan and Korea (MSung-Light,
STSong-Light, HeiseiMin-W3, HYSMyeongJo-Medium from the Adobe Asian font packs, many
government forms).

### Reproduction

Helpers (`pdf()`, `page()`, `stream()`, render harness): at the end of this issue.

```python
text = "中文字型測試".encode("utf-16-be").hex().upper().encode()
objs = page(b"BT /F1 48 Tf 20 120 Td <" + text + b"> Tj ET", b"<< /Font << /F1 5 0 R >> >>",
            size=(340, 200))
objs[5] = (b"<< /Type /Font /Subtype /Type0 /BaseFont /MSung-Light /Encoding /UniCNS-UCS2-H "
           b"/DescendantFonts [6 0 R] >>")
objs[6] = (b"<< /Type /Font /Subtype /CIDFontType0 /BaseFont /MSung-Light "
           b"/CIDSystemInfo << /Registry (Adobe) /Ordering (CNS1) /Supplement 5 >> "
           b"/FontDescriptor 7 0 R /DW 1000 >>")
objs[7] = (b"<< /Type /FontDescriptor /FontName /MSung-Light /Flags 6 /FontBBox [-160 -249 1015 1071] "
           b"/ItalicAngle 0 /Ascent 880 /Descent -120 /CapHeight 880 /StemV 93 >>")
open("cjk-msung-nonembedded.pdf", "wb").write(pdf(objs))
```

With the default `InterpreterSettings` (the resolver wrapped to print its queries, and
a `warning_sink` that counts warnings):

```
query: Fallback(name=Some("MSung-Light"), collection=Some(CharacterCollection { family: AdobeCNS1, supplement: 3 })) -> Helvetica
340x200 in 4.5ms, non-white pixels: 0
warnings: 0
```

### Suggestions

1. Document `InterpreterSettings::font_resolver` together with
   `FallbackFontQuery::character_collection`, with an example that maps collections to
   system fonts (on Windows, for example: Adobe-CNS1 -> PMingLiU / Microsoft JhengHei,
   Adobe-GB1 -> SimSun / Microsoft YaHei, Adobe-Japan1 -> Yu Gothic / MS Mincho,
   Adobe-Korea1 -> Malgun Gothic / Batang).
2. Emit an `InterpreterWarning` when a CID font with a CJK collection falls back to a
   standard font (or when glyph lookup fails), so applications can tell the user a font
   is missing instead of showing an empty page.
3. The doc comment of `FontQuery::Fallback` (`hayro-interpret/src/font/mod.rs:524-527`)
   still says this query type is "currently not supported".
~~~

---

## 9. Tiled rendering: images are decoded again on every `render_into` call, and `RenderCache` has no size bound

- **Type**：Feature request（效能／記憶體）
- **Version**：`ced00dd0`
- 備註：FastPDF 目前的對策（block 合併、process 級 decode budget、閒置 5 s 釋放執行緒與 cache）見 `docs/benchmarks/b5-memory.md`。這兩點需要 hayro 端的 API，adapter 無法根治。

~~~markdown
### Summary

`render_into` (#1375) made tiled rendering possible — thanks! Two things make it
expensive for a tiled viewer:

1. **Images are decoded on every call.** Each `render_into` decodes every image of the
   page again at full resolution and converts it to RGBA, however small the tile. On a
   300-dpi grayscale scan (one 2480x3508 JPEG per page), eight 512 px tiles at scale 4
   cost 23.3-23.8 ms each, the price of a full-page decode, while tiles of a text page
   cost 0.2 ms. Each decode also allocates the samples plus the RGBA pixmap (about
   44 MB for this page, about 240 MB for a 600-dpi A4 color scan) on every rendering
   thread at once.
2. **`RenderCache` is unbounded and per thread.** It is `!Send`, so a pool of render
   threads keeps one copy per thread, and there is no way to see or limit its size. In
   our viewer, after rendering a test document with all 5,401 Big5 level-1 characters
   (about 22 embedded TrueType subsets), each thread kept roughly 47-67 MB (measured as process
   private bytes; it is freed when the `RenderCache`/`InterpreterCache` is dropped,
   presumably fonts and the glyph outline cache).

### Requests

- An optional decoded-image cache (e.g. in `RenderCache`) keyed by image object and
  decode size, with a byte budget, so neighbouring tiles reuse the decoded image.
- A byte budget, or at least a size query and a way to clear the outline cache, for
  `RenderCache`; or a `Send + Sync` variant that threads can share.

Reproduction of the tile timing: render 512x512 tiles with
`render_into(page, &cache, &settings, &RenderSettings::default(), &mut ctx, Affine::translate((-x, -y)) * Affine::scale(4.0) * page.initial_transform(true).to_kurbo())`,
reusing one `RenderCache` and `RenderContext`.
~~~

---

## 修正 patch：驗證與送 PR 前的步驟

### Patch 清單

| 檔案 | 對應 | commit（本地分支 `fastpdf/transparency-fixes`） | 範圍 |
|---|---|---|---|
| `patches/hayro-0001-keep-soft-masks-whose-group-has-no-color-space.patch` | #1 | `da45b370` | `hayro-interpret/src/x_object/soft_mask.rs`＋測試 `mask_no_cs`（render、SVG） |
| `patches/hayro-0002-support-knockout-groups.patch` | #7 | `eb8753b1` | `hayro-interpret`（`device.rs`、`x_object/form.rs`）、`hayro`（`lib.rs`、`mask.rs`）＋測試 `group_knockout` |
| `patches/hayro-transparency-before-after.png` | #1、#7 | — | PR 用的前後對照圖（FastPDF fixture，96 dpi） |

兩個 patch 用 `git format-patch` 產生，各自都能單獨 `git apply --check` 到 `ced00dd0`。0002 單獨套用後也編譯、測試通過。

### hayro 測試（本機，rustc 1.99.0）

- 先在未修改的 `ced00dd0` 產生 baseline snapshot，再切到分支比對。本機能跑的測試：`ced00dd0` 436 個通過；加上兩個 patch 後 439 個通過（436＋新增的 `render::mask_no_cs`、`svg::mask_no_cs`、`render::group_knockout`），**既有 snapshot 沒有任何像素差異**。
- 另外 1,277 個測試需要 `sync.py` 從 hayro-assets.dev 下載的 PDF（pdf.js、PDFBox、PDFium、corpus 與部分 custom），本次沒有下載，兩邊都是同樣的「找不到檔案」。
- `hayro-interpret` 單元測試 48 個通過；`cargo test -p hayro-tests -- "load::"`（debug，同 CI）118 個通過；`hayro-syntax` 有 2 個失敗，原因同樣是需要下載的檔案，`ced00dd0` 上也一樣。
- `cargo fmt --check --all`、`RUSTDOCFLAGS="-D warnings" cargo doc -p hayro-interpret -p hayro --no-deps`、`cargo check -p hayro-interpret --no-default-features` 通過。clippy 1.99 在既有程式碼有新版 lint 的警告（`chunks_exact`→`as_chunks`、`sort_by_key` 等），修改到的程式碼沒有警告；CI 用的是 1.92，本機沒有裝。
- 為了讓測試 PDF 的位元組和 Linux／CI 相同，本地 clone 已用 `core.autocrlf=false` 重新 checkout（原本 Windows 的 autocrlf 會把文字型 PDF 轉成 CRLF，xref offset 跟著錯位）。

### FastPDF 驗證（不改 `D:\fastPDF`）

方法：`git archive HEAD`（`2f162e0`）匯出到 scratchpad，在匯出的 `Cargo.toml` 加上

```toml
[patch."https://github.com/LaurenzV/hayro"]
hayro = { path = "D:/fastPDF/upstream/hayro/hayro" }
```

`fastpdf-engine-hayro` 不用任何修改就能編譯。以 `--features engine-zpdf` build 未修補（git `ced00dd0`）與修補後兩個 `fastpdf-bench`。本地 clone 平常停在 `main`（＝`ced00dd0`），重現前先 `git -C upstream/hayro -c core.autocrlf=false switch fastpdf/transparency-fixes`。

**`diff-corpus --engine hayro,zpdf`（84 檔、185 頁，tolerance 16）**

| | 未修補 | 修補後 |
|---|---|---|
| `transparency/softmask-groups.pdf` 差異像素 | 10.884% | **0.311%** |
| 同上 PSNR／平均絕對差 | 18.85 dB／8.07 | **43.83 dB／0.64** |
| 其他 184 頁 | — | 數字完全相同 |
| 全 corpus 每檔最大差異的中位數／p90 | 3.679%／10.881% | 3.679%／10.502% |

剩下的 0.311% 只在反鋸齒邊緣與文字（兩個 rasterizer 本來就有的差異）。前後對照：`patches/hayro-transparency-before-after.png`（左：`ced00dd0`；右：修補後。上：Alpha soft mask (group ca 0.4)；下：knockout）。

**`corpus --repeat 3` 與 `compare`**

量測時機器上有其他 agent 在編譯（CPU 35–80%），計時噪音大：未修補版本自己跑 4 次，同一個指標的（最大−最小）／中位數，中位數 7%、p90 35%。所以未修補與修補後**交替各跑 4 次**，比較中位數：

- 84 檔的狀態（ok／partial／open_error…）完全相同。
- 和 `benchmarks/baseline.json` 比（各取 4 次的中位數，>10% 算 regression）：未修補 135 項、修補後 129 項。兩者都大量超標，原因是 baseline（`f033b35`，安靜的機器）之後 FastPDF 本身的變更（2 個 render worker、B-5 等）加上目前的機器負載，不是 hayro patch；兩者的差集只有零星、在噪音範圍內的小數值。**因此不能直接用 baseline 判斷，改用 A/B 比較。**
- A/B：中位數差超過 10% 且落在未修補 4 次範圍之外的只有 5 項，且都不可能被修補影響：`malformed/deep-nesting-dict-5000.pdf` 的 open（7.40 → 8.30 ms）、time to first page、page render，`flat-2000p` 的 first page（2.20 → 2.46 ms），`bad-xref-offsets` 的 page render（−13%，改善）。開檔根本不會執行修補到的程式碼；用「同樣以 path dependency 編譯、但沒有修補的 `ced00dd0`」當對照組跑 `open --repeat 30` 三輪：deep-nesting 為 git 版 7.34、path 對照 7.79、修補後 8.06 ms（範圍重疊），flat-2000p 為 3.88／3.77／4.32 ms（範圍重疊）。差異來自 git 與 path dependency 的編譯差異加上噪音，不是 patch。
- **patch 造成的效能差異（正確性的代價）**：只有 `transparency/softmask-groups.pdf`。整頁 render（96 dpi，`render --repeat 40` 三輪）3.8 → 5.4 ms（+1.6 ms），private 峰值 11.2 → 13.1 MB：原本被丟掉的 alpha soft mask 現在真的畫出來（一張全頁的 mask），knockout 物件改走 vello_cpu 的 blend 路徑。只裝 0001 的版本已經佔了大部分（同一輪負載下：未修補 7.1、只裝 0001 9.0、兩個都裝 9.6 ms，private 峰值 11.9／13.1／13.1 MB）。corpus 中位數：first page 5.89 → 6.43 ms，RSS 16.0 → 16.9 MB。

### 送 PR 前的步驟

查核結果（2026-10-04，用 `gh api` 唯讀查 GitHub community profile 與 repo 內容）：hayro **沒有** CONTRIBUTING、PR template、issue template、code of conduct，也沒有 AI policy 檔案。以下依 CI 設定（`.github/workflows/ci.yml`）、`hayro-tests/README.md` 與既有 PR 整理：

1. **基準**：GitHub 上 `main` 已前進到 `ea9c81dc`（Version bump #1390：版本改成 0.8.0，只改各 crate 的 `Cargo.toml` 與 `Cargo.lock`）。兩個 patch 都不碰這些檔案，應該能直接套用；請在最新 `main` 開分支後 `git am` 套用，再跑一次下面的檢查。
2. **作者資訊**：patch 的 `From:` 是本機 git 身分（`sensa-code <255512260+sensa-code@users.noreply.github.com>`）。請改成要用來送 PR 的 GitHub 身分（`git am` 之後 `git commit --amend --reset-author`）。
3. **AI 協助揭露**：commit 訊息帶有 `Co-Authored-By: Claude Opus 5.5` trailer。hayro 沒有 AI 政策，但 maintainer 會逐項評估 AI 產生的內容（#1195 只採用有實測效益的部分）。建議 PR 描述簡短、說明是 AI 協助並已人工審閱與驗證，自己讀懂程式碼以便回應 review。
4. **CI 檢查**（MSRV 1.92；本機只有 1.99，請用 1.92 再跑一次）：
   - `RUSTFLAGS="-D warnings" cargo clippy --workspace --exclude hayro-demo --exclude hayro-bench --tests --examples`
   - `cargo fmt --check --all`
   - `RUSTFLAGS="-D warnings" cargo doc --workspace --exclude hayro-demo --exclude hayro-bench --no-deps`
   - `RUSTFLAGS="-D warnings" cargo hack check --each-feature --workspace --exclude hayro-demo --exclude hayro-bench`（需要 cargo-hack，本次沒有裝）
   - `cargo check --workspace`、no_std 檢查、`cargo test -p hayro-tests -- "load::"`
5. **完整回歸測試**（`hayro-tests/README.md`）：`python3 sync.py` 下載 pdf.js／PDFBox／corpus 與字型（數千個 PDF，本次沒有下載）；在 `main` 跑 `cargo test --release` 產生 baseline snapshot；切到分支再跑，檢查 `diffs/`。0001 只會影響 soft mask 的 group 缺少或無法解析 `/CS`（或沒有 `/Group`）的檔案，0002 只會影響含 `/K true` 的檔案，預期都是改善；把有變化的檔案列在 PR 描述裡。新測試的 snapshot 不進 repo，reviewer 會自己產生。
6. **Windows 注意**：clone 要用 `core.autocrlf=false`，否則文字型的測試 PDF 會被轉成 CRLF。`git am` 會因為 PDF xref 行尾的空白（PDF 格式要求，既有測試檔也一樣）出現 whitespace 警告，屬正常。
7. **慣例**：一個 PR 一件事（0001、0002 分開送）；標題以 crate 名稱開頭（`hayro-interpret: …`、`hayro: …`，同 commit 標題）；新測試＝`pdfs/custom/*.pdf`＋`manifest_custom.json`＋`tests/render.rs`（必要時 `tests/svg.rs`），PDF 以未壓縮、可讀的形式提交。PR 描述可直接用上面的草稿，附上前後對照圖，必要時附上 FastPDF fixture。
8. **0002 的 API 選擇**：目前新增的是有預設實作的 `Device::push_knockout_group`，屬於非破壞性變更，可以進 0.8.x。maintainer 也可能偏好在下一個 breaking release（0.9）讓 `push_transparency_group` 直接接收 group 屬性（knockout，以及之後的 isolated）；PR 描述已經提出這個選項，依 review 調整即可。
9. README 與 `hayro/src/lib.rs` 的 crate 文件仍寫著 knockout group 未支援（後者也還寫著不支援加密檔）；patch 沒有改文件，交給 maintainer 決定措辭。

---

## 附錄 A：產生 PDF 的 Python helper

只用標準函式庫。`page()` 產生 1 頁的文件骨架（物件 1–4），各 issue 再把自己的物件放在 5 以後（物件編號必須連續）。

```python
def stream(entries, data):
    return b"<< %s /Length %d >>\nstream\n%s\nendstream" % (entries, len(data), data)

def page(content, resources=b"<< >>", size=(200, 200)):
    """Objects 1-4: catalog, page tree, page, content stream."""
    return {1: b"<< /Type /Catalog /Pages 2 0 R >>",
            2: b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
            3: b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 %d %d] /Resources %s /Contents 4 0 R >>"
               % (size[0], size[1], resources),
            4: stream(b"", content)}

def pdf(objects):
    """Serializes objects 1..n with a classic xref table at the right offsets."""
    out, offsets = bytearray(b"%PDF-1.7\n"), []
    for num in range(1, len(objects) + 1):
        offsets.append(len(out))
        out += b"%d 0 obj\n%s\nendobj\n" % (num, objects[num])
    xref = len(out)
    out += b"xref\n0 %d\n0000000000 65535 f \n" % (len(objects) + 1)
    out += b"".join(b"%010d 00000 n \n" % o for o in offsets)
    out += b"trailer\n<< /Size %d /Root 1 0 R >>\nstartxref\n%d\n%%%%EOF\n" % (len(objects) + 1, xref)
    return bytes(out)
```

## 附錄 B：Rust render harness

`Cargo.toml`：`hayro = { git = "https://github.com/LaurenzV/hayro", rev = "ced00dd0" }`。用法：`harness FILE [SCALE] [X,Y ...]`。

```rust
use std::sync::Arc;

use hayro::hayro_interpret::InterpreterSettings;
use hayro::hayro_syntax::Pdf;
use hayro::vello_cpu::color::palette::css::WHITE;
use hayro::{PixmapSettings, RenderCache, RenderSettings, render};

fn main() {
    // An 8 MiB stack, like the main thread on Linux.
    std::thread::Builder::new().stack_size(8 << 20).spawn(run).unwrap().join().unwrap();
}

fn run() {
    let args: Vec<String> = std::env::args().collect();
    let pdf = Pdf::new(Arc::new(std::fs::read(&args[1]).unwrap())).unwrap();
    let scale: f32 = args.get(2).map_or(1.0, |s| s.parse().unwrap());
    let started = std::time::Instant::now();
    let pixmap = render(&pdf.pages()[0], &RenderCache::new(), &InterpreterSettings::default(),
        &RenderSettings::default(), &PixmapSettings { x_scale: scale, y_scale: scale, bg_color: WHITE });
    let data = pixmap.data_as_u8_slice();
    let ink = data.chunks_exact(4).filter(|p| p[..3].iter().any(|&c| c < 224)).count();
    println!("{}x{} in {:?}, non-white pixels: {ink}", pixmap.width(), pixmap.height(), started.elapsed());
    for point in args.iter().skip(3) {
        let (x, y) = point.split_once(',').unwrap();
        let i = (y.parse::<usize>().unwrap() * pixmap.width() as usize + x.parse::<usize>().unwrap()) * 4;
        println!("pixel {point} = {:?}", &data[i..i + 4]);
    }
}
```

#8 的輸出額外把預設 resolver 包一層，印出收到的 `FontQuery`：

```rust
let mut settings = InterpreterSettings::default();
let default = settings.font_resolver.clone();
settings.font_resolver = Arc::new(move |query| {
    if let hayro::hayro_interpret::font::FontQuery::Fallback(f) = query {
        println!("query: Fallback(name={:?}, collection={:?}) -> {:?}",
                 f.post_script_name, f.character_collection, f.pick_standard_font());
    }
    default(query)
});
```

## 附錄 C：#2 的加密檔（pypdf）

```python
# pip install pypdf==6.19.0 cryptography
from pypdf import PdfWriter

for algorithm in ("RC4-40", "RC4-128", "AES-128", "AES-256-R5", "AES-256"):
    w = PdfWriter()
    w.add_blank_page(200, 200)
    w.encrypt(user_password="user", owner_password="owner", algorithm=algorithm)
    with open(f"enc-{algorithm.lower()}.pdf", "wb") as f:
        w.write(f)
```
