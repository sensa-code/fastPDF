<!-- 本檔由 FastPDF.docx 轉出的純文字版本，方便 diff / grep 與 AI 工具閱讀。原始規格以 repo 根目錄的 FastPDF.docx 為準。 -->

# FastPDF — Rust 極速 PDF Reader

> Project Handoff for Claude

## 0. 你的角色

你現在接手一個新的 PDF Reader 專案。

目標不是製作 Adobe Acrobat 的完整替代品，也不是做 PDF Editor，而是打造：

一個以 Rust 為核心、Windows 優先、啟動極快、低記憶體、低背景 CPU、GPU 加速、可長期維護與商業化的極速 PDF Reader。

產品精神接近：

- SumatraPDF 的輕量
- Zed / GPUI 的現代 GPU-native UI
- Rust 的安全性與模組化
- Lazy loading
- Tile-based rendering
- 嚴格 memory budget
- 零 telemetry
- 零 cloud dependency
- 零 login
- 開檔即看

暫定產品名稱：

FastPDF

名稱未來可以更換，不要讓命名耦合進核心架構。

## 1. 專案核心目標

FastPDF 最重要的 KPI 不是功能數量，而是：

- Cold Start 快
- PDF First Page 顯示快
- 大型 PDF 不爆 RAM
- Scroll 順
- Zoom 順
- Idle CPU 幾乎為 0
- 不在背景做不必要工作
- 不 render 使用者目前看不到的內容
- 所有 cache 必須有硬性容量限制
- Renderer 可以替換

基本原則：

Never render what the user cannot see.

以及：

Opening a PDF must not require processing the entire PDF.

## 2. 建議 upstream 專案

首先研究：

### UI / Reader Prototype

Repository:

https://github.com/Lej77/pdf-reader-gpui

用途：

- 作為 Reader UI architecture 起點
- GPUI window / event handling
- PDF viewport prototype
- Windows Rust build pipeline
- 現有 Hayro integration

我們不是要永遠跟隨 upstream。

它主要是：

bootstrap / architectural reference / fork base

如果實際 audit 後發現程式碼品質不適合直接 fork，可以重新建立 workspace，逐步移植可用的部分。

不要因為已經 fork 就保留不好的 architecture。

## 3. PDF Engine 候選

### Primary candidate

Repository:

https://github.com/Xero-Team/zpdf

目標：

長期評估作為主要 PDF backend。

原因：

- Rust
- MIT license
- CPU rendering
- GPU rendering
- wgpu
- tiny-skia
- text extraction
- fonts
- annotations
- encryption
- PDF structures
- cache architecture
- 適合未來直接深入優化 renderer

但：

不要相信 README 宣稱的效能。

所有效能都必須由 FastPDF benchmark 驗證。

### Secondary / Reference Backend

Repository:

https://github.com/LaurenzV/hayro

用途：

- compatibility comparison
- renderer fallback
- correctness comparison
- regression testing
- 開發初期快速建立 PDF rendering

Hayro 不應直接耦合到 UI。

## 4. 最重要的架構決策

FastPDF 不可以寫成：

```text
GPUI
  ↓
Hayro
```

也不可以寫成：

```text
GPUI
  ↓
zpdf
```

必須是：

```text
                    FastPDF
                       │
             ┌─────────┴─────────┐
             │                   │
             UI              Reader Core
            GPUI                  │
                                  │
                            PdfEngine API
                                  │
                ┌─────────────────┼─────────────────┐
                │                 │                 │
              zpdf              Hayro            future
                                                    │
                                                 PDFium?
```

PDF engine 必須可以替換。

## 5. Rust Workspace 建議

請優先考慮：

```text
fastpdf/
│
├── Cargo.toml
│
├── crates/
│   │
│   ├── fastpdf-app/
│   │
│   ├── fastpdf-ui/
│   │
│   ├── fastpdf-core/
│   │
│   ├── fastpdf-render/
│   │
│   ├── fastpdf-engine-api/
│   │
│   ├── fastpdf-engine-hayro/
│   │
│   ├── fastpdf-engine-zpdf/
│   │
│   ├── fastpdf-cache/
│   │
│   ├── fastpdf-search/
│   │
│   └── fastpdf-bench/
│
├── fixtures/
│
├── benchmarks/
│
├── docs/
│
└── tools/
```

不要過度 microservice 化。

crate 拆分只服務以下目的：

- compile isolation
- backend replacement
- benchmark
- testability
- dependency isolation

## 6. PdfEngine API

請建立乾淨的 abstraction layer。

概念例如：

```text
pub trait PdfEngine: Send + Sync {
    type Document;
    type Error;

    fn open(
        &self,
        source: DocumentSource,
    ) -> Result<Self::Document, Self::Error>;

    fn page_count(
        &self,
        document: &Self::Document,
    ) -> usize;

    fn page_size(
        &self,
        document: &Self::Document,
        page: usize,
    ) -> Result<PageSize, Self::Error>;

    fn render(
        &self,
        request: RenderRequest,
    ) -> Result<RenderResult, Self::Error>;

    fn extract_text(
        &self,
        document: &Self::Document,
        page: usize,
    ) -> Result<TextLayer, Self::Error>;

    fn outline(
        &self,
        document: &Self::Document,
    ) -> Result<Vec<OutlineItem>, Self::Error>;

    fn links(
        &self,
        document: &Self::Document,
        page: usize,
    ) -> Result<Vec<Link>, Self::Error>;
}
```

實際 API 可以改良。

重點是：

UI 不應知道：

- Hayro type
- zpdf type
- PDFium type

UI 只知道 FastPDF domain model。

## 7. Domain Model

請建立自己的 domain types。

例如：

```text
DocumentId
PageId
PageIndex
PageSize
Viewport
ZoomLevel
RenderScale
RenderTile
RenderRequest
RenderResult

TextLayer
TextSpan

OutlineItem
DocumentMetadata
Link

CacheKey
TileKey
ThumbnailKey
```

不要讓第三方 renderer 的 datatype 滲透到整個 codebase。

## 8. V0.1 功能範圍

第一版只做：

```text
Open PDF

Previous Page
Next Page

Continuous Scroll

Zoom In
Zoom Out

Fit Width
Fit Page

Rotate

Text Selection
Copy

Search

Outline / Bookmarks

Thumbnail Sidebar

Print

Keyboard shortcuts

Drag & Drop PDF

Open Recent
```

如果某些功能會明顯拖慢第一版，可以延後：

- Print
- text selection
- thumbnail

但 architecture 必須預留。

## 9. V0.1 禁止功能

不要做：

```text
PDF editing

OCR

AI chat

Cloud storage

Account

Login

Sync

Annotations

Signature

PDF merge

PDF split

PDF convert

Compression

Online service

Telemetry

Analytics

Auto update framework

Plugin marketplace
```

這些不是目前目標。

## 10. 啟動流程

錯誤：

```text
Launch

→ initialize everything
→ scan fonts
→ create thumbnail engine
→ load search engine
→ scan document
→ render multiple pages
→ display
```

正確：

```text
Launch
  ↓
Window visible
  ↓
Open PDF
  ↓
Read minimum metadata
  ↓
Resolve page tree
  ↓
Display Page 1
  ↓
Prefetch visible-nearby pages
```

非必要功能全部 lazy initialization。

## 11. PDF 開檔策略

對於：

```text
2000 pages
800 MB
```

不能：

```text
Open
→ parse everything
→ extract all text
→ render all thumbnails
→ index everything
```

應為：

```text
Open
↓
minimum required PDF parse
↓
page count
↓
page 1 metadata
↓
page 1 render
↓
UI immediately usable
```

之後：

```text
Viewport
↓
visible pages
↓
near-visible pages
↓
low-priority prefetch
```

Search index 也必須 background incremental。

## 12. Rendering Architecture

禁止以：

每頁完整 bitmap

作為唯一 rendering model。

核心設計應支援：

Tile Based Rendering

例如：

```text
Page

┌────┬────┬────┬────┐
│ A1 │ A2 │ A3 │ A4 │
├────┼────┼────┼────┤
│ B1 │ B2 │ B3 │ B4 │
├────┼────┼────┼────┤
│ C1 │ C2 │ C3 │ C4 │
└────┴────┴────┴────┘
```

例如：

```text
256 × 256
```

或：

```text
512 × 512
```

實際 tile size 請 benchmark。

Zoom 後只 render：

viewport intersecting tiles

不要因為使用者 zoom 到 600%，就把整頁 rasterize 到 600%。

## 13. Progressive Rendering

Zoom 時：

不要等待完整 high-resolution tile。

優先考慮：

```text
existing low-res texture
        ↓
temporary upscale
        ↓
schedule high-res visible tiles
        ↓
replace progressively
```

目標：

UI 永遠有畫面，不要 freeze 等高解析 render。

## 14. Render Scheduler

建立獨立 scheduler。

Priority 大致：

```text
P0
Current visible tiles

P1
Adjacent visible page tiles

P2
Near viewport

P3
Next page prefetch

P4
Thumbnail

P5
Search / other background work
```

如果使用者快速 scroll：

過期 render request 必須可以：

- cancel
- discard
- reprioritize

不能繼續 render 使用者早已離開的頁面。

## 15. Memory Budget

Memory cache 不准無限成長。

第一階段可以測試：

```text
Font Cache              32 MB

Decoded Image Cache     64 MB

Render Tile Cache      128 MB

Thumbnail Cache         32 MB

Text Cache              32 MB
```

不要求死守以上數字。

但是：

每個 cache 都必須有 budget。

使用：

- LRU
- weighted LRU
- cost-aware eviction

皆可。

## 16. Memory Pressure

最好有統一：

```text
MemoryBudgetManager
```

例如：

```text
Normal

Soft Limit

Hard Limit
```

Soft limit：

開始 aggressive eviction。

Hard limit：

立即移除低 priority caches。

優先保留：

```text
visible tiles
current page
critical font state
```

優先丟：

```text
far pages
thumbnails
old zoom level
old tiles
background text
```

## 17. Zoom Cache Key

Tile cache 至少要考慮：

```text
Document
Page
Tile X
Tile Y
Render Scale
Rotation
Color Mode
```

但是 zoom scale 不應產生無限種類 cache。

考慮 quantized scale：

```text
1.0
1.25
1.5
2.0
3.0
4.0
```

UI 可以是任意 zoom：

```text
137%
```

但 renderer texture cache 可以選最接近合理 resolution。

請 benchmark UX 與 memory tradeoff。

## 18. Multi-threading

Rendering 不可以 block UI thread。

考慮：

```text
UI Thread

Render Scheduler

Worker Pool

GPU Upload Queue
```

Thread 數量不應：

```text
= CPU 核心數
```

直接無限制拉高。

例如 32-core CPU 不代表應同時 render 32 個 PDF tiles。

需要 benchmark：

- 2
- 4
- 6
- 8

workers。

目標是：

latency 優先，而不是 throughput 最大化。

## 19. GPU Rendering

如果 zpdf GPU backend 足夠成熟，可以優先評估。

但不要假設：

GPU 一定快。

必須 benchmark：

```text
simple text PDF

vector PDF

large image PDF

scanned PDF

CAD PDF

transparency-heavy PDF
```

如果某些 workload CPU renderer 更快：

允許 runtime strategy。

例如：

```text
CPU fast path

GPU fast path
```

不要為了技術漂亮犧牲實際速度。

## 20. UI Architecture

目標：

```text
native feel

minimal chrome

very low visual overhead
```

大致：

```text
┌────────────────────────────────────────────┐
│ ← → │ file.pdf          125% │ Search │ ⋮ │
├─────────┬──────────────────────────────────┤
│         │                                  │
│ Page 1  │                                  │
│         │              PDF                 │
│ Page 2  │                                  │
│         │                                  │
│ Page 3  │                                  │
│         │                                  │
└─────────┴──────────────────────────────────┘
```

Sidebar 預設可考慮關閉。

因為 thumbnail generation 本身就是成本。

使用者開 sidebar 時才建立 thumbnails。

## 21. Keyboard First

至少提供：

```text
Ctrl + O

Ctrl + F

Ctrl + P

Ctrl + +

Ctrl + -

Ctrl + 0

Ctrl + Mouse Wheel

Page Up

Page Down

Home

End

F11
```

並集中管理 shortcuts。

不要 scattered hardcoding。

## 22. Search

V0.1 不需要在 open document 時全文 index。

第一次：

```text
Ctrl + F
```

才初始化 search subsystem。

可以：

```text
current page
↓
near pages
↓
remaining document
```

逐步搜尋。

UI 必須允許：

```text
3 results found so far...
```

而不是：

等 2000 頁全部搜完才顯示。

## 23. Thumbnail

Thumbnail 只 render：

```text
currently visible sidebar rows
+
small prefetch margin
```

禁止一次 render：

```text
2000 thumbnails
```

## 24. Error Handling

PDF 是非常髒的格式。

要預期：

```text
malformed xref

broken fonts

invalid object

missing object

weird encoding

huge image

encryption

corrupted stream
```

Reader 不應：

```text
panic
```

請將 PDF error isolation 做好。

單頁 render failure 不應讓整個 application crash。

## 25. Security

PDF 是不可信輸入。

所有 parser / renderer input：

assume hostile.

至少注意：

- integer overflow
- decompression bomb
- giant bitmap
- giant page dimension
- infinite recursion
- malformed object tree
- malicious font
- memory exhaustion

為：

```text
Max decoded image dimension
Max bitmap allocation
Max nesting
Max recursion
Max object length
```

建立合理 guardrails。

## 26. Benchmark Harness

這是第一階段最重要的工作之一。

建立：

```text
fastpdf-bench
```

Benchmark 不依賴 GUI。

必須可以：

```text
cargo run -p fastpdf-bench -- file.pdf
```

輸出：

```text
Open Time

Metadata Time

First Page Render

Page Render

Peak RSS

CPU Time

Text Extraction

Thumbnail Render
```

最好輸出 JSON：

```text
{
  "file": "example.pdf",
  "open_ms": 17.3,
  "first_page_ms": 42.6,
  "rss_peak_mb": 81.2
}
```

以便 CI 做 regression tracking。

## 27. Test Corpus

建立：

```text
fixtures/
```

但不要 commit 有 copyright 問題的大型文件。

應分類：

```text
small-text

large-text

scanned

image-heavy

vector-heavy

cad

fonts

cjk

japanese

traditional-chinese

transparency

encrypted

malformed

large-page-count

large-file
```

台灣使用場景一定要包含：

```text
Traditional Chinese PDF
CJK fonts
embedded fonts
政府文件
醫療報告
掃描 PDF
```

## 28. 第一階段 Benchmark Competitors

至少比較：

```text
FastPDF

pdf-reader-gpui upstream

SumatraPDF

Adobe Acrobat Reader
```

如果容易：

再加：

```text
Chrome
Edge
Firefox
```

主要 KPI：

```text
Cold launch

Warm launch

PDF open

Time to first visible page

Idle RAM

Peak RAM

Scroll responsiveness

Zoom latency
```

## 29. Performance Metrics

初期目標。

注意：

這些是 engineering targets，不是已驗證能力。

```text
Application executable:
盡量 < 30 MB

Idle RAM:
目標 < 50 MB

Small PDF:
首頁盡量 < 200 ms

Large PDF:
不需完整 scan 即可顯示

Idle CPU:
接近 0%

Network:
0

Telemetry:
0
```

如果達不到：

不要作弊。

必須紀錄真實結果。

## 30. Performance Regression

未來所有涉及：

- renderer
- cache
- scheduler
- document loading

的重大 PR，最好跑 benchmark。

Regression threshold 初期可以：

```text
> 10%
```

標記。

不要因為：

```text
code cleaner
```

但 RAM +80%。

## 31. Logging

Development：

允許詳細 tracing。

Production：

避免大量 log。

使用：

```text
tracing
```

或相似架構。

支援：

```text
FASTPDF_LOG=debug
```

但 default release：

低噪音。

## 32. Profiling

需要建立 profiling 文件：

```text
docs/profiling.md
```

Windows 優先考慮：

```text
Windows Performance Recorder

Windows Performance Analyzer

Visual Studio Profiler

cargo instruments equivalent where applicable
```

Rust：

```text
cargo flamegraph
```

若 Windows 支援狀況不佳可使用其他 profiler。

不要只靠猜測優化。

## 33. Windows Priority

第一版：

Windows 11 first.

不要急著同時追：

- macOS
- Linux
- Android

除非跨平台完全免費。

Windows 需要特別處理：

```text
File association

Drag & Drop

DPI scaling

High refresh rate

Multi monitor

Windows native file dialog

Print

Dark mode

Explorer integration
```

## 34. 不要使用 Electron

硬性原則：

FastPDF 不引入 Electron。

也不應為了方便改成：

```text
Chromium + PDF.js
```

整個產品的核心價值就是：

不使用龐大 browser runtime。

## 35. Tauri

目前也不建議改用 Tauri。

不是因為 Tauri 不好。

而是這個專案的目標是：

native Rust GPU reader。

因此先維持 GPUI。

除非經過 benchmark / technical blocker 能證明 GPUI 不適合，再提出 ADR。

## 36. Dependency Policy

任何新 dependency 都問：

```text
Why?
Size?
Compile impact?
Runtime memory?
License?
Maintenance?
Can std do it?
```

不要：

為了一個 helper function 引入 30 crates。

## 37. License

長期希望保留商業化選項。

優先：

```text
MIT

Apache-2.0

BSD
```

對：

```text
GPL

AGPL
```

dependency 必須特別審查。

尤其不要因為 fork / copy code 不小心污染整體 license。

建立：

```text
THIRD_PARTY_LICENSES.md
```

## 38. AI 開發規則

如果使用 Claude / Codex 持續開發：

禁止一次進行：

```text
"rewrite the entire project"
```

請拆成：

```text
baseline
↓
test
↓
small architecture change
↓
benchmark
↓
commit
↓
next change
```

每次效能優化必須回答：

```text
Before

After

Why

Tradeoff
```

## 39. ADR

重大架構決策請建立：

```text
docs/adr/
```

例如：

```text
0001-use-gpui.md

0002-pdf-engine-abstraction.md

0003-tile-rendering.md

0004-memory-budget.md

0005-zpdf-backend.md
```

避免半年後不知道為什麼當初這樣設計。

## 40. 第一個 Milestone

### M0 — Audit

先不要重寫。

請：

- Clone / fork pdf-reader-gpui
- 確認 Windows build
- 閱讀 workspace
- 找出：
  - UI entrypoint
  - document loading
  - render pipeline
  - cache
  - thread model
  - Hayro coupling
- 產生：

```text
docs/architecture-current.md
```

內容：

```text
current architecture
dependency graph
render flow
thread flow
memory ownership
technical debt
```

## 41. 第二個 Milestone

### M1 — Baseline Benchmark

不要優化。

先量。

建立：

```text
fastpdf-bench
```

測：

```text
open
first render
page render
text extraction
peak memory
```

保存：

```text
benchmarks/baseline.json
```

## 42. 第三個 Milestone

### M2 — Engine Isolation

建立：

```text
fastpdf-engine-api
```

將 Hayro 隔離。

完成後：

GPUI 層不能：

```text
use hayro::...
```

只有：

```text
fastpdf-engine-hayro
```

可以。

## 43. 第四個 Milestone

### M3 — zpdf Proof of Concept

加入：

```text
fastpdf-engine-zpdf
```

先做到：

```text
Open

Page Count

Page Size

Render Page
```

不用一次完成所有功能。

建立 runtime / compile feature：

```text
--features engine-zpdf

--features engine-hayro
```

或者其他更合理方案。

## 44. 第五個 Milestone

### M4 — Renderer Comparison

同一批 PDF 比較：

```text
Hayro

zpdf
```

測：

```text
correctness

open latency

first page latency

CPU

memory

render latency
```

產生：

```text
docs/engine-comparison.md
```

不要先決定誰贏。

讓數據決定。

## 45. 第六個 Milestone

### M5 — Tile Renderer

建立：

```text
RenderTile

TileCache

RenderScheduler
```

先只支援 current viewport。

完成後再加入：

```text
prefetch
```

## 46. 第七個 Milestone

### M6 — Memory Budget

建立：

```text
MemoryBudgetManager
```

所有重要 cache 都必須可觀察：

```text
current bytes

entry count

hit rate

miss rate

eviction count
```

Development overlay 可以顯示：

```text
RAM
FPS
tiles
render queue
cache
```

Release 預設關閉。

## 47. 第八個 Milestone

### M7 — UX

性能架構穩定後才開始 polish：

```text
toolbar

sidebar

search UI

keyboard shortcuts

recent files

settings

dark mode
```

不要反過來。

## 48. 第一輪請不要做的事情

Claude 接手後不要：

- 全面 rewrite。
- 換 UI framework。
- 引入 Electron。
- 做 AI 功能。
- 做 PDF editing。
- 大量增加 dependency。
- 為了美觀重寫 CSS / UI。
- 一開始建立 plugin system。
- 一開始完整實作兩套 renderer。
- 還沒 benchmark 就宣稱比較快。

## 49. 最優先回答的技術問題

Audit 完成後，請回答：

### Q1

pdf-reader-gpui 是否值得直接 fork？

還是：

建議建立新的 FastPDF workspace，再移植部分 code？

請說明理由。

### Q2

目前 Hayro coupling 有多深？

### Q3

GPUI 是否存在：

- Windows rendering limitation
- scrolling limitation
- texture limitation
- high-DPI limitation

會影響 Reader？

### Q4

zpdf 現況是否真的適合 interactive PDF Reader？

不要只讀 README。

請查看：

- API
- examples
- issues
- code architecture

### Q5

zpdf GPU backend 是否適合：

```text
tile-based viewport rendering
```

### Q6

是否有必要保留：

```text
Hayro fallback
```

### Q7

目前最可能的性能瓶頸會在哪：

```text
PDF parse

fonts

image decode

rasterization

GPU upload

texture cache

GPUI layout

scroll
```

## 50. Claude 第一個任務

請現在開始執行：

### Step 1

Audit：

```text
pdf-reader-gpui
```

以及：

```text
zpdf
```

必要時：

```text
hayro
```

### Step 2

產出：

```text
docs/PROJECT_AUDIT.md
```

包含：

```text
Executive Summary

Current pdf-reader-gpui Architecture

Renderer Architecture

Threading

Memory Model

Hayro Integration

zpdf Architecture

Licensing

Risks

Recommended Fork Strategy
```

### Step 3

提出：

```text
FastPDF Workspace Structure
```

請列出：

- crate
- responsibility
- dependencies

### Step 4

設計：

```text
PdfEngine trait
```

與核心：

```text
Document
Page
Viewport
RenderRequest
RenderTile
TextLayer
```

domain model。

### Step 5

設計 Benchmark Harness。

至少支援：

```text
fastpdf-bench open file.pdf

fastpdf-bench render file.pdf --page 1

fastpdf-bench full file.pdf
```

## 51. 最終交付格式

第一輪不要直接大改 code。

先提供：

```text
1. Repository audit

2. Architecture diagram

3. Dependency analysis

4. License analysis

5. Proposed workspace

6. PdfEngine API

7. Benchmark plan

8. Migration plan

9. Risks

10. Recommended first PR
```

然後才進入 coding。

## 52. 專案最高原則

FastPDF 的定位不是：

Rust 寫的 PDF Reader。

而是：

一個因為 architecture 正確，所以快、輕、低延遲的 PDF Reader。

Rust 只是達成這件事的工具。

我們真正追求的是：

```text
Open PDF
↓
Immediately see document
↓
Scroll
↓
No lag
↓
Zoom
↓
No freeze
↓
Close
```

使用者甚至不需要知道：

它是 Rust 寫的。

如果使用者只感覺：

「這 PDF Reader 怎麼這麼快？」

那才算成功。

## 53. 最終產品哲學

每增加一項功能，都先問：

它是否會讓使用者開 PDF 變慢？

如果答案是「可能會」：

就必須：

- lazy load
- isolate
- defer
- disable by default
- 或不加入

FastPDF 的優勢不能隨著功能增加逐漸消失。

## 核心優先序

永遠維持：

```text
1. Latency

2. Memory

3. Responsiveness

4. Correctness

5. Compatibility

6. Features
```

其中 correctness 與 security 不能為了效能被破壞。

## 開始工作

現在請先完成 repository audit。

不要直接進行全面重構。

先理解 upstream，建立 baseline，確認技術風險，再決定第一個 PR。
