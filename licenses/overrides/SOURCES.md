# `licenses/overrides/` 的來源

這裡的每個檔案都是在本機找到的授權全文，逐位元組複製，沒有任何修改。來源只有兩種：**同一個 crate 的其他版本**，或**同一個 repository 的其他套件**。沒有任何內容是自行撰寫或拼湊的。

`tools/license_report.py --bundle` 只會使用列在下表、而且 SHA-256 相同的檔案。表格格式固定為「檔案、SHA-256、來源」三欄：前兩欄用反引號包住，來源欄不可以使用直線符號。

| 檔案 | SHA-256 | 來源 |
|---|---|---|
| `alloc-stdlib-0.2.4/LICENSE` | `c0c56f26d9c051cac4d200c34c84e7ae9aaa853e01a982a1df08b09931e518ae` | crates.io 套件 `alloc-no-stdlib` 2.0.4 內的 `LICENSE`（BSD-3-Clause，Copyright (c) 2016 Dropbox, Inc.），取自 cargo registry 的 `alloc-no-stdlib-2.0.4.crate`。同一個 repository：兩個套件的 `repository` 都是 https://github.com/dropbox/rust-alloc-no-stdlib。依各自的 `.cargo_vcs_info.json`，`alloc-no-stdlib` 2.0.4 從 repository 根目錄發佈（commit `6032b6a9b20e`），`alloc-stdlib` 從子目錄 `alloc-stdlib/` 發佈（0.2.4 為 commit `ae42d22078b9`；0.2.2 與 `alloc-no-stdlib` 2.0.4 是同一個 commit）。兩者的授權欄位都是 `BSD-3-Clause`，作者相同。 |
| `pulp-wasm-simd-flag-0.1.1/LICENSE` | `d64f878c89bd5f1e5ada7e4aad57690b122727cf5229b7b87ea1980c188da1d9` | crates.io 套件 `pulp` 0.22.3 內的 `LICENSE`（MIT，Copyright (c) 2021 sarah），取自 cargo registry 的 `pulp-0.22.3.crate`。同一個 repository、同一個 commit：兩個套件的 `repository` 都是 https://github.com/sarah-quinones/pulp/，依各自的 `.cargo_vcs_info.json` 都從 commit `5eb07fd7b68e` 發佈，`pulp` 在子目錄 `pulp/`，`pulp-wasm-simd-flag` 在子目錄 `pulp-wasm-simd-flag/`。兩者的授權欄位都是 `MIT`，作者相同。 |

尚未找到：`seahash` 4.1.0、`taffy` 0.13.0。說明與補上的方法見 [README.md](README.md)。
