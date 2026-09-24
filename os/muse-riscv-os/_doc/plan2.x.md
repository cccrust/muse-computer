# muse-riscv-os v2.x 規劃：容器（隔離層層疊）

> 狀態：v1.x 已結案（SMP + 排程 + 回收 + net/TCP 前半，`test.sh` 60 項全過，
> 見 `_doc/plan1.x.md`）。v2.x 主線是把單機 Unix 升級為「容器宿主」：
> 隔離一層一層疊，每層可獨立驗收、上層不重寫下層。
> 本計畫第 0–7 節為總綱，各版落點見 `v2.0.md` 起的版本檔；
> v2.0–v2.3 已結案，v2.4+ 候選見 §8。

## 0. 環境與前提

- QEMU `virt` + OpenSBI，S-mode kernel，`-smp 4`，Rust nightly，零外部依賴維持。
- Docker 四層按序做：**fs 視圖 → pid 視圖 → 資源配額 → 網路/掛載豪裝**。
  順序是鐵律：配額 accounting 依賴穩定的進程身份，镜像分發依賴可用的
  chroot+pid 語意，網路隔離最後（最大版）。
- 無用戶體系：全員 root 語意，權限檢查從簡（寫死在各版文件，不重複討論）。

## 1. 主線地圖（版本 ↔ 隔離層）

```
v2.0  chroot 文件系統隔離   root 視圖 + 越獄不可能 + ctr 組裝 + chroot_test
v2.1  pid namespace         pid 視圖 + unshare + nstest
v2.2  cgroup-lite（內存）    frame 記賬 + cap 上限 + cgtest（內存半）
v2.3  docker 體驗三件套      ctr run 一鍵 + pull 镜像 + CPU 上限 + cgtest（CPU 半）
v2.4+ 生命週期/overlay/網路  ctr stop·rm·ps、overlayfs、registry 認證、
                            CPU floor/share（CFS）、netns/veth、volume（見 §8）
```

- v2.0–v2.2 是「隔離原語」，互相正交（root 視圖、pid 視圖、內存 cap；
  壞一層不拖累另兩層設計）。
- v2.3 是「能用」：原語齊了之後補一鍵 run、镜像概念、CPU 配額。
  三件互相獨立，共用 test 樁風格（host python 樁、guest 主動、確定斷言）。

## 2. 工程骨架（v2.0 改動面 + 各版增量）

```
kernel/src/task/mod.rs  # Proc += fsroot(2.0) / ns+lpid+pending_ns(2.1) /
                        #        cg(2.2) / CG_CPU·CAP·WIN atomics(2.3)
kernel/src/syscall.rs   # SYS_CHROOT=45(2.0) SYS_UNSHARE=46(2.1)
                        # SYS_CGCREATE=47 SYS_CGENTER=48 SYS_CGLIMIT=49(2.2)
                        # SYS_CGSETCPU=50(2.3)
kernel/src/trap.rs      # timer ISR 加 cg_tick(2.3，atomics+短 sched_lock)
kernel/src/fs/disk.rs   # create_empty/mkdir 全重置(2.3，防 stale 塊別名)
kernel/src/mem/*        # alloc 熱路徑記賬+超限回 None(2.2，用戶路徑 panic-on-OOM 退役)
user/ctr                # <name>組裝(2.0) → run(2.3) → pull+untar(2.3)
user/{chroot_test,nstest,cgtest}  # 每層一個自測，autorun 順序執行
user/sh                 # builtin chroot/unshare(2.1)；autorun 加 pull+run(2.3)
user/wget               # 串流化 body(2.3，>4KB 層包)
user-lib                # chroot/unshare/cgcreate/cgenter/cglimit/cgsetcpu/time
tools/img_registry.py   # :8091 registry 樁(2.3，stdlib tarfile 現包 guest ELF)
run.sh/test.sh          # test.sh 61→62→63→65 項；run1 背景常駐+樁全開
```

## 3. v2.0：chroot 文件系統隔離（已結案，61 項）

- per-proc root（`Proc.fsroot[128]`）+ **雙錨點解析**（絕對錨 root、
  相對錨 cwd，`..` 到 root 邊界即停）：containment by construction，
  全 syscall 經 `resolve_for` 自動被囚。
- `SYS_CHROOT=45` 只收緊不放寬（解出來的必是子孫，免前綴檢查）。
  誠實限制：已開 fd 仍指外界（Unix 同款）；pid/配額/網路一律不隔離。
- `ctr <name>`：`/ctr/<name>/bin` + hardlink 全 `/bin`（零拷貝，冪等）。
- `chroot_test`：getcwd/`/bin/sh` 可見/`/TESTDATA` 不可見/`..` 越獄失敗，
  全返回值斷言。`test.sh` 60+1=61 項。

## 4. v2.1：pid namespace（已結案，62 項）

- NS 表進 Sched（不另開鎖）：`Ns{parent,next}`，`Proc += {ns,lpid,pending_ns}`；
  root ns 恆等（`lpid==pid`，舊行為逐位元保留）。
- `fork` 分配：父 `pending_ns` 置位 → 子新 ns 當 pid 1（**one-shot**，
  與 Linux 常駐不同，可預測可測）；`fork`/`waitpid` 回傳一律 global pid，
  lpid 只從 `getpid`/`ps` 漏出去（現有斷言零改動）。
- 視圖：`ps` 只列同 ns（印 lpid）；`kill` 先配 `(ns,lpid)` 再退回 global
  （跨 ns 誤殺擋掉）；`exit` reparent 與 ns 正交。
- `SYS_UNSHARE=46`（只認新 pid ns，影響下一個 fork 的孩子）+ sh builtin
  `chroot`/`unshare`（交互可用性一次到位）。
- `nstest`：四子各新 ns 斷言 `getpid()==1`、一孫斷言 `==2`；
  `test.sh` 61+1=62 項。

## 5. v2.2：cgroup-lite 內存記賬 + cap（已結案，63 項）

- 記賬放 frame 側（`owner[32768]u16` + `CG_USE/LIM[256]`，既有 `ALLOC` 鎖下，
  熱路徑只拿一把鎖，無 sched↔frame 逆序）；歸屬= current 的 cgroup，
  boot/idle 記 cgroup 0（root，不限）；`dealloc_frame` 單一 choke 點扣賬。
- 元數據放 task 側（Sched 鎖下）：`Cg{parent}`（parent 只記不遍歷，
  扁平執行+層級記賬）、`Proc.cg`（fork 繼承）；`CGCREATE(limit)` 一步到位
  建組+設限，`CGENTER` 只許自己搬家；`halt` 行加 `[CG] groups=N`（v2.3 補）。
- 執行：`alloc_frame` 超限回 `None`（不 panic）；用戶可達路徑逐個審計
  （sbrk/mmap→`-1`，fork→`usize::MAX`，exec 順延 false 通道）；
  boot/內核表路徑保留 `expect`（root 不限，觸發即真 OOM）。
- `cgtest`（內存半）：子進小組 `sbrk` 到 `-1`（必須是 `-1` 不是全機 panic），
  父自家 `sbrk(1MB)` 照通 + `memstat` 差值小閾值（回收延續）；
  `test.sh` 62+1=63 項。

## 6. v2.3：docker 體驗三件套（已結案，65 項）

- `ctr run <name> <prog> [args]`：root 不存在報錯（pull 先行，無隱式魔法）；
  `unshare→fork`（子當新 ns pid 1）→ 子 `chroot+chdir+exec`；
  父 `waitpid` 原樣轉 exit code；prog 解析同 shell（有 `/` 照用、
  裸名補 `/bin/`）；stdio 繼承，不設 fg（後台語意 v2.4 議題）。
- 镜像（標準優先，自造為恥）：manifest 純文本（一行一層，`#` 註解），
  層=ustar（只認 regular/dir）；`ctr pull` 經 `wget` 二進制下載 manifest→
  逐層存 `/tmp`→順序解包進 `/ctr/<image>`（後層覆蓋）→刪 tar→`img PASS`
  （冪等）；解包自寫（512 對齊、octal、typeflag、corrupt-size 守衛）。
  `tools/img_registry.py`（`:8091`，`tarfile` 現包 `hello.txt`+guest `echo` ELF——
  跑起來的二進制才是證明）。
- CPU 配額（cap-as-ceiling）：timer tick 給 current 的 cgroup `CG_CPU++`
  （atomics，ISR 不拿新鎖）；100-tick 窗，`cg_pct` 默認 100；
  pick 三處同規則（`pick_locked`/`find_next`，`run_on` 經前者覆蓋）
  跳過超額任務，**除非無他可跑**（work-conserving，不閒置）；
  超額=`win_use*100 > pct*elapsed`（整數）。
  誠實聲明：滿載下被 cap 組趨近 0 而非 5%（floor/share 是 v2.4+）。
- `cgtest`（CPU 半，併入不新增 suite 項）：12 capped hog + 4 free hog 同跑，
  第一個收到的必須是 uncapped（15s 上限；hogs 數≫hart 數保證每核永遠有
  替代可跑，否則 pass-2 fallback 讓順序變擲幣——實測 2+6 會 flake）。
- 附帶修（壞一件不拖累另兩件之外，順手不繞路）：
  `wget` 串流化（>4KB 層包）、inode 全重置（防 unlink 大 tar 後 stale 塊別名）、
  `open` 存在性繞過 `/bin` embed 回退（`/ctr/.../bin/echo` 建檔用）、
  `[CG] groups=N` 補上（v2.2 承諾）。
- `test.sh` 63+2=65 項（+`img PASS` +`hello-from-image`），零 PANIC/FAIL，
  連跑 2 輪；手動 `pull && run`、`cat` 下載物、`ps` 看 pid 1。

## 7. run/test 與通用紀律（v2.x 通用，後版沿用）

- 驗收門檻：`./test.sh` 全綠（項數見各版）+ 零 `PANIC`/`FAIL` + **連跑 2 輪**
  （單輪綠是運氣，MTTCG 抖動只認第二輪）。
- test 樁風格：host python 樁（udp_echo/http/dns/img_registry，test.sh 啟停）、
  guest 主動（QEMU user-net 經 10.0.2.2 回連，無時序脆弱）、確定斷言
  （返回值/順序/存在性；比率只做順序化——v2.3 CPU 測只斷言先收誰）。
- stdin 一律 prompt 門控：autorun 期 console reader 會啃 early input
  （前例 `crashwrite`→`ashwrite`）；run3/run4/§11 一律 poll `^sh\$ ` 再送。
- 慢 host 判定流程：death 點推進=慢、定點=死；smp1 對照排除死鎖；
  同機背靠背排除環境；**同點掛兩次才是真 bug**，否則加 poll 預算不砍 workload。
- suite 只增不減：舊測項零退化是每版驗收第一條（重點是 root ns/cgroup0
  不限時的行為逐位元同舊）。

## 8. 版本落點（v2.4+ 候選，按需排序，另立版本檔）

- `ctr stop/rm/ps` 生命週期管理（後台語意、`ctr run -d`、組回收；空 ns 回收
  可一併做：v2.1 欠賬——僵屍 ns 目前留著）。
- overlayfs/union mount（展開式 rootfs 夠用直到镜像語意完備那天）。
- registry 認證/摘要、TLS、私有 registry（目前裸 HTTP，寫死在 v2.3）。
- CPU floor/share（CFS 式；v2.3 只有 ceiling，會餓死不斷言 floor）。
- 跨容器網路隔離（netns/veth，大版；附帶：容器內 `ps` 已隔離、`netstat` 類未）。
- volume/bind mount（持久數據進出容器的正規路；目前靠 hardlink 與整盤持久）。
- IO 權重、層級配額 enforced（`Cg.parent` 已留位，執行仍是扁平）、
  cgroupfs 偽文件系統、OOM-killer 啟發式（目前超限回 None 即全部策略）、
  `pivot_root` 語意（v2.0 欠賬）。
- 非目標重申（除非另立大版）：users/capabilities 驅動的權限檢查、
  跨 cgroup 遷移已跑進程的複雜語意、CPU 熱插拔/affinity（v1.x 範疇）。
