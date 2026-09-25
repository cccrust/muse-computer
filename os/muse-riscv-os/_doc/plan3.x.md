# muse-riscv-os v3.x 規劃：套件管理（apt 式，預編包）

> 狀態：v2.x 已結案（容器宿主：隔離 + 生命週期 + 配額接線 + 可觀測，
> `test.sh` 全綠，見 `_doc/plan2.x.md`）。v3.x 主線是把 guest 變成
> 「裝得上軟體的系統」：apt 式套件管理——guest 只吃預編包，
> 永遠不直連 crates.io（判據見 §1）。
> 本計畫第 0–4 節為總綱，各版落點見 `v3.0.md` 起的版本檔。

## 0. 環境與前提

- QEMU `virt` + OpenSBI，S-mode kernel，`-smp 4`，Rust nightly，
  零外部依賴維持（guest 側；host 側 python 樁 + cargo 工具鏈如常）。
- 關鍵判據（plan 階段已定，不重複討論）：guest 跑自創 ecall ABI，
  非 Linux——crates.io 的普通 crate 跑不起來，能跑的只有針對
  `user-lib` 寫的 guest-target crate。因此 guest 永遠只跟自家
  registry 講話（v2.3 `ctr pull` 的延伸）；crates.io 是 host 端
  打包管線的源，guest 碰不到它。
- FS 無 symlink/chmod（全員 root 語意）：包內 `bin/*` 用 hardlink
  進 `/bin`（`cmd_assemble` 招式）；權限位忽略（untar 已如此）。

## 1. 主線地圖（版本 ↔ 能力）

```
v3.0  安裝三件套      ctr install/remove/list + registry 包路由 +
                      manifest（name/version）+ 第一批包（內建程式重包）
v3.1  依賴            manifest depends: + guest 拓撲安裝 + 版本 `=`
v3.2  crates.io 管線  user-lib 發布 + tools/pkgbuild（fetch→交叉編譯→打包）
```

- v3.0 是「能用」：單包安裝閉環，不碰依賴。
- v3.1 是「好用」：依賴解析，guest 側十幾行確定性代碼。
- v3.2 是「開源生態」：host 端才碰 crates.io，guest 零改動——
  這就是 apt 模型的紅利。
- 順序是鐵律：格式（v3.0）→ 依賴（v3.1）→ 外部源（v3.2）。
  依賴解析依賴穩定的包格式，外部源依賴可用的安裝器。

## 2. 工程骨架（各版增量）

```
user/ctr                # install/remove/list（v3.0）→ depends 解析（v3.1）
tools/img_registry.py   # + /pkg/... 包路由（v3.0，格式先行）
tools/pkgbuild          # host 打包管線（v3.0 打內建包；v3.2 接 crates.io）
/pkg/<name>/            # guest store（解包目的地；/pkg/db 追蹤檔）
user/sh                 # autorun 加 install→run→remove（v3.0）
run.sh/test.sh          # test.sh 項數見各版；registry 樁啟停沿用 v2.3
user-lib                # 發布到 crates.io（v3.2 前提；v3.0 不動）
```

## 3. 包格式（v3.0 定，後版只加行）

- 沿用 ustar layer（guest untar 不動，只認 regular/dir）。
- manifest 純文本（`/pkg/<name>/manifest`），`#` 註解：
  ```
  name: <pkg>
  version: <ver>
  layer1.tar
  ...
  depends: a b        # v3.1 才解讀，v3.0 忽略未知行（前向兼容）
  ```
- 單版本語意：重裝覆寫，不並存（多版本是 v3.x 範圍外）。

## 4. run/test 與通用紀律（v3.x 通用，後版沿用）

- 驗收門檻：`./test.sh` 全綠（項數見各版）+ 零 `PANIC`/`FAIL` +
  **連跑 2 輪**（v2.x 紀律沿用）。
- test 樁風格：host python 樁、guest 主動、確定斷言（v2.x 紀律沿用）。
- suite 只增不減：舊測項零退化是每版驗收第一條。
- 非目標重申（除非另立大版）：版本並存、`upgrade`、簽名驗證、
  delta 更新、私有 registry 認證、guest 側編譯器。
