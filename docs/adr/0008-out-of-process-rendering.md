# ADR 0008 — Out-of-Process Rendering（render host process）

- 狀態：Accepted（2026-10-05）。PR 1–4 已實作（`crates/fastpdf-engine-remote`）。**Windows 的預設 engine 是 `hayro-isolated`**；`--engine hayro` 在 process 內 render，`fastpdf-bench` 一律在 process 內。PR 5（降低權限）還沒做。實作與本文的差異見〈實作現況〉，驗收見〈PR 4 第二輪〉，每個 tile 的 CPU 見〈PR 4 第三輪〉。
- 日期：2026-10-04
- 相關 spec：§12、§18、§24、§25、§29、§33；風險：`docs/PROJECT_AUDIT.md` R1、R10；`docs/audit/hayro.md` R1–R3
- 原型：scratchpad 的 `oop/proto`（不在 repo 內），以 path dependency 指向 HEAD `c69460c` 的 `git archive` 快照，避免受其他 agent 未 commit 的修改影響

## Context

Spec §24 要求「單頁 render failure 不應讓整個 application crash」，§25 要求所有輸入一律視為 hostile。目前所有 engine 呼叫都經過 `GuardedDocument`（`catch_unwind`、request 驗證、bitmap 上限），但有三類失敗它攔不住：

| 失敗 | 為什麼攔不住 | 已知案例 |
|---|---|---|
| stack overflow | Windows 上直接以 `STATUS_STACK_OVERFLOW` 結束 process，不會 unwind | Hayro 10,000 層 `/Indexed` 色彩空間鏈（`docs/audit/hayro.md`） |
| 記憶體配置失敗 | Rust 的 allocation failure 會 abort（`0xC0000409`） | 1 KB 級檔案可配置 1–2 GB；flate bomb |
| 無上限的運算 | engine 沒有取消點時，worker 一直被占住 | 24 層 form XObject DAG 32.7 s |

另外還有一類：網路磁碟上的 mmap 在斷線時觸發 `EXCEPTION_IN_PAGE_ERROR`（R10），一樣會讓 process 直接結束。

**現況（本 ADR 的量測）**：HEAD 的 Hayro adapter 已有 guardrail（靜態掃描、預先解譯預算、bomb 預解壓），上表的已知 hostile 檔在 in-process 下都會以 `LimitExceeded` 收場。例如 `/Indexed` 鏈回報 nesting depth、1 GiB bomb 回報 object size、form DAG 在 10 s 時回報 render time、20000² 影像回報 decoded image size。zpdf 則以自身預算解完 bomb，peak 約 196 MiB。

換句話說，已知案例都有對策，**但 guardrail 只能擋已知的形狀**。新的 decoder bug、engine 升級造成的退化、尚未加深度上限的遞迴路徑、unsafe decoder 的記憶體破壞，都還是會讓整個 reader 消失。把 render 移到獨立 process 是 Chromium、Edge PDF、Adobe Reader 的共同做法，也是 R1 中期緩解的選項。本 ADR 以原型量測它的成本。

## Decision

### 1. 架構

```text
fastpdf.exe（UI process）                                 render host（每份文件一個）
┌───────────────────────────────────────────┐            ┌────────────────────────────────┐
│ UI / DocumentSession / scheduler（不變）  │            │ host main：讀 request、分派     │
│   └─ GuardedDocument（不變）              │  命令 pipe │ worker pool（W 條 thread）      │
│        └─ RemoteDocument : EngineDocument ├───────────►│   GuardedDocument               │
│             slot pool、watchdog、restart  │◄───────────┤     └─ Hayro / zpdf adapter      │
│                                           │  回應 pipe │   render 直接寫進 slot          │
│   tile slot section（共享記憶體）◄────────┼────────────┼──► 同一個 section 的 view       │
│   文件 file handle（DuplicateHandle）─────┼────────────┼──► 唯讀 mapping / 讀取         │
└───────────────────────────────────────────┘            └────────────────────────────────┘
        Job Object：記憶體上限、KILL_ON_JOB_CLOSE、DIE_ON_UNHANDLED_EXCEPTION、UI 限制
```

#### 1.1 `RemoteEngine` / `RemoteDocument`

- `RemoteEngine` 實作 `PdfEngine`，`RemoteDocument` 實作 `EngineDocument`，`open_guarded(&RemoteEngine, …)` 照常回傳 `GuardedDocument`。因此 core、UI、scheduler、搜尋、列印（`fastpdf_print::print` 吃 `&GuardedDocument`）**完全不用改**。parent 端的 `GuardedDocument` 仍做 request 驗證、bitmap 上限與 panic 隔離，作為第二道防線。
- `EngineDocument` 的方法都是同步的，由 scheduler 的 worker thread 呼叫。`RemoteDocument::render` 的流程：
  1. 取一個空閒 slot，送出 `Render { id, page, scale, rotation, region, slot, deadline }`。
  2. 在這次 request 自己的 condvar 上等待，每 2–5 ms 檢查一次 `CancelToken`（`CancelToken` 是 `AtomicBool`，沒有通知機制）。
  3. 收到 `Done` 後，把 slot 複製進呼叫端的 `PixmapMut`（1 MiB 約 13–40 µs），再歸還 slot。
- 取消：送出 `Cancel { id }`，立即回傳 `EngineError::Cancelled`。**但 slot 要等 host 回覆 `Done`／`Cancelled` 才能歸還**，否則 host 可能寫進已重新分配的 slot。host 端為每個 request 建立 `CancelToken`，收到 `Cancel` 時設定它，engine 的合作式取消照常生效。
- `page_count` 在 `Opened` 時一起帶回；`page_info` 在 parent 快取（`GuardedDocument` 本身也有快取）。`text_layer`、`outline`、`links`、`metadata` 以 request/response 傳回。Hayro 的 text layer 每頁約 285 KB，走 pipe 約 0.2 ms；必要時也可以走 slot。
- `trim_memory` 轉送給 host。`memory_usage` 由 host 回報 engine 內部用量，parent 另外直接讀 host 的 private bytes（overlay 顯示 host PID、private、重啟次數）。
- 大於一個 slot 的 request（列印 band 最大 8192×256×4 = 8 MiB、超大縮圖）由 `RemoteDocument` 切成符合 slot 大小的水平條，逐條 render 後拼回 target。這樣 slot pool 只需要一種尺寸。

#### 1.2 Render host：同一個 exe 的子命令，或獨立的小 exe

| | `fastpdf.exe --render-host`（同一個 exe） | `fastpdf-render-host.exe`（獨立小 exe） |
|---|---|---|
| 發佈 | 只有一個執行檔要簽章與發佈；不會有版本不一致 | 兩個執行檔；需要 protocol version 與 build id 握手 |
| 啟動（量測） | spawn＋結束：暖 10–12 ms；第一次執行新 binary 192–211 ms（21.4 MB，含防毒掃描） | 暖 12–14 ms（console 版）；第一次 122–135 ms（10.5 MB，含兩個 engine） |
| 冷啟動實務 | parent 本身就是這個 image，已經被讀入、掃描過，所以 host 一律是暖啟動 | 安裝或更新後第一次多付一次掃描成本 |
| 載入的 DLL | 靜態 import 會全部載入：user32、gdi32、d3d11、dxgi、dcomp、dwrite、uiautomationcore、shell32、comctl32、winspool 等 26 個 | 只有 kernel32、ntdll、CRT |
| 沙箱 | 因為 import user32／gdi32，**無法啟用 win32k lockdown**（`ProcessSystemCallDisablePolicy`）；每個 DLL 的初始化也擴大攻擊面 | 可以啟用 win32k lockdown；攻擊面最小 |

**建議**：第一版用**同一個 exe 的子命令**（PR 2–4），把 host 的入口放在 `main` 最前面（GPUI 初始化之前）分派，保持發佈簡單。等到要做 win32k lockdown 時（PR 5），再拆成獨立的小 exe；protocol 從第一天就帶 version 與 build id，屆時可以直接切換。

#### 1.3 命令通道（named pipe）

- 兩條**單向** byte-mode pipe，一條送 request、一條收 response。Windows 的同步 I/O 在同一個 handle 上會序列化，duplex pipe 一邊阻塞 `ReadFile` 時另一邊的 `WriteFile` 也會被卡住；拆成兩條就不需要 overlapped I/O 狀態機。parent 端用 overlapped handle，`ConnectNamedPipe` 搭配 `WaitForMultipleObjects([連線事件, host process])`，host 在連線前就死掉時不會卡住。
- 原型用隨機名稱並設 `FILE_FLAG_FIRST_PIPE_INSTANCE`、`PIPE_REJECT_REMOTE_CLIENTS`。**產品版改用 handle 繼承**：`STARTUPINFOEX` 加上 `PROC_THREAD_ATTRIBUTE_HANDLE_LIST`，只讓 host 繼承那幾個 handle。如此沒有全域名稱，不會被搶註（squatting），Low IL 的 host 也不需要開啟有名物件的權限。
- Frame 格式：`u32 長度 + u8 種類 + little-endian 欄位`，單一 frame 上限 64 MiB。decoder 對所有長度做邊界檢查，並且要有 fuzz target：host 可能已被攻破，它回來的資料一律不信任。parent 還要驗證 `Done` 的 id 與 slot 屬於自己發出的 request、尺寸等於要求的 region。
- 主要訊息：`Hello { protocol, build_id }`、`Open { file_handle | section_handle, password }`、`Opened { page_count, first_page_info }`、`Render`、`Cancel`、`Done { id, outcome, partial }`、`PageInfo`、`TextLayer`、`Outline`、`Links`、`Metadata`、`Trim`、`Quit`。

#### 1.4 Tile 像素：共享記憶體 slot

- parent 以 `CreateFileMappingW(INVALID_HANDLE_VALUE, …)` 建立 pagefile-backed section，切成 N 個固定大小的 slot。tile 含 gutter 為 516²×4 = 1,065,024 B，slot 取 1.0625 MiB。N = 2 ×（render worker 數＋縮圖 lane）≈ 10，合計約 11 MiB。
- host 把 `PixmapMut::from_slice` 直接建在 slot 上，engine 寫入共享記憶體，host 端零複製；parent 端複製一次進自己的 tile buffer（GPUI 的 `RenderImage` 需要自有的 `Vec`）。
- slot 的所有權由 parent 的 free list 管理：送出 request 時配出，收到該 request 的終結回覆（`Done`／`Cancelled`／錯誤）或 host 已死時才收回。section 在 session 期間一直重用，不做每個 tile 的配置。
- 量測確認 pagefile-backed section 不計入任一方的 private bytes：parent 與 host 各自映射 16 MiB section，private 只有 0.8–1.3 MiB。它計入 system commit 一次，被碰到的頁面計入兩邊的 working set。

#### 1.5 文件 bytes 的共享

- **檔案的開啟權在 parent**。parent 以不含 `FILE_SHARE_WRITE` 的 share mode 開檔，在文件存活期間一直持有這個 handle（延續 ADR 0006 的不可變保證），再用 `DuplicateHandle` 把唯讀 handle 交給 host。好處：
  - host 重啟之間檔案不會被改掉；
  - 沒有 path 重新解析的 TOCTOU；
  - Low IL／AppContainer 的 host 不需要以路徑開啟使用者檔案的權限。
- **大檔（> 64 MiB）**：host 從 handle 建立唯讀 file-backed mapping。file-backed 頁面由 cache manager 管理，parent 若也映射同一個檔案（例如未來的另存、雜湊、in-process fallback），實體頁面是共用的，兩邊都不算 commit。
- **小檔（≤ 64 MiB）**：OOP 模式下 parent **不再讀檔**（core loader 新增只開 handle 的策略），由 host 讀進自己的記憶體。總量和今天 in-process 一樣，只是從 parent 移到 host。
- **沒有檔案的來源**（記憶體中的下載、剪貼簿、解壓縮後的附件）：parent 把 bytes 複製一次進 pagefile-backed section，然後丟掉自己的 `Vec`，把 section 以唯讀 handle 交給 host。兩邊的 view 共用實體頁面，commit 只算一次。
- 附帶效果：網路磁碟 mmap 的 `EXCEPTION_IN_PAGE_ERROR`（R10）只會殺掉 host。

### 2. 失敗處理

**偵測**（四個來源，以先到者為準）：

1. **回應 pipe 斷線**：parent 的 reader thread 讀到 EOF／`ERROR_BROKEN_PIPE`。實測 host 死亡後 2–3 ms 內就會發現（stack overflow 案例）。
2. **process handle 被 signal**：monitor thread 等 `WaitForMultipleObjects`，取得 exit code，可以分辨 `0xC00000FD`（stack overflow）、`0xC0000409`（abort／配置失敗）、`0xC0000005`。
3. **Job Object completion port**：`JOB_OBJECT_MSG_PROCESS_MEMORY_LIMIT`、`JOB_OBJECT_MSG_ABNORMAL_EXIT_PROCESS` 直接告訴 parent 原因。配置失敗的 exit code 和其他 abort 相同，必須靠這個訊息才能分辨。
4. **per-request deadline**（hang）：預設取 `ResourceLimits::max_render_time`（20 s）加上餘裕。逾時就 `TerminateProcess`，實測 1.0–1.3 ms 內結束，之後視同 crash。

**重啟與頁面標記**：

- host 死亡時，所有在途 request 都收到錯誤並立即釋放 slot。在途 request 的頁面視為**嫌疑頁**。
  - 只有一頁在途：該頁記一次 strike。
  - 多頁在途：在新 host 上**逐頁重送**（序列化，不平行），找出真正的 culprit。
  - 沒有嫌疑的 request 直接重送。
- 重啟流程：spawn 新 host，送 `Open`（大檔 mmap、小檔讀取，實測 reopen 16–22 ms），再重送 request。
- 備用 host：任何一次 crash 後就預先 spawn 一個待命 host（不開文件），下一次重啟只剩 open 的成本。
- **同一頁連續 crash**：第 2 次 strike 後，該頁在本次 session 標為永久失敗，不再自動重試。畫面顯示頁面錯誤（現有的 page error 繪製），回傳 `EngineError`。建議在 engine-api 新增 `EngineError::HostCrashed { code, cause }`；`EngineError` 是 `#[non_exhaustive]`，不算破壞性變更。使用者可以手動「重試此頁」。
- **crash storm**：同一文件 60 s 內 host crash ≥ 3 次，或累計 ≥ 5 次，就停止自動重啟，顯示文件層級提示，並提供以另一個 engine 或更嚴格限制重新開啟。
- **一份文件一個 host**：一份 hostile 文件只會拖垮自己的 host。V0.1 是單文件視窗，代價只有一個 process。

### 3. Windows 隔離與資源限制

host 在 `CREATE_SUSPENDED` 狀態下建立，先 `AssignProcessToJobObject` 再 `ResumeThread`，避免在進入 job 之前就開始執行。Job 設定：

| 設定 | 作用 | 實測 |
|---|---|---|
| `JOB_OBJECT_LIMIT_PROCESS_MEMORY` | 每個 process 的 commit 上限，bomb 與配置失敗只影響 host | 128 MiB 上限下，zpdf 解 512 MiB bomb 的那一頁讓 host 以 `0xC0000409` 結束，69–90 ms 內偵測到，parent private 0.6 → 0.7 MiB 不變，其餘頁正常。主動配置到上限（512 MiB）的測試：128–130 ms 結束，14–21 ms 內 process 完全消失 |
| `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` | parent 消失（含被強制結束）時 host 跟著結束 | 中間層 parent 被 `TerminateProcess` 後 1.3–1.8 ms，host 就被系統結束 |
| `JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION`，加上 host 內 `SetErrorMode(SEM_NOGPFAULTERRORBOX)` | 不跳 WER 對話框、不等 dump | stack overflow 2–3 ms 內偵測；`abort()`（fail-fast）約 55 ms，WER 的 fail-fast 路徑仍有成本，可接受 |
| `JOB_OBJECT_LIMIT_PROCESS_TIME`（CPU 時間） | **不建議當主要手段** | 設 1 s，實際在 6.9–7.5 s 後才以 `0xC0000044`（`STATUS_QUOTA_EXCEEDED`）結束，粒度太粗；而且長壽命的 host 會累積正常的 CPU 時間。改由 parent 的 per-request deadline 處理（2 s deadline → 1 ms 內結束） |
| `JOB_OBJECT_LIMIT_ACTIVE_PROCESS = 1` | host 不能再開子 process | 未量測，零成本 |
| `JOBOBJECT_BASIC_UI_RESTRICTIONS` | 禁止讀寫剪貼簿、存取其他 process 的 USER handle、改系統參數、切換 desktop | 未量測，零成本 |

記憶體上限的建議值：預設 1.5 GiB，可設定。正常文件的 host peak 在 75–92 MiB（A0 向量圖、8 條 thread），bitmap 上限是 256 MiB，影像解碼上限是 256 MP。上限應遠高於正常 peak，只用來擋失控。

**降低權限（只分析，未實作）**：

- **Restricted token**（`CreateRestrictedToken`：`DISABLE_MAX_PRIVILEGE`、deny-only 的 Administrators 與 Users 群組）**＋ Low integrity level**（`SetTokenInformation(TokenIntegrityLevel)`，以 `CreateProcessAsUserW` 啟動）：
  - 好處：engine 被打穿（例如 unsafe decoder 的記憶體破壞）時，攻擊者無法寫入使用者檔案、HKCU 與 medium IL 的物件，配合 UI 限制也碰不到其他視窗。
  - 需求：
    - 檔案一律由 parent 開好再 duplicate handle（§1.5 已經這樣設計）。
    - pipe 與 section 用 handle 繼承，不用有名物件，否則要加 low mandatory label（`S:(ML;;NW;;;LW)`）。
    - 系統字型（`C:\Windows\Fonts`）與使用者字型（`%LOCALAPPDATA%\Microsoft\Windows\Fonts`）在預設的 no-write-up 政策下仍可讀，CJK fallback 不受影響。
    - host 不寫暫存檔。
  - 成本：spawn 多建立一次 token（預估數 ms，PR 5 驗收時量測）；除錯時需要以相同權限重現。
- **Win32k lockdown**（`ProcessSystemCallDisablePolicy`，Chromium renderer 的做法）：host 只做記憶體內 render，不需要 GDI／USER。列印的 GDI 呼叫留在 parent，`fastpdf-print` 本來就是在 parent 呼叫 `render` 再 `StretchDIBits`，正好分工。前提是 host exe 不 import user32／gdi32（見 §1.2）。
- **AppContainer**：隔離最強，連使用者檔案與網路都要 capability。代價是要建立 profile、處理 ACL，使用者字型也讀不到。留作長期選項。
- 其他低成本 mitigation：CFG、ACG（不產生動態程式碼，我們沒有 JIT）、image load policy（禁止遠端與低完整性 image）。

### 4. 成本量測

環境：AMD Ryzen 9 9950X（16C/32T）、125 GB RAM、Windows 11 10.0.26200、release（thin LTO）、Hayro 與 zpdf adapter 取自 HEAD `c69460c`。量測期間 CPU 負載 6–11%，開始與結束時都沒有其他 cargo／rustc 在執行（其他 agent 可能在中途短暫編譯）。Hayro 各組態跑 3 次取中位數，zpdf 兩組各跑 2 次取平均；IPC 每次 1000 回合，共 3 次；啟動時間每組 15 次。原型 render 一律 scale 1.5、512² tile；in-process 與 OOP 平行度相同（4 條 thread 對 4 個在途 request＋host 4 個 worker，另有一組 8 對 8）。

**Host 啟動**

| 項目 | 結果 |
|---|---|
| spawn＋結束（暖） | GPUI exe（21.4 MB）10–12 ms；小 exe 12–14 ms |
| spawn＋結束（第一次執行新 binary） | GPUI exe 192–211 ms；小 exe 122–135 ms（主要是防毒掃描新 image） |
| host ready（spawn → 兩條 pipe 連上 → `Ready`） | 中位數 13–15 ms，p95 15–20 ms |
| 經 host 開檔 vs in-process | 2000 頁 7.6 vs 7.5 ms；300 頁 1.1 vs 1.1 ms；6 頁影像 2.5 vs 2.2 ms（pipe 只多約 0.1 ms） |
| 閒置 host private | 0.8 MiB；開檔後 6–10 MiB |
| crash 後重啟 | 新 host ready 15–50 ms（舊 host 還在結束時，中位數約 25 ms）；重啟加 reopen 16–22 ms |

**IPC**

| 項目 | 結果 |
|---|---|
| 空命令往返 | 中位數 9–14 µs，p95 16–22 µs |
| 1 MiB tile 經共享記憶體（host 寫滿 slot＋parent 複製出來＋往返） | 中位數 37 µs，p95 52–56 µs |
| 同一個 tile 走 pipe | 中位數 600 µs，p95 約 785 µs（**慢 16 倍**，所以像素一律走 slot） |
| 8 個在途的 pipeline | 53–59k tiles/s（slot 在 cache 內，是上限值） |
| 對照：in-process 1 MiB memcpy | 12.8 µs |
| 實際 render 的每 tile IPC overhead | 中位數 0.05–0.18 ms |

**Render：in-process vs out-of-process（Hayro，另列兩組 zpdf）**

| 檔案 | 第一頁 in-proc | 第一頁 OOP（host 已啟動） | OOP 含 host 啟動 | tiles/s in-proc vs OOP |
|---|---:|---:|---:|---:|
| small-text 3 頁 | 5.7 ms | 6.4 ms | 20.0 ms | 2966 vs 2869 |
| dense-300p（前 10 頁、60 tiles） | 12.8 ms | 12.2 ms | 27.7 ms | 688 vs 676 |
| photos-rgb-jpeg 6 頁 | 26.0 ms | 24.0 ms | 40.0 ms | 318 vs 323 |
| A0 平面圖（70 tiles，4 thread） | 211.8 ms | 210.0 ms | 223.3 ms | 339 vs 338 |
| A0 平面圖（8 thread） | 129.3 ms | 128.6 ms | 143.4 ms | 566 vs 568 |
| 正體中文公文 2 頁 | 9.1 ms | 9.6 ms | 23.3 ms | 3679 vs 3978（工作量極小，屬雜訊） |
| zpdf dense-300p | 34.3 ms | 31.6 ms | 47.0 ms | 416 vs 418 |
| zpdf A0 平面圖 | 237.6 ms | 239.0 ms | 254 ms | 335 vs 334 |

結論：**host 啟動之後，吞吐量與第一頁時間和 in-process 沒有可量測的差異**（差距落在雜訊內）。額外成本只有 host 啟動的 13–15 ms，可以完全藏起來（見 §5）。

**記憶體分攤**（捲動結束後的 private，MiB；不含 UI：真正的 app 中 GPUI／D3D 基線約 105–120 MiB，會留在 parent）

| 檔案 | in-process | OOP parent | OOP host（peak） |
|---|---:|---:|---:|
| Hayro dense-300p | 49.5 | 1.3 | 49.8（49.8） |
| Hayro A0 平面圖 | 49.8（peak 83.0） | 1.1 | 49.9（75.0） |
| Hayro photos | 37.5（peak 58.7） | 0.8 | 38.2（50.6） |
| zpdf dense-300p | 12.7（peak 25.4） | 1.3 | 12.7（17.1） |

engine 的快取與文件 bytes 整批移到 host，總量不變；額外成本是 host 的 process 基線（約 1 MiB）加上 slot section（約 11 MiB commit，不計入任何一方的 private）。

**Hostile 輸入與 Job 記憶體上限**（每頁一個 tile，15 s deadline）

| 檔案 | Hayro（job 512 MiB） | zpdf（job 512 MiB） | zpdf（job 128 MiB） |
|---|---|---|---|
| `malformed/decompression-bomb-512mib.pdf` 第 2、3 頁 | `LimitExceeded(ObjectSize)`，host 存活 | 解完，peak 196–198 MiB | **host 被終止**（`0xC0000409`），69–90 ms 內偵測，重啟＋reopen 18–22 ms，第 4 頁正常，parent 不受影響 |
| `malformed/decompression-bomb-nested-flate.pdf` 第 2 頁 | `LimitExceeded(ObjectSize)` | 解完，peak 196 MiB | **host 被終止**，69 ms 偵測，重啟後第 3 頁正常 |
| 1 GiB content bomb、20000² 影像 | `LimitExceeded` | 解完，peak 約 197 MiB | **host 被終止**，70–77 ms 偵測 |
| 10,000 層 `/Indexed`、form DAG 24／40 層、CCITT 40000²、`deep-nesting-content-100000` | `LimitExceeded`（nesting depth／render time 10 s／decoded image size）或正常 | 正常結束（DAG 0.75 s） | — |
| 注入：stack overflow | host `0xC00000FD`，2–3 ms 偵測 | | |
| 注入：無窮迴圈 | 2 s deadline → `TerminateProcess` 1 ms 內結束，重啟 15–18 ms | | |

所有案例中 parent（量測程式本身）都存活，private 不變。

原型還暴露了一個設計重點：host 的 worker 在 `GuardedDocument` **外面** panic（原型自己的 slice bug）時，該 request 永遠不會被回覆，parent 只會看到 timeout。所以 host 的每個 request 處理都要再包一層 `catch_unwind` 作為後備，並確保每個 request 一定有終結回覆。

### 5. 建議

**範圍**：Windows 上**所有文件都走 out-of-process**，不分大小也不分 engine。

- 不分大小：bomb 與深度攻擊和檔案大小無關（1 KB 級檔案就能配置數 GB）。
- 不分 engine：成本實測接近零，而 Hayro 的 guardrail 只能擋已知形狀，zpdf 的硬化程度更低。
- 依檔案來源調整的應該是**沙箱強度**，不是要不要 OOP：帶 Mark-of-the-Web（`Zone.Identifier` ADS，ZoneId ≥ 3）的檔案用 Low IL＋restricted token＋較低的記憶體上限；本機檔案先用 job 限制。
- in-process 模式保留給 benchmark、測試、非 Windows 平台，以及 host 無法啟動時的降級（顯示警告），並提供 `--in-process` 開關。

**Cold start KPI**：

- host ready 13–15 ms，第一次執行新 binary 時是 120–210 ms。這兩個數字都小於 GPUI 建立視窗的時間（B-8：window visible 約 171–183 ms）。
- 具體做法：在 `main` 一開始、`PendingOpen` 背景開檔的同一個位置，就 spawn host 並送出 `Open`，與 GPUI 初始化平行進行。視窗出現時文件通常已經開好，**第一頁時間預期不變**。
- 沒有命令列檔案時也預先啟動一個空的 host（0.8 MiB），讓「開檔」不必等 spawn。crash 後立即預備下一個待命 host。
- 用同一個 exe 時，host 使用的 image 已經被 parent 讀入並掃描過，不會再付第一次執行新 binary 的成本。

**實作拆成 5 個 PR**：

| PR | 內容 | 驗收條件 |
|---|---|---|
| 1. `fastpdf-host-proto` | 訊息型別、framing、`Hello { protocol, build_id }`，純 safe Rust | 所有訊息 round-trip 單元測試；過長或截斷的 frame 被拒絕；decoder fuzz target 跑 10 分鐘無 crash |
| 2. host 入口與 Windows IPC 層 | `fastpdf.exe --render-host`（在 GPUI 初始化前分派）；handle 繼承的 pipe（`PROC_THREAD_ATTRIBUTE_HANDLE_LIST`）；slot section；suspended＋job（記憶體上限、`KILL_ON_JOB_CLOSE`、`DIE_ON_UNHANDLED_EXCEPTION`、UI 限制、active process = 1）；完成埠原因回報 | 整合測試：kill-on-close（parent 被殺後 host 在 100 ms 內消失）；zpdf＋128 MiB 上限的 bomb 檔 → host 結束、原因為 memory limit、parent 存活；stack overflow 與無窮迴圈的注入測試 |
| 3. `RemoteEngine`／`RemoteDocument` | slot pool 與大型 region 分條、取消（slot 等到終結回覆才歸還）、deadline watchdog、crash 偵測、strike 與重啟策略、`EngineError::HostCrashed` | 既有的 adapter 測試套件改以 RemoteEngine 包裝 Hayro 執行並全數通過；`fastpdf-bench diff-corpus` in-process vs remote：84 檔 0 像素差異；throughput 差距 ≤ 5%；注入測試：同頁 2 次 crash 後標為永久失敗，其他頁不受影響 |
| 4. core／app 整合 | loader 新增「只開 handle、不讀檔」策略並 duplicate 給 host；`main` 開頭平行 spawn；`--in-process` 開關；頁面與文件層級的錯誤 UI；overlay 顯示 host PID、private 與重啟次數 | B-8：window visible 與第一頁清晰的中位數和 in-process 相差 ≤ 5 ms；idle CPU 不變；parent＋host 總 private ≤ in-process＋20 MiB；hostile 語料（`fixtures/generated/malformed/` 與 hostile 產生器的檔案）全跑一遍，UI process 0 次結束 |
| 5. 權限降低 | restricted token＋Low IL（MOTW 檔案）、拆成獨立小 exe 並啟用 win32k lockdown、CFG／ACG | host 無法寫入 `%USERPROFILE%`（測試）；CJK 與非內嵌字型的 fixture 輸出與 PR 3 一致；spawn 時間增加 ≤ 5 ms |

## 實作現況（2026-10-04）

- **程式**：
  - `crates/fastpdf-engine-remote`：
    - `protocol/`：std-only 的二進位格式，含 fuzz 測試；
    - `win/`：pipe、section、process，是唯一含 `unsafe` 的部分，每個 block 都有 `// SAFETY:` 說明；
    - `host.rs`、`client.rs`、`remote.rs`；
    - `policy.rs`：crash 歸責與重啟策略；
    - `gate.rs`：可疑頁單獨執行。
  - App 端：`fastpdf --render-host` 在 `main` 最前面分派；`--engine NAME-isolated`（或 `FASTPDF_ENGINE`）是 opt-in 的入口；host 無法啟動時改回 in-process，並寫一行警告。
- **與本文設計的差異**：
  1. **pipe 以名稱開啟，不用 handle 繼承**（§1.3）。
     - 做法：隨機名稱，加上 `FILE_FLAG_FIRST_PIPE_INSTANCE`、`PIPE_REJECT_REMOTE_CLIENTS`，連線後檢查 client PID；host 端以 `SECURITY_IDENTIFICATION` 開啟，parent 無法冒用 host 的身分。
     - 原因：handle 在可繼承狀態下時，同一 process 其他 thread 的 `CreateProcess`（例如 `std::process::Command`）也會繼承到。這份複本會讓 pipe 在 host 死後仍保持開啟，延後 crash 偵測。
     - 代價：多了一個全域名稱，被搶註時最多只會讓啟動失敗。PR 5 的 Low IL host 需要替 pipe 加上 low mandatory label。
  2. **用 `PROC_THREAD_ATTRIBUTE_JOB_LIST` 直接在 job 內建立 host**，取代 §3 的「`CREATE_SUSPENDED` → `AssignProcessToJobObject` → `ResumeThread`」。
     - 原因：舊做法有空窗。測試 process 在 `CreateProcessW` 與 `AssignProcessToJobObject` 之間結束時，host 永遠停在 suspended 狀態，不屬於任何 job。測試中發生過 2 次。
     - 結果：改成 `JOB_LIST`，並讓 reader thread 只持有 `Weak`（原本的 `Arc` 會讓 `RemoteEngine` drop 後待命 host 仍然存活）之後，4 次完整測試都沒有殘留的 host。
  3. **超過 slot 大小的 render 使用獨立的 section**，不分條（§1.4），結果與 in-process 逐位元組相同。
  4. **錯誤種類**：沒有 `EngineError::HostCrashed`，改為兩種（engine-api）：
     - `HostExited(HostExit { reason, permanent })`：host 在處理請求時結束。`reason` 是 `Crashed { exit_code }`、`MemoryLimit` 或 `Deadline`；`permanent` 表示這個請求不會再送給 host（同頁第 2 次 strike，或文件已停止重啟）。
     - `Unavailable`：暫時無法服務（重啟失敗、host 沒有回應、UI thread 的幾何查詢超過 1 s、重送 3 次仍失敗）。
     - `is_transient()` 為真的錯誤（`Unavailable` 與非永久的 `HostExited`）不會被快取，也不會被 core 當成頁面的最終錯誤（見〈PR 4〉）。
  5. **超過 slot 的 render、文件 bytes 的共享**依 §1.4、§1.5 實作，但 deadline 改成由 reply reader thread 檢查：有請求在途時每 50 ms 一次；沒有請求時不設 timer，第一個請求進入時以 event 喚醒（閒置時 0 次喚醒）。
  6. **待命 host 的補充延後 1 s**：開檔或重啟取走待命 host 之後，1 s 後才啟動下一個，讓它的啟動不和第一頁的 render 搶 CPU。這 1 s 內需要 host 時就地啟動。
- **UI thread 不等 host**：core 在 UI thread 上同步呼叫 `page_info`（`DocumentSession::resolve_pages`）。
  - **快取**：`RemoteDocument` 自己快取頁面幾何，快取不受 host 重啟影響。open 時先取前 64 頁，其餘頁面由背景 thread 每批 1024 頁補齊。
  - **cache miss**（只會發生在背景補齊之前）：
    - 不經過 admission gate 與 state lock，也不會自己重啟 host；
    - host 端由 4 條專用的幾何 thread 處理，不排在 render worker 後面；
    - UI thread 最多等 1 s。
  - **實測**：
    - 3000 頁全部命中快取：2.8 ms，host 已經結束也一樣；
    - cache miss 且唯一的 render worker 正忙：130 µs；
    - gate 被獨佔：165 µs；
    - host 卡住：1.014 s 後回報錯誤，其他頁的請求不受影響。
  - 超過 1 s 時回報 `Unavailable`；core 保留估計尺寸並稍後重試（見〈PR 4〉），不會變成永久的頁面錯誤。
- **取消與 admission**：被取消的請求，admission 要等到 host 送出該請求的最終回覆，或 host 結束，才會釋放；呼叫端仍然立即返回。
  - 實測：呼叫端在 203 ms 返回，下一個請求在 1.502 s、host 真正做完時才進入。
  - 這保證可疑頁仍然單獨執行，crash 的歸責也不會出錯。另外，只有恰好一個可歸責的請求在執行時才會歸責，否則視為不明。
- **驗收量測**（Hayro，release build，背景負載 4–10%）：
  - **正確性**：84 個 fixture 各取第一、中間、最後一頁，共 192 頁。186 頁整頁與 186 個 tile 逐位元組相同，6 頁的 render 錯誤也相同，192 個 text layer 相同。0 個差異，host 0 次 crash。
  - **捲動吞吐量**：
    - 需要 render 新內容時（11 次交替量測）：−0.3% 至 −5.0%，其中一次 +1.8%。
    - tile 全部命中 Hayro block cache 時：−14% 至 −29%，remote 仍有 6,300–12,000 tiles/s。差距來自兩次複製與三次 thread 交接。
  - **hostile 輸入**（`malformed/`，job 記憶體上限 64／32 MiB）：bomb 由 Hayro 自己的 guardrail 擋下；`deep-nesting-content-100000` 的第 2–3 頁讓 host 結束，重啟後第 4 頁正常 render，parent 的 private 只從 672 KiB 增加到 932 KiB。
  - **crash 偵測時間**：正常結束 1.4–2.2 ms、stack overflow 16–20 ms、abort 118–182 ms（經過 Windows Error Reporting）。
  - **成本**：
    - 多一個 process（約 3–5 MiB）；
    - tile section 10.6 MiB；
    - 文件複製一份；
    - 每個 tile 4–24 µs；
    - 啟動約 +15 ms。
  - **測試**：48 個單元測試、22 個整合測試、2 個 app 測試。protocol fuzz 預設 40k 次，各 fuzz 測試另外以 1M 次跑過一次（約 25 s）。
  - **修正 P1／P2 之後重新驗收**（背景負載 8–16%）：84 個 fixture 的比對仍然 0 差異。捲動吞吐量為 −0.1% 至 −3.7%，只有 2 頁的 gov-letter（每次 12 個 tile）是 −3.4% 至 −7.8%（修正前 −5.0% 至 +1.8%）。這一輪的修改沒有碰到每個 tile 的路徑，但無法從這幾次量測完全排除影響。
### PR 4（2026-10-04）

- **只交 handle**（§1.5）：
  - app 啟用 isolated engine 時把 loader 切到只開 handle 的模式（`loader::set_handle_only`）。有檔案的來源：parent 以不含 `FILE_SHARE_WRITE` 的 share mode 開檔，不讀也不映射；`SharedBytes` 帶著 `FileOrigin`（handle、長度、是否在網路磁碟），內容只在有人呼叫 `as_slice()` 時才讀取或映射。
  - `RemoteDocument` 以 `DuplicateHandle` 把唯讀 handle（`FILE_GENERIC_READ`）交給每個 host。host 對 ≤ 64 MiB 或網路磁碟上的檔案以 positional read 讀進記憶體，其餘以 `PAGE_READONLY` 映射，並在映射期間持有自己的 handle，所以檔案在 host 使用期間一直無法以寫入模式開啟。
  - 沒有檔案的來源維持複製進唯讀 section。開檔時檔案已被其他程式以寫入模式開著（sharing violation），一樣走一般讀取＋section。
  - 修改前：≤ 64 MiB 的檔案 parent 讀一次再複製進 section；更大的檔案 parent 先 mmap，再整份複製進 pagefile section（800 MB 的檔案要 800 MB commit）。
  - 代價：isolated 模式下，**小檔在文件開著時也無法被其他程式覆寫**（in-process 時只有 > 64 MiB 的檔案會被鎖，ADR 0006）。host 重啟時要重新取得同一份不可變的 bytes，所以 parent 必須一直持有 handle。
- **網路磁碟（R10）**：`loader::is_network_path` 以 path prefix（UNC、`\\?\UNC\`）與 volume root 的 `GetDriveTypeW`（`DRIVE_REMOTE`）判斷；網路上的檔案在 parent 與 host 都只讀取、不映射（ADR 0006）。
- **暫時性錯誤與重試**（core）：
  - `resolve_pages`、tile、縮圖遇到 `is_transient()` 的錯誤時，保留估計尺寸、不顯示錯誤，退避重試：250 ms 起每次加倍，上限 8 s，最多 10 次（約 48 s），之後才以最後的錯誤作為頁面錯誤。
  - 重試時間到時由 session 的 wake hook 喚醒 UI。計時 thread 在第一次重試時才建立，沒有待重試的項目時以 condvar 無期限等待，不輪詢、沒有 idle CPU。
- **錯誤 UI**（en／zh-TW）：
  - 頁面訊息依原因顯示：繪製程式當掉（含結束代碼）、需要的記憶體超過上限、繪製時間過長、沒有回應；永久失敗另加「不會再重試」。
  - crash storm（停止自動重啟）時，文件區左下角顯示文件層級提示，說明當掉次數並請使用者重新開啟文件。
  - 開檔失敗時，host 當掉與無法使用也有各自的訊息。
- **診斷**：
  - `EngineDocument::host_status()`（engine-api 的 `HostStatus`）回報 host PID、private bytes、重啟與 crash 次數、永久失敗的頁、是否已停止重啟。
  - dev overlay 多一行 `host pid … restarts … crashes … failed …`，engine 自己的估計不再從 parent 的 private 扣除。
  - memory budget manager 的 external bytes 加上各文件 host 的 private bytes。
- **平行啟動**：`RemoteEngine::start` 不等 host，第一個 host 在背景 thread 啟動。app 用預期的 `EngineInfo`（同一個 engine 的 in-process 版本）建立 engine，第一次開檔才等 host（`RemoteEngine::ready`）；host 無法啟動時改用 in-process engine 並寫一行警告。命令列文件照舊在 `Startup::begin` 的 open thread 開啟。
  - 實測（測試用 host）：`start` 在 0.15 ms 內返回，host 9.9 ms 後就緒。
  - B-8 的 22 次執行（兩種模式）中，`document_opened` 都比 `window_visible` 早 5.7–58.6 ms，`first_page_exact` 與 `first_paint` 相差 ≤ 0.1 ms：經由 host 開檔與第一頁 render 都不在關鍵路徑上。
- **閒置 CPU**：reply reader 原本每 50 ms 醒來檢查 deadline，現在只在有請求在途時才計時。
  - 量測方式：不開 UI 的量測程式，開檔、render 第一頁、等 3 s 後量 10 s 的 CPU cycles（`QueryProcessCycleTime`），交替 2 次。
  - 結果：parent 從 20.6–21.0 降到 0.53–0.63 Mcycles；document host 2.1–2.3（in-process 時同一份 hayro 的背景活動是 2.9–4.2）；待命 host 0。
- **吞吐量（第一輪）**：目標（cold 與 cache 命中都在 in-process 的 5% 內）**只有部分達成**。第二輪以 pipelining 達成，見〈PR 4 第二輪〉。
  - 量測方式：scratch 量測程式。兩邊都從乾淨的 process 開始（in-process 在新的子 process，remote 在新的 host），先 render 第 1 頁（不計時），再以 2 條 thread render 第 2–9 頁，scale 2、512 px tile；cache 命中是同一批 tile 再 render 9 次取中位數。7 對交替，背景 CPU 17–29%，沒有編譯。
  - cold（新內容，修改前／後的中位數）：dense-300p −3.9%／+4.0%，photos −2.4%／−0.9%，都在 5% 內；頁面很簡單的 three-pages −9.0%／−9.6%、gov-letter −13.5%／−13.6%。
  - cache 命中（修改前／後）：three-pages −43%／−36%，photos −39%／−36%，gov-letter −45%／−31%，dense-300p −4%／+1%（雜訊內）。
  - 原因：每個 tile 多一次 IPC 往返與一次 1 MiB 複製。
    - IPC 往返：單 thread 量測（2000 次中位數），`metadata` 往返 50–66 µs，in-process 0.8–1.5 µs。一次往返要喚醒 4 條 thread：host 的命令 reader、host worker、parent 的 reply reader、等待中的 render thread。
    - 複製：slot 複製進 `fastpdf-render` 自有的 tile buffer。
    - 合計：cache 命中的 516 px tile 是 380–463 µs，in-process 是 281–316 µs。
  - 本輪做到的：兩端改成緩衝讀取，每個 tile 少 2 次 `ReadFile`，效果在雜訊內。
  - 要再降低，需要下列其中一項，本輪沒有做：
    - render 直接寫進最後的 tile buffer：要改 `fastpdf-render` 與 engine-api 的 buffer 所有權；
    - 改傳輸方式：每個 slot 一個完成 event，直接喚醒等待的 thread，或讓 worker 自己讀命令（leader/follower）。
  - 使用者實際看到的新內容 render，額外成本約 0.1 ms／tile。

### PR 4 驗收（第一輪）

- **環境**：
  - 硬體與 build：AMD Ryzen 9 9950X、Windows 11 26200、release build（thin LTO）、hayro。
  - 背景負載：其他 agent 的編譯與 benchmark，以及使用者的另一個 Node 測試套件（vitest，20 個 node process）。量測前確認沒有 cargo、rustc、link 在跑。bench-app 記錄的開始時 CPU 是 40–100%。
  - GUI 啟動共 24 次：
    - B-8 第一批 12 次（使用者的測試套件在跑，負載 45–100%）；
    - B-8 第二批 10 次（負載 40–100%）；
    - hostile 語料 2 次。
- **B-8**：A＝`--engine hayro`，B＝`--engine hayro-isolated`，ABAB 交替，以 `tools/bench-app` 量測，每次執行只啟動一次（`-Runs 1`）。

| 項目 | 條件 | 結果 | 判定 |
|---|---|---|---|
| 3 頁（`three-pages-platypus-times`） | `window_visible`／`first_page_exact` 中位數差 ≤ 5 ms | 第二批 3 對：`window_visible` A 236.1、B 233.9 ms（每對 B−A 的中位數 −2.1 ms）；`first_page_exact` A 236.1、B 239.0 ms（+2.9 ms）。每對的差距範圍 −909 至 +510 ms，由負載決定。第一批 3 對的 `window_visible` 差距為 −1364.2、+160.1、+123.0 ms | 第二批的中位數符合，但變異遠大於門檻 |
| 300 頁（`dense-300p-times`） | 同上，至少 3 對 | 第二批只有 2 對，每對 `window_visible` 的 B−A 為 −82.5 與 +301.2 ms；第一批 3 對為 −677.5、+273.6、+147.3 ms | **無法確認，未通過** |
| idle CPU | 不變 | bench-app 的 10 s idle CPU 兩種模式都在 15.6–312.5 ms 之間跳動（負載造成），無法比較；改以不開 UI 的 cycles 量測（見上），isolated 的總和 2.7–2.9 Mcycles／10 s，in-process 2.9–4.2 | 通過（以直接量測判定） |
| parent＋host 總 private | ≤ in-process＋20 MiB | idle 時 A 107.1–109.7 MiB，B 112.2–116.2 MiB，11 對的差距全部在 +4.8 至 +7.0 MiB（B 有 3 個 process：app、文件 host、待命 host）；peak 差 −1.3 至 +7.1 MiB | 通過 |
| hostile 語料經由 UI | UI process 結束 0 次 | 46 個檔（`fixtures/generated/malformed/` 29 個、hostile 產生器 17 個），每檔開啟、翻頁、最後一頁、放大、縮圖，共 783 步，跑 2 次。預設記憶體上限：host 0 次 crash。`FASTPDF_HOST_MEMORY_MB=48`：host 因記憶體上限結束 6 次，`deep-nesting-content-100000` 與 `form_dag_depth20` 各 3 次後停止重啟（各有 1 頁永久失敗）。兩次腳本都跑完，process 由腳本最後的 Quit 結束（第 2 次記錄到結束碼 0，第 1 次沒有記錄結束碼）；0 次 panic，0 個步驟逾時 | 通過 |

- **第一輪的決定**：300 頁情境的 B-8 時間比較未能確認，預設維持 in-process。第二輪以負載閘門重新量測後全部通過，見下一節。

### PR 4 第二輪（2026-10-05）：吞吐量與預設

完整的量測方法與數字見 [`docs/benchmarks/render-host.md`](../benchmarks/render-host.md)。

- **Pipelining**：IPC 的往返沒有消失，改成把它藏在 host 的 render 後面。
  - `EngineDocument::render_queue_depth`：remote 文件回報 2，其他 engine 維持預設的 1。`GuardedDocument` 轉送時限制在 1–4。
  - `RenderScheduler` 開 `workers × depth` 條 thread。session 的 worker 仍是 2，所以 remote 時有 4 個請求在途。
  - host 新增 render lane，執行緒數是 `RemoteConfig::render_threads`。app 設成 session 的 worker 數（2），所以 host 同時 render 的頁數、hayro 的 render context 與快取數量都和 in-process 相同，多出來的請求在 host 排隊。
  - 開檔、text layer、outline 等其他工作留在原本的 work lane，不排在 render 後面。
- **排隊中的 render 的 deadline**：
  - render 註冊時，前面每有一輪 `render_threads` 個 render 在途，deadline 就多一個 timeout。
  - 註冊與寫入 pipe 在同一個鎖內完成，所以 host 收到請求的順序與註冊順序相同。
  - 呼叫端自己的放棄時間改為以註冊的 deadline 計算。
  - 卡住的 render 仍在自己的 deadline 被終止。
- **吞吐量**（7 對交替，有負載閘門，負載 4–14%；修改前 → 修改後的中位數）：

| 檔案 | cold | cache 命中 |
|---|---|---|
| three-pages | −7.3% → −2.4% | −23.9% → +9.0% |
| dense-300p | −1.4% → −1.4% | −2.5% → −2.0% |
| photos | −2.0% → −0.8% | −20.1% → +18.0% |
| gov-letter | −6.5% → +5.3% | −20.7% → +12.8% |

- **剩下的成本**：
  - 每個 tile 仍有一次 IPC 往返（20–26 µs、110–130 kcycles，要喚醒 4 條 thread）與一次 1 MiB 複製，只是不再擋住 render。
  - 每個 tile 的總 CPU cycles 比 in-process 多：新內容多 8–31%，cache 命中多 13–59%（修改前是 4–13% 與 5–27%）。增加的部分來自同時執行的工作變多後的快取與記憶體頻寬競爭。
  - 複製省不掉：GPUI 的 `RenderImage` 只接受 process 自己的 `Vec`，tile cache 又會長期持有 tile，不能借用 slot。
  - 少 1–2 次喚醒（每個 slot 一個完成 event，或 host worker 自己讀命令）估計只省 3–5% 的 CPU，本輪沒有做。
- **修正**：只開 handle 時，空檔案讓 host 以 protocol 錯誤結束（結束碼 4）。原因是長度 0 的檔案被當成 section 傳給 host。現在改當成空的來源，回應與 in-process 相同（`Malformed`）。
- **正確性**：
  - 84 個 fixture 的第一、中間、最後一頁共 192 頁，兩邊逐位元組比較：372 個 render 相同；2 頁的 geometry 錯誤與 8 個 render 錯誤兩邊相同；190 個 text layer 相同。0 個差異，host 0 次 crash。
  - 取消、crash 歸責、deadline、slot 回收的整合測試全部通過。
- **B-8**：
  - 方法：bench-app 1.1.0、`-ThreadDetail`、dist build。A＝`--engine hayro`，B＝`--engine hayro-isolated`，3 頁與 300 頁各 6 對。
  - 負載閘門：每次啟動前取樣系統忙碌度，低於 30% 才啟動，實際啟動時是 4.1–15.3%。
  - 判定：配對差的中位數。

| 項目 | 條件 | 結果 | 判定 |
|---|---|---|---|
| 3 頁 `window_visible`／`first_page_exact` | 配對差中位數 ≤ 5 ms | −0.43 ms／−3.04 ms | 通過 |
| 300 頁 `window_visible`／`first_page_exact` | 同上 | +3.07 ms／−3.49 ms | 通過 |
| idle CPU（`-ThreadDetail`） | 不變 | 兩種模式的 idle CPU 都來自 GPUI 的主執行緒與 `VSyncProvider`；兩個 host 的所有 thread 都是 0 cycles、0 次喚醒。整個 tree 的配對差中位數：3 頁 −4.15 Mcycles／s、−21.7 次／s；300 頁 +1.27 Mcycles／s、+4.1 次／s（雜訊內） | 通過 |
| parent＋host 總 private | ≤ in-process＋20 MiB | private bytes +6.40／+6.45 MiB，private working set +3.70／+3.75 MiB（3 頁／300 頁） | 通過 |
| hostile 語料經由 UI | UI process 結束 0 次 | 預設 engine（isolated）加 `FASTPDF_HOST_MEMORY_MB=48`：46 個檔、783 步跑完，host 因記憶體上限結束 6 次，2 份文件停止重啟，結束碼 0，0 次 panic | 通過 |
| 吞吐量（第 1 項） | cold 與 cache 命中 ≥ in-process −5% | 最差的中位數：cold −2.4%，cache 命中 −2.0% | 通過 |

- **決定**：全部通過。
  - Windows 的預設 engine 改為 isolated：沒有 `--engine` 時用 `hayro-isolated`。
  - `--engine hayro`（或 `FASTPDF_ENGINE=hayro`）在 process 內 render。
  - `--help` 會印出預設 engine。
  - `fastpdf-bench` 維持在 process 內。
  - 其他平台沒有 host 實作，維持在 process 內。

### PR 4 第三輪（2026-10-05）：每個 tile 的 CPU

完整的方法與數字見 [`docs/benchmarks/render-host.md`](../benchmarks/render-host.md)〈第三輪〉。

- **問題**：第二輪之後，每個 tile 的 CPU cycles（parent＋host）比 in-process 多：新內容 8–31%，cache 命中 13–59%。預設 isolated 會讓筆電 render 時更耗電。目標是兩者都在 +10% 內，吞吐量與 B-8 維持。
- **成本來源**（每個 thread 的 cycles 與單獨的微量測）：
  - IPC：每個 tile 喚醒 4 條 thread，兩端各一次 pipe 寫入與讀取。
  - host 裡的 BGRA 轉換：engine 的 block cache 是 RGBA，`copy_from_rgba` 逐像素換位；來源不在 CPU 快取時每 MiB 570–680 kcycles，同樣的資料逐列複製只要 190–290。in-process 時轉換與新記憶體的 page fault 在同一趟，remote 時是多出來的一趟。
  - pipelining：同時忙碌的 thread 變多，每個 tile 多 2–22 個百分點（in-process 的 worker 從 1 條加到 4 條也多 14–18%）。
- **修改**：
  1. **slot channel**（`win/channel.rs`、`win/sync.rs`）：tile render 不經 pipe。
     - 每個 slot 一個 4 KiB 控制區塊（第二個共享 section）。parent 寫入編碼好的 `Render`、標成 REQUESTED（在 table 的鎖內，順序與註冊順序相同），再 release 一個 semaphore。
     - host 的 render thread 等 semaphore，以 CAS 取 order 最小的區塊，render 後把 `Done` 寫回區塊，set 該 slot 的完成 event。
     - 等待中的 worker 直接等自己 slot 的 event 與「host 已結束」的 event。每個 tile 只喚醒 2 條 thread，沒有 `ReadFile`／`WriteFile`。
     - 取消仍送 `Cancel`，另外在區塊設 cancel flag；deadline、crash 歸責照舊；被放棄的 slot 等 host 做完後由 reader 的 tick 回收；host 結束前已寫好回覆的請求照常交付，不算在途。
     - handle 以最小權限 duplicate：semaphore 只有 `SYNCHRONIZE`，event 只有 `EVENT_MODIFY_STATE`。parent 檢查區塊的狀態、id、長度後才以 protocol decoder 解碼，違規就終止 host。
     - 大於 slot 的 render 仍走命令，由同一組 render thread 處理，host 同時 render 的數量不變。
  2. **RGBA 傳輸**：parent 一律向 host 要 RGBA（engine 自己的順序），host 的 copy out 變成逐列複製；parent 從 slot 複製時順便換成 BGRA（u32 遮罩與位移，會向量化）。不改 protocol，結果逐位元組相同。
  3. **取消 pipelining**：`render_queue_depth` 回到預設的 1。scheduler 的 `workers × depth` 機制保留給 round trip 慢的 engine；host 的 render lane 與排隊 render 的 deadline 照舊（viewport 與縮圖的 scheduler 同時 render 時仍會排隊）。
- **結果**（7 輪交替，每次執行前負載 < 20%；中位數，修改前 → 修改後）：

| 檔案 | CPU 新內容 | CPU cache 命中 | 吞吐量 新內容 | 吞吐量 cache 命中 |
|---|---|---|---|---|
| three-pages | +28.4% → −2.0% | +53.7% → −2.2% | −0.4% → −0.1% | +12.3% → +0.2% |
| dense-300p | +11.7% → +2.5% | +9.7% → +1.0% | +0.0% → −2.3% | +0.3% → −1.3% |
| photos | +8.3% → +1.4% | +50.1% → +0.8% | −1.0% → −1.0% | +14.9% → +0.5% |
| gov-letter | +27.6% → −1.4% | +56.4% → +6.5% | −4.0% → +0.7% | +6.9% → −3.8% |

- **各項的貢獻**（另一組 4 個版本輪替，cache 命中）：只有 slot channel 時少 0–11 個百分點；加上 RGBA 傳輸再少 4–23；取消 pipelining 再少 6–22（新內容 2–19）。
- **取捨**：
  - cache 命中的吞吐量不再比 in-process 快，變成相同。保留 pipelining 時快 44–61%（dense-300p 沒有差），但 CPU 多 5–23%。新內容的吞吐量兩種都相同。
  - 背景負載 24–30% 時，cache 命中的 CPU 最多 +18.9%、吞吐量最低 −19.2%：每個 tile 有兩次 thread 交接，tile 很便宜時喚醒延遲就顯得明顯。
  - 不開 UI 的第一頁（開檔加第一頁所有 tile）：remote 比 in-process 多 1.16–1.23 ms，保留 pipelining 時 1.24–1.34 ms。
- **B-8 抽查**（dist build，3 頁 4 對，負載 4–18%）：`window_visible` 配對差中位數 +2.95 ms；`first_page_exact` +8.45 ms〔−9.86, +18.68〕，超過 5 ms。
  - 4 對中有 2 對是 B 的整個啟動就較晚（`window_visible` +11.4、+6.0 ms）；第一頁只會落在第一個 frame 或晚一個 frame（0 或 6–15 ms），A、B 都是。
  - 不開 UI 的量測顯示 render 的差距是 1.2 ms，與有無 pipelining 無關。4 對不足以判定，需要時以第二輪的方法（每個情境 6 對）重跑。
  - idle：兩個 host 的 FastPDF thread 都是 0 cycles、0 次喚醒；private bytes +6.00 MB、private working set +3.50 MB（第二輪 +6.40／+3.70）。
- **正確性**：84 個 fixture 0 差異；取消、crash 歸責、deadline、slot 回收、空檔、只交 handle 的整合測試全部通過；新增 slot channel、event／semaphore、R／B 換位與 BGRA＋夜間模式＋大於 slot 的比對測試。
- **還沒做**：in-process 的 `copy_from_rgba` 也是逐像素換位（engine-api 的 `PixmapMut`），改成同樣的遮罩寫法可以讓 in-process 的 cache 命中也變快。這不影響 remote（host 已不換位）。

### 最終驗收重測（2026-10-05，`658b47a`，GPUI 本地 patch 之後）

- **條件**：A＝in-process、B＝isolated，每個情境 6 對交替，啟動前負載 5–11%。細節見 `docs/benchmarks/b8-app.md`〈第十輪〉。
- **`first_page_exact`**：
  - 3 頁：中位數 164.7 對 165.8 ms，中位數相減 +1.2 ms；配對差中位數 **+8.9 ms**，超過 5 ms 的門檻；
  - 300 頁：中位數相減 −0.6 ms，配對差 −2.7 ms，通過。
- **`window_visible`**：配對差 +3.2 ms 與 +1.5 ms，通過。
- **其他項目**：記憶體（private working set +3.6 MB、private bytes +5.5–6.2 MB）、idle（兩者主執行緒都是 0 次喚醒）、吞吐量與每個 tile 的 CPU（`render-host.md` 第三輪），全部通過。
- **未達門檻的原因**：GPUI 本地 patch 讓視窗提早了約 30 ms，文件 host 的開檔與第一頁剛好落在第一個 frame 的邊緣。3 頁情境的 6 對中，有 4 對 isolated 晚了一個 frame（約 8–11 ms），2 對反而比較快。不開 UI 時，開檔加第一頁只多 1.2 ms。
- **決定：維持 isolated 為 Windows 的預設**，3 頁情境的配對門檻列為已知未達成：
  - 差距最多是偶爾晚一個 frame，兩種模式的首頁都在 165 ms 左右，遠低於 spec §29 的 200 ms；
  - isolation 換來的是 hostile PDF 無法讓 reader 結束，這是本 ADR 的主要目的；
  - 需要最低延遲的使用者可以用 `--engine hayro`。
- **後續**：在 GPUI 啟動期間更早啟動文件 host 並開檔（目前只有待命 host 是提早啟動的），讓第一頁穩定落在第一個 frame。

## Consequences

- UI process 不再因為 engine 的 stack overflow、配置失敗、mmap I/O 錯誤或失控運算而消失，§24、§25 的要求從「盡量 contain」變成由 OS 保證。R1 的「中期緩解」與 R10 都由這個 ADR 承接。
- 多一個 process 要管理：spawn、握手、重啟、版本一致性、診斷（host 的 log 要轉送到 parent 的 logger）。
- 每個 tile 多一次 1 MiB 複製（parent 從 slot 複製時順便把 RGBA 換成 BGRA）與一次 slot channel 往返（喚醒 2 條 thread）。每個 tile 的 CPU cycles 與 in-process 相差 −2.2% 至 +6.5%，吞吐量相同（新內容 −2.3% 至 +0.7%、cache 命中 −3.8% 至 +0.5%），見〈PR 4 第三輪〉。背景負載高時 cache 命中的 tile 受兩次 thread 交接的喚醒延遲影響較大。低階機器要在 B-8 補測（R12）。
- 記憶體總量基本不變，但 private bytes 分成兩個 process，再加上待命 host。B-8 第二輪的合計比 in-process 多：private bytes 6.4 MiB、private working set 3.7 MiB。memory budget manager 已把 host 的 private 納入 external bytes，overlay 也分開顯示。
- 列印、搜尋、選取透過 `GuardedDocument` 的介面自動走 host，不需要個別修改。

## Alternatives considered

- **只靠 guardrail**（現況）：已能擋下所有已知的 hostile 檔，零額外成本；但只擋已知形狀，且 Rust 無法攔截 stack overflow 與配置失敗。保留，作為 host 內的第一道防線。
- **大 stack 的 worker thread**：可以推遲 stack overflow，但無法處理配置失敗與失控運算，也只是把門檻移高。
- **fork／vendor engine 加深度與記憶體上限**：需要長期維護，而且永遠追不完。仍然向 upstream 回報（hayro #1347、`docs/upstream-issues/zpdf.md`）。
- **只在大檔或特定來源時走 OOP**：攻擊與檔案大小無關；依來源判斷可以靠 MOTW，但本機檔案同樣可能有害，而且 OOP 的成本已接近零，條件分支只會增加測試矩陣。依來源只調整沙箱強度。
- **duplex pipe 加 overlapped I/O**：一條 pipe 就夠，但需要 overlapped 狀態機；兩條單向 pipe 較簡單。
- **像素走 pipe**：每 MiB 600 µs，比共享記憶體慢 16 倍。否決。
- **每個 tile 用一個 host（或每次 render 一個 process）**：隔離最徹底，但每次 13 ms 以上的 spawn 與 reopen 成本無法接受。否決。

## Validation

- 原型與量測腳本放在 scratchpad 的 `oop/proto`（`startup`、`ipc`、`inproc`、`oop`、`hostile`、`fault`、`orphan` 子命令），不進 repo。PR 2–4 的整合測試會把其中的情境搬進 CI。
- PR 3 的 diff-corpus 必須 0 差異；PR 4 的 B-8 必須和 in-process 比較並記錄負載狀況。
