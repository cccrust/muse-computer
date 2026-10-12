# Bug report：偶發全機凍結（sched-lock 被帶進 trap regress）

- 狀態：未解（open）。v3.16 調查中。
- 嚴重性：高。`test.sh` 11 個 boots 任一都可能中（used-image 約 25%，fresh 極少），中一次整輪作廢。
- 引入版本：pre-existing（v3.14 二進位即有同徵兆；v3.15/v3.16 皆未動相關路徑）。

## 1. 現象

- Guest log 在固定區域附近停止推進（常見：`ctr run -v … volc …`
  exec 後，或 stress fork storm 中），之後零輸出、無 PANIC，QEMU
  進程活著。等 90s～550s 不會自己恢復。
- QEMU monitor 實測（多次凍結一致）：
  - 3 hart pc = `SpinMutex<Sched>::lock_counted` 自旋；
    第 4 hart pc = sepc = `__trap_entry`，scause=12
    （instruction page fault），stval = entry 本身。
  - `SCHED.locked` = 1；per-hart 臨界區計數（v3.16 調查期曾短暫加入
    的 `INTR_NEST`，已 revert）全為 1——鎖有人拿著，但無 hart 在
    臨界區內推進。
  - 手動 walk 當前 satp root 表：entry 全零。trap 向量頁在該表無映射，
    每個 trap 重 fault → 無限迴歸。
- 觸發條件：host 高負載（load ≥4 區間易中，load ~2 十二連 boots 全過）；
  同一 image 重開 outcome 不同（timing，非確定性 state）；
  磁碟 59%（非滿）、fsck/journal 正常、stub 存活不影響（stub 全死時
  反而跑得快——RST 快速失敗 vs retry-burn 的差別，不要誤判為 hang）。

## 2. 已排除（有證據）

| 假設 | 證偽 |
|---|---|
| timer-ISR 重入自死鎖（SIE 未 mask） | 實作 xv6 式 push_off/pop_off 後凍結依舊、徵兆相同；已 revert |
| 磁碟滿 / fsck / journal | 凍結 image 59% used；replay + bitmap rebuild 正常 |
| stub 半殘造成 | stub 全死只會變快；真凍結是零推進（90s+ 行數不動） |
| v3.15 引入（installs/socket 改動） | v3.14 二進位同徵兆；installs 段在凍結點之後 |
| 全局 NSOCK 耗盡（v3.15 的另一個 bug，已修） | 該 bug 的徵兆是 `wget FAIL (socket)` + log 繼續推進，與本凍結互斥 |

## 3. 目前最可能的鏈（未證實，需追查）

1. 某 hart 在拿著 sched-lock 的臨界區內觸發 synchronous fault。
2. fault 當下 satp 指向一張缺 kernel 映射的表（全零 root），連 trap
   entry 本身都 fault → sepc/stval 被洗成 entry，原始 fault 位置丟失。
3. regress 佔著鎖不放，其餘 hart 在 `lock_counted` 轉到天荒地老。

關鍵未知數（依可查性排序）：
1. 那張全零 root 表從哪來？候選：exec 建表未完成就 `pt::activate`
  （`task/mod.rs` exec 段 `activate` 在 sched-lock 內）；reap 釋放了
   仍在用的地址空間（stale `current[]`，v2.0 註解承認此類 stale 存在）；
   `tf_pa` 錯亂導致 `*tf = TrapFrame::empty()` 歸零了頁表頁。
2. 第一下 fault 的位置（需在 regress 蓋掉 sepc 前抓到，或加臨時追蹤）。
3. 為何 fresh image 幾乎不中：timing（journal replay +1~2s 改變交錯）
   還是真的 state 相關——目前證據偏 timing（同 image 重開結果不同）。

## 4. 重現步驟

1. 正常跑完一次完整 autorun 的 fs.img（或任意 used image）。
2. host 加載到 load ≥4（Chrome + 幾個編譯進程即可）。
3. `qemu-system-riscv64 -machine virt -smp 4 … -kernel kernel.bin`
   跑 autorun，盯 volc 段（~1500 行）或 stress fork storm。
4. 凍結確認：log 90s 行數不動 + monitor `info registers`
   看到上述簽名。附帶 monitor 指令見 `_doc/v3.16.md`（QEMU
   `-monitor unix:/tmp/mon.sock,server,nowait` + `xp` 讀鎖字）。

## 5. 修復方向（未動工）

- 短期止血（治標）：凍結只發生在 autorun 段；suite 層面只能靠低負載
  窗口 + 重跑。目前不建議加自動恢復（會掩蓋真 bug）。
- 根治（治本）：按 §3 候選逐一排查——`pt::activate` 調用點審計、
  exec 建表的 kernel 映射完整性、`free_user_space`/reap 與
  `current[]` 的生命週期。修好後驗收：高負載連開 5 boots 零凍結
  + `test.sh` 連綠兩輪。
- 另有獨立的理論風險（未觀測到，不動）：NET/UART 鎖在 ISR 也拿得到，
  同類自死鎖存在；`sync.rs` 保持裸 spinlock 是有意的（FS/DRIVER 的
  IRQ-等待路徑未審計，不敢全改）。

## 6. 附帶發現（已修，不屬本 bug）

- v3.15 調查期發現 32-slot socket 耗盡（exit 不回收 + close 後 30s
  lingering + fetch burst），修法：reap 關死進程 kind-7 fds +
  滿表時回收最老 closing slot。詳見 `_doc/v3.15.md §5`。
