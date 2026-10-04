# 授權全文 override（`licenses/overrides/`）

有些 crate 發佈到 crates.io 時沒有附授權檔，但 MIT、BSD 等授權要求 binary 隨附含 copyright 的授權全文。這個資料夾存放在本機找到、來源明確的全文。`tools/license_report.py --bundle` 會把它們放進 release zip 的 `licenses/third-party/<crate>-<version>/`，並在 bundle 的 `MISSING.md` 列在「Filled in from licenses/overrides」一節。

## 規則

- 只收錄**同一個 crate 的其他版本**或**同一個 repository 的其他套件**所附的授權全文，逐位元組複製。
- **不可自行撰寫或拼湊 copyright 行。** 找不到全文的 crate 維持缺漏，列在 bundle 的 `MISSING.md`。
- 資料夾名稱是 `<crate>-<version>`，版本必須和 `Cargo.lock` 完全相同。依賴升級後，舊資料夾不會被沿用：該 crate 會列為缺漏，bundle 也會顯示警告。
- 每個檔案都要在 [SOURCES.md](SOURCES.md) 登記來源與 SHA-256。只要有一個檔案沒有登記或 SHA-256 不符，整個資料夾都不會被使用。
- 只有在 crate 自己沒有任何授權全文時才會使用 override。
- `.gitattributes` 讓這些檔案保持原本的位元組（不轉換換行），SHA-256 才會一致。

## 目前狀態（2026-10-04）

| Crate | 授權 | 狀態 |
|---|---|---|
| `alloc-stdlib` 0.2.4 | BSD-3-Clause | 已補上：同一個 repository 的 `alloc-no-stdlib` 2.0.4 的 `LICENSE` |
| `pulp-wasm-simd-flag` 0.1.1 | MIT | 已補上：同一個 repository、同一個 commit 的 `pulp` 0.22.3 的 `LICENSE` |
| `seahash` 4.1.0 | MIT | **缺漏**：本機只有這個版本，套件沒有附授權檔，也找不到同一個 repository 的副本 |
| `taffy` 0.13.0 | MIT | **缺漏**：本機的 0.9.0、0.10.1、0.13.0 都沒有附授權檔，也找不到同一個 repository 的副本 |

已搜尋的位置：cargo registry 的 `src` 與 `cache`（含 `.crate`）、cargo git checkouts、`upstream/` 的 clone、其他 crate 內附的授權檔。

## owner 如何補上缺漏

1. 從 crate 的 upstream repository 取得授權全文。請用和發佈版本相符的 revision：套件內 `.cargo_vcs_info.json` 的 `git.sha1` 就是發佈時的 commit。
   - `seahash` 4.1.0：https://gitlab.redox-os.org/redox-os/seahash ，commit `94b632aeac099031c373599313d5b5f0acbbaec0`，套件在 repository 根目錄。
   - `taffy` 0.13.0：https://github.com/DioxusLabs/taffy ，commit `45a56299d366ddb383e593a1f0372158d00e8530`，套件在 repository 根目錄。
2. 原樣存成 `licenses/overrides/<crate>-<version>/<原檔名>`，不要修改內容或換行。
3. 計算 SHA-256，例如 `Get-FileHash -Algorithm SHA256 <檔案>`。
4. 在 SOURCES.md 的表格加一列：檔案路徑、SHA-256、來源（repository URL、commit 或 tag、檔案在 repository 中的路徑）。
5. 執行 `python tools/license_report.py --bundle <空資料夾>`，確認該 crate 出現在 `MISSING.md` 的「Filled in from licenses/overrides」，而且不再列為缺漏。
6. 如果 upstream 在那個 revision 也沒有授權檔，就維持缺漏，由 owner 決定是否向 upstream 詢問或更換依賴。

依賴升級時，bundle 會把舊版本的 override 列為 stale。先確認新版本的 upstream 授權檔沒有變動，再以新版本建立資料夾、更新 SOURCES.md，最後刪除舊資料夾。
