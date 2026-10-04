# ADR 0008 — Out-of-Process Rendering（render host process）

- 狀態：Proposed
- 日期：2026-10-04
- 相關 spec：§12、§18、§24、§25、§29、§33；風險：`docs/PROJECT_AUDIT.md` R1、R10；`docs/audit/hayro.md` R1–R3
- 原型：scratchpad 的 `oop/proto`（不在 repo 內），以 path dependency 指向 HEAD `956c573` 的 `git archive` 快照，避免受其他 agent 未 commit 的修改影響

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

## Decision（提議）

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

環境：AMD Ryzen 9 9950X（16C/32T）、125 GB RAM、Windows 11 10.0.26200、release（thin LTO）、Hayro 與 zpdf adapter 取自 HEAD `956c573`。量測期間 CPU 負載 6–11%，開始與結束時都沒有其他 cargo／rustc 在執行（其他 agent 可能在中途短暫編譯）。Hayro 各組態跑 3 次取中位數，zpdf 兩組各跑 2 次取平均；IPC 每次 1000 回合，共 3 次；啟動時間每組 15 次。原型 render 一律 scale 1.5、512² tile；in-process 與 OOP 平行度相同（4 條 thread 對 4 個在途 request＋host 4 個 worker，另有一組 8 對 8）。

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

## Consequences

- UI process 不再因為 engine 的 stack overflow、配置失敗、mmap I/O 錯誤或失控運算而消失，§24、§25 的要求從「盡量 contain」變成由 OS 保證。R1 的「中期緩解」與 R10 都由這個 ADR 承接。
- 多一個 process 要管理：spawn、握手、重啟、版本一致性、診斷（host 的 log 要轉送到 parent 的 logger）。
- 每個 tile 多一次 1 MiB 複製（13–40 µs）與一次往返（約 10 µs）。實測對吞吐量沒有影響，但低階機器要在 B-8 補測（R12）。
- 記憶體總量基本不變，但 private bytes 分成兩個 process。memory budget manager 要把 host 的 private 納入 external bytes，overlay 也要分開顯示。
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
