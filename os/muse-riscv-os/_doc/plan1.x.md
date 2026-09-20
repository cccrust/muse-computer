# muse-riscv-os v1.x 規劃：SMP（多核心）

> 狀態：v0.x 已結案（v0.12，`test.sh` 47 項全過，單 hart）。v1.x 主線是把
> 全系統從「單 hart + 隨處假設」升級為「多 hart 正確」，效能是次要目標。
> 本計畫第 1–7 節為總綱，各版落點見 `v1.0.md` 起的版本檔。

## 0. 環境與前提

- QEMU `virt` + OpenSBI（SBI 有 `hsm`、`rfnc`，開機 banner 可驗），S-mode kernel。
- 起步 `-smp 4`（一開始就 4 核：少核的 SMP 很多競態跑不出來；N>4 調參往後放）。
- Rust nightly，零外部依賴維持（SBI 呼叫全部手寫 `ecall`）。

## 1. 工程骨架（v1.0 改動面）

```
kernel/src/entry.S     # mhartid 分流：hart0 -> rust_main，各 AP -> secondary 入口
kernel/src/sbi.rs      # + hsm_hart_start / rfnc_remote_sfence_vma / send_ipi
kernel/src/task/mod.rs # current/yield per-hart 化，全域大鎖排程（沿用 SpinMutex）
kernel/src/trap.rs     # per-hart trap stack、kernel_sp 隨遷移更新、PLIC 多 context
kernel/src/plic.rs     # 各 hart S-mode context（hart h -> context 2h+1）全開
kernel/src/timer.rs    # per-hart set_timer；TICKS 轉 AtomicU64
kernel/src/mem/*       # remote sfence 包裝；unmap/protect/exec 後打 RFENCE
kernel/src/fs/*        # CACHE 持鎖跨 block 修掉（見 4）；裸 static 審計
kernel/src/uart.rs     # TX kick/ISR 跨核競態處理（見 4）
tools/mkfs, user/*     # 基本不動；新增 user/smp_test（見 6）
run.sh/test.sh         # -smp 2；QEMU 版本需求註明
```

## 2. 啟動與特權流（v1.0）

- AP 喚醒：boot hart 經 `HSM hart_start(hartid, _start_secondary, tp=hartid)` 叫醒副核（QEMU+OpenSBI 支援；fallback：entry.S 內 `wfi` parking + boot hart 寫 release flag——先做 HSM，fallback 留坑）。
- `tp` 存 hartid（entry 即設，全核 invariant，trap/schedule 都靠它，不再有全域 current）。
- 每核獨立：boot stack 16K×N、trap stack 16K×N（`TRAP_STACK: [[u8;16384]; MAX_HART]`）、`kernel_sp` 在每次 pick 時重填（任務遷移安全）。
- `sscratch` 是 per-hart CSR，無共享問題；`TRAPFRAME_VA` 全核同值。
- PLIC：priority 全設 1；enable/threshold/claim 按 context 配（UART/virtio 每核都開）。

## 3. 記憶體與 TLB（v1.0 即做對，不留債）

- `sfence.vma` 只刷本地：`activate()` 的本地全刷保留（context switch 照舊安全）。
- 凡是「改了別核可能快取著的映射」→ 打 `RFENCE remote_sfence_vma`：`unmap`（munmap/sbrk-shrink）、`protect`（BSS 權限提升）、`exec` 換 root 後舊 root 丟棄前。`SBI RFENCE`（EID 0x52464E43）OpenSBI 有，不用自幹 IPI 協議。
- Fresh frame（從未映射過）免 shootdown（不可能有 stale entry）；ASID 仍全 0（v1.x 不碰 ASID 管理）。
- `copy_kernel_tables`/`clone_user`/`map_user` 本體不動（呼叫路徑已在鎖內或單核 boot 期）。

## 4. 併發正確性審計（v1.0 必做，逐項打勾）

- 原則：**SpinMutex 只保護「不 yield 的短臨界」**；凡睡眠一律 sleeping-wait（既有 `block_current` + wake 模式）。
- 已知要修：`blk::read` 持 CACHE 鎖跨 `virtio::read_block`（內含 block+yield）→ 改 drop 後重取（re-check）；同理掃 `pipe`/`REG`/console 是否有持鎖睡眠。
- `static mut` 逐個定性：SCHED（已有鎖，但 `current`/`yield_flag` 改 per-hart 陣列）、FG_PID（轉 AtomicUsize）、TICKS（轉 AtomicU64）、disk 掛載狀態（mount 在 AP 起跑前完成，之後唯讀，加註）、virtio 队列狀態（DRV sleeping lock 已在 v0.11 備好，跨核照用）、UART ring（SpinMutex  OK；kick 與他核 ISR 的 THR 競態可接受：最壞多一次中斷，不死結——putchar 臨界內不睡眠是鐵律）、TX_ON/TX_MARKED（write-once/冪等，註明即可）。
- `kill`/`exit`/`waitpid`/`fork` 全在 SCHED 大鎖短臨界內（現況如此，保持）。
- 被 trace/println：trap 內印字不取新鎖（現況如此，保持）。

## 5. 排程（v1.0 保守，v1.1 再擴）

- v1.0：**全域 queue + 大鎖**（現在的 queue 不變量「等待者不在 queue」沿用）；`current: [usize; MAX_HART]`；`schedule_point` 用 `tp` 取核；wake 方 push 回全域 queue（被喚醒任務可遷移，TF 是 PA、可任意核進入）。
- `set_yield_flag`/`take_yield_flag` per-hart 化；timer tick 只搶佔本核。
- 4 核共用大鎖在 bring-up 階段可接受（正確性先行，爭用數據 v1.1 再量）。
- v1.1：per-CPU runqueue + 偷工作/負載平衡 + IPI 搶佔（跨核 yield）；v1.0 先用全域鎖跑通正確性。

## 6. run/test（v1.0）

- `run.sh`/`test.sh`：QEMU 加 `-smp 4`（其餘參數不動；stdin 管線 quirks 沿用 v0.x 註記）。
- 新增 `user/smp_test`：fork 四個 CPU-bound 自旋子行程（`yield` 交出，佔滿 4 核），父 `waitpid` 全收並校驗 exit code，印 `smp PASS`；`test.sh` run1 加斷言。
- 既有 47 項全數保留（尤其 kill/waitpid/bg 在真併發下重驗；時序類斷言窗口可能要放寬，見 7）。
- 每核開機印 `hart<N> up`（`test.sh` 斷言 `hart1/2/3 up` 全出現，證明 3 個 AP 都起來了）。

## 7. 風險與動工順序

1. AP 啟動（entry.S 分流 + HSM + per-hart stack/tp）→ 以 `hart1/2/3 up` 驗。
2. 裸 static/鎖審計 + CACHE 持鎖修掉 → 單核測試先全綠（`-smp 1` 回歸）。
3. per-hart current/yield/trap-stack + PLIC 多 context + per-hart timer → `-smp 4` 開機。
4. RFENCE 接線（unmap/protect/exec）→ `smp_test` + 全套件。
5. `test.sh` 斷言 + 整理文件。
- 風險：AP 競態（3 個副核啟動時序交錯，比 2 核更容易撞；HSM 逐個起、每核印 banner 確認）、IPI 遺失（RFENCE 是 SBI 保證，比自幹可靠）、TLB 殘留症狀=幽靈 fault（沿用 v0.11 playbook：pid 印字、strace、`objdump`）、QEMU smp 下時序變慢（MTTCG 調度、超時窗口要留餘量，`test.sh` 的 25s 可能要放寬）、QEMU 版本差異（smp/topology，`qemu --version` 先記）。
- 非目標（v1.0）：4 核以上調優、NUMA（無）、CPU 熱插拔、affinity API、per-CPU 排程（v1.1）、virtio-net、journal（v1.2+ 候選，v0.12 已鋪好測試前提）。
