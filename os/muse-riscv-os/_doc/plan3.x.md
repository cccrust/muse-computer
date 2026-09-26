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
v3.3  升級鏈          registry 多版本路由 + index + 版本範圍 +
                      upgrade + autoremove（見 §5）
v3.4  信任鏈          sha256 完整性 + registry 認證（見 §6）
v3.5  volume          持久數據進出容器的正規路（見 §7）
v3.6  容器小件收尾    ps 限額 + rm -f + restart（見 §8，spec 已定）
v3.7  registry 常駐化 publish 上架 + 磁碟持久（見 §9）
v3.8  guest 函式庫     guest-args + guest-fmt 首發備料（見 §10，均已上架）
v3.9  crates.io 閉環   外部 crate 下載→編譯→安裝→運行（見 §11）
v3.10 upgrade pin 重驗 依賴者約束不滿足即 held by 拒絕（見 §12）
v3.11 私包+刪包+logout  `--private` 上架、DELETE 管理面（見 §13）
v3.12 併發修復          /tmp pid-unique，併發安裝不互踩（見 §14）
v3.13+ 候選池         按需排序，不預排版號（見 §15）
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
v3.3+ 增量見 §5–§14（多版本路由、sha256、`/vol/`、sidecar、publish、guest 庫、閉環、pin 重驗、私包、併發修復）。

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
- 非目標重申（除非另立大版）：版本並存（v3.3 解決）、`upgrade`
  （v3.3）、簽名驗證（v3.4）、delta 更新、私有 registry 認證
  （v3.4）、guest 側編譯器（永不）。

## 5. v3.3：升級鏈（registry 多版本 + 版本範圍 + upgrade）

動機：v3.0–v3.2 只會裝不會升；`already installed` 一擋了事。
升級的前提是多版本並存——registry 端用「啟動現包」直接加
`/pkg/<name>/<ver>/` 路由（v3.0 同招；曾考慮 `tools/packages/`
磁碟目錄，已斃：blob 進版控 + 重打包流程，收益為負），
另加 `/pkg/<name>/index` 版本列表。

- registry：`/pkg/<name>/<ver>/manifest` + layer 路由改讀磁碟；
  另加 `/pkg/<name>/index`（版本列表，一行一版，guest 選版用）。
  舊無版號路由保留（指最新版，test 舊測項不用改）。
- manifest：`version:` 語意收緊為三段式（`x.y.z`，數字比較；
  不合規 → loud 拒收）。依賴 token 支援 `name>=v`（`=` 沿用；
  `>`/`<`/`~` 不做——夠用就好）。
- `ctr upgrade [<pkg>]`: 無參數 = 全升，有參數 = 單升。對每個包：
  查 index 取最大滿足約束的版本 → 與 db 比 → 新則「裝新版 +
  刪舊版」原子視角（先裝後刪：中間態雙 store 並存，失敗留舊版，
  不變量：db 永遠指一個完整版）。依賴重解（新版的 depends 為準）。
- `ctr autoremove`：刪「不在任何 depends 閉包裡」的包
  （db + 各 store manifest 反查，v3.1 needed-by 的反方向）。
  先印清單再刪（`autoremove <name>...` 行，確定斷言用）。
- suite 方向：registry 放 `hello 1.0 + 2.0`（2.0 改一行 txt）；
  `install hello`（得 1.0？不——新裝直接取最新，這點要在 doc 釘死：
  install 無約束 = 最新版）→ `upgrade` → 版本行變 2.0 →
  `autoremove`（視 suite 安排）。約 6–8 個 marker。
- 非目標：downgrade（`install =ver` 指定舊版？v3.4 再議）、
  並行下載、delta。

## 6. v3.4：信任鏈（完整性 + registry 認證）

動機：v3.3 之後包會越裝越多，來源必須可驗。兩層，各自獨立：

- 完整性：manifest `sha256:` 行（layer bytes 串接 hash），guest
  先驗後解。擋傳輸損壞/調包；**不是非對稱簽名**（ed25519 要先有
  SHA-512，~500 行 crypto——進 §8 候選池，不在本版展開）。
  guest sha256 住 `user-lib`（core-only，host-tests NIST 向量先行）。
- registry 認證：私仓 token（`Authorization: Bearer`；guest wget
  加 header 位，token 落 `/pkg/token`，`ctr login` 寫入）。
- 順序：完整性先（不依賴 registry 改動，本地驗），認證後。
- suite 方向：篡改 tar → mismatch marker；無 token 抓私包 → 401
  marker。registry 樁加 401 分支。
- 非目標：ed25519、吊銷/過期、TLS（guest 側 handshake 太重；
  user-net 本來就只信本地）、key 輪換。

## 7. v3.5：volume（容器數據正規路）

動機：plan2.x §8 原話——「持久數據進出容器的正規路；目前靠
hardlink 與整盤持久」。包（v3.x）和容器（v2.x）都齊了，數據卷
是最後一塊拼圖。

- `ctr volume create <v>`：`/vol/<v>/`（host 視角目錄，與 `/ctr`
  平級，不隨容器生死）。
- `ctr run [-d] -v <v>:<cpath> ...`：子 chroot **之前**把 vol
  bind 進 jail（實現選型二選一，v3.5 定案時選：a) VFS mount 表
  （真 bind mount，動 FS 核心）；b) 目標路徑預建 hardlink 樹
  （assemble 招式，無 FS 改動，但語義是快照不是共享）。
  傾向 a)，但 b) 是可接受的過渡——寫 doc 時誠實註明。
- 生命週期：volume 與容器正交（`rm` 容器不刪卷，docker 同款）；
  `ctr volume ls/rm`（rm 拒非空？還是遞迴——定案時選）。
- suite 方向：卷內寫檔 → 容器內可見 → 容器刪後卷還在。
- 非目標：quota on volume（cgroup 配額是容器側的）、跨宿主遷移、
  volume driver。

## 8. v3.6：容器小件收尾（ps 限額 + rm -f + restart）

動機：v2.x 留的三個小件，一次收完（唯讀/組裝既有原語）。
落點見 `v3.6.md`（SYS_CGSTAT + `.run` sidecar + stop 共用 helper）。
狀態：spec 已定，未實作。

## 9. v3.7：registry 常駐化（publish 上架）

動機：測試樁只能 serve 寫死的包；日常可用的第一步是活著能收新包、
重啟還在。落點見 `v3.7.md`（磁碟佈局 + PUT 上架 + index 生成；
guest 零改動）。

## 10. v3.8：guest 函式庫（guest-args + guest-fmt）

動機：管線能編 binary，但 crates.io 上零個 guest 包——先放兩個
真正有用的 library（argv 解析 + 無分配格式化，in-tree 手刻 N 遍的
收攏）。落點見 `v3.8.md`（獨立 crate + host 向量 + mirror 紀律；
`cargo publish` 由主人執行，兩包皆已上架）。

## 11. v3.9：crates.io 端到端閉環

動機：v3.2 的 deferred 項——第一個 guest crate 上架後，證明外部
crate 從 crates.io 下來、編進 guest 二進制、裝進系統跑起來。
落點見 `v3.9.md`（`tools/pkgrepeat` 全依賴走 crates.io
——含後來上架的 `user-lib` 0.1.1；update：已全切）。

## 12. v3.10：upgrade 依賴 pin 重驗

動機：v3.3 的已知缺口——upgrade 不檢查依賴者的版本 pin，
依賴關係靜默失配。落點見 `v3.10.md`（升級前掃 stored manifest，
約束不滿足即 `held by` 拒絕）。

## 13. v3.11：私包 + 刪包 API + logout

動機：v3.4 的認證只有一半——registry 認得 token，但包沒有公私之分、
上架的東西刪不掉。落點見 `v3.11.md`（`--private` 上架落 `.private`
標記 + 讀取門、`DELETE` 管理面、`ctr logout`；v3.13 做 ed25519 真簽名）。

## 14. v3.12：併發安裝 /tmp 競態修復

動機：`install` / `upgrade` / `pull` 共用固定 scratch 檔名，
併發跑必互踩。落點見 `v3.12.md`（`tmp_path()` pid 後綴；
單線行為不變；掉電原子性另立項）。

## 15. v3.13+ 候選池（按需排序，不預排版號）

- 包小件：`install =ver` 指定舊版（downgrade 語意）、並行下載、
  delta 更新、`KB/MB` 別名。
- 運行時大件（plan2.x §8 遺產）：overlayfs、netns/veth（最大版，
  放最後）、IO 權重、層級配額 enforced、cgroupfs、OOM-killer、
  `pivot_root`。
- 包信任大件：ed25519 非對稱簽名（含 guest SHA-512）、key 輪換/吊銷。
- 永不（除非另立大版）：guest 側編譯器、users/capabilities 驅動的
  權限檢查。
