# `licenses/overrides/` 的來源

這裡的每個檔案都是逐位元組複製的授權全文，沒有任何修改。來源只有三種：

- 同一個 crate 的其他版本；
- 同一個 repository 的其他套件；
- 該 crate 的 upstream repository 中，固定在某個 commit 的授權檔，網址與 commit 記錄在表中。

沒有任何內容是自行撰寫或拼湊的。

`tools/license_report.py --bundle` 只會使用列在下表、而且 SHA-256 相同的檔案。表格格式固定為「檔案、SHA-256、來源」三欄：前兩欄用反引號包住，來源欄不可以使用直線符號。

| 檔案 | SHA-256 | 來源 |
|---|---|---|
| `alloc-stdlib-0.2.4/LICENSE` | `c0c56f26d9c051cac4d200c34c84e7ae9aaa853e01a982a1df08b09931e518ae` | crates.io 套件 `alloc-no-stdlib` 2.0.4 內的 `LICENSE`（BSD-3-Clause，Copyright (c) 2016 Dropbox, Inc.），取自 cargo registry 的 `alloc-no-stdlib-2.0.4.crate`。同一個 repository：兩個套件的 `repository` 都是 https://github.com/dropbox/rust-alloc-no-stdlib。依各自的 `.cargo_vcs_info.json`，`alloc-no-stdlib` 2.0.4 從 repository 根目錄發佈（commit `6032b6a9b20e`），`alloc-stdlib` 從子目錄 `alloc-stdlib/` 發佈（0.2.4 為 commit `ae42d22078b9`；0.2.2 與 `alloc-no-stdlib` 2.0.4 是同一個 commit）。兩者的授權欄位都是 `BSD-3-Clause`，作者相同。 |
| `pulp-wasm-simd-flag-0.1.1/LICENSE` | `d64f878c89bd5f1e5ada7e4aad57690b122727cf5229b7b87ea1980c188da1d9` | crates.io 套件 `pulp` 0.22.3 內的 `LICENSE`（MIT，Copyright (c) 2021 sarah），取自 cargo registry 的 `pulp-0.22.3.crate`。同一個 repository、同一個 commit：兩個套件的 `repository` 都是 https://github.com/sarah-quinones/pulp/，依各自的 `.cargo_vcs_info.json` 都從 commit `5eb07fd7b68e` 發佈，`pulp` 在子目錄 `pulp/`，`pulp-wasm-simd-flag` 在子目錄 `pulp-wasm-simd-flag/`。兩者的授權欄位都是 `MIT`，作者相同。 |
| `seahash-4.1.0/LICENSE` | `23f18e03dc49df91622fe2a76176497404e46ced8a715d9d2b67a7446571cca3` | upstream repository https://gitlab.redox-os.org/redox-os/seahash 的 `LICENSE`（MIT），commit `3088c5c912b70b586d27bf553fbe964e025a2c89`（2023-10-24，"fix: add missing MIT license text"），2026-10-05 下載。4.1.0 的發佈 commit `94b632aeac099031c373599313d5b5f0acbbaec0` 還沒有這個檔案，它是發佈後由 upstream 補上的同一份授權。upstream 的檔案只有 MIT 許可文字、**沒有 copyright 行**，這裡原樣收錄、不自行補寫；作者依 `Cargo.toml` 為 ticki 與 Tom Almeida。 |
| `simd_helpers-0.1.0/LICENSE` | `d69f24ad84ec2ade64c0b68bdb31b41170e997b158370342056918329cc9af1e` | upstream repository https://github.com/lu-zero/simd_helpers 的 `LICENSE`（MIT，Copyright (c) 2019 Luca Barbato），commit `82040194cd05affb060bf94d6f19f82a771d07fb`（2019-12-06，"Create LICENSE file"），2026-10-05 下載。0.1.0 的發佈 commit `ca1a2f84aa386d758e98f8a609d990263932fb85` 還沒有這個檔案，它是發佈後由作者補上的。這個 crate 只經由 `rav1e` 的 weak 依賴進入 bundle 範圍，沒有連結進 exe；補上後 bundle 就沒有任何缺漏。 |
| `taffy-0.13.0/LICENSE` | `f97daf1a0124413dccf399a4e6626b4b74acd05282f80b6d64ac82225650b77a` | upstream repository https://github.com/DioxusLabs/taffy 的 `LICENSE`（MIT，Copyright (c) 2018 Visly Inc.、Copyright (c) 2026 Taffy Authors），commit `45a56299d366ddb383e593a1f0372158d00e8530`，也就是 0.13.0 的發佈 commit（套件 `.cargo_vcs_info.json` 的 `git.sha1`），2026-10-05 下載。 |

目前沒有缺漏。說明見 [README.md](README.md)。
