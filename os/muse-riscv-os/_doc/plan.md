# RV64 OS Plan (Rust, SV39 + Multi-Process + Unix v6)

使用者選擇：完整 v6 體驗 / 兩種測試都要 / 獨立 ELF 載入。

> 狀態：v0.1 已結案（見 `_doc/v0.1.md`），本計畫第 1–9 節為原始規劃，
> 實際落點與取捨見文末 §10；下一版規劃見 `_doc/v0.2.md`（僅規劃，未實作）。

## 0. 環境
- 空目錄，nightly rust + riscv64gc-unknown-none-elf OK
- qemu-10.2.2 + riscv64-elf-gcc OK
- 路線: QEMU virt + OpenSBI default bios, S-mode kernel

## 1. 工程骨架
```
Cargo.toml (workspace: kernel, user-lib, user apps, tools/mkfs, host-tests)
rust-toolchain.toml -> nightly
.cargo/config.toml -> build-std core,alloc, target riscv64gc-unknown-none-elf
kernel/linker.ld BASE 0x80200000
kernel/src/{main.rs, lib.rs, entry.s, switch.s, sbi.rs, console.rs, uart.rs,
  trap.rs, timer.rs,
  memory/{mod.rs, addr.rs, frame.rs, page_table.rs, heap.rs},
  process/{mod.rs, pcb.rs, context.rs, scheduler.rs, pid.rs, elf.rs},
  syscall/{mod.rs, proc.rs, fs.rs, mm.rs},
  fs/{mod.rs, ramfs.rs, pipe.rs, fd.rs, virtio_blk.rs},
  shell.rs, tests.rs}
user/user-lib/{syscall_wrap.rs, ulib.rs, linker-user.ld}
user/{init, sh, ls, cat, echo, grep, fork_test, pipe_test, usertests}.rs
tools/mkfs.rs (host 端打包 fs.img, 可選)
run.sh, test.sh
```

## 2. 啟動與特權流
- entry.s: boot_stack -> 清 BSS -> rust_main (S-mode, OpenSBI 已在 M-mode)
- sbi.rs: SBI v0.1 console_putchar(1) + shutdown(8) + set_timer(0)
- uart.rs: NS16550A 0x10000000, console.rs + print! 巨集
- trap.rs: stvec 分 kernel_trap/user_trap, sscratch+TrapFrame,
  處理 ecall(8), timer(5), external(9), sret 回 U-mode

## 3. SV39 MMU
- addr.rs: VPN[2:0] 9+9+9 + offset 12
- page_table.rs: PTE V/R/W/X/U/G/A/D + PPN, 三層 512-entry,
  satp = 8<<60 | ppn, sfence.vma
- frame.rs: ekernel..MEMORY_END(0x88000000,128M) bitmap/stack 分配器
- MemorySet: kernel 恆等映射 + trampoline, user U|R/W/X + stack + heap(brk/sbrk)
- 流程: heap_init -> frame_init -> kernel MemorySet -> activate(satp)
  -> 印 `[MMU] SV39 enabled`

## 4. 多進程
- pcb.rs: Pid, TrapFrame, PageToken(MemorySet), fd_table, brk, children, exit_code, state
- switch.s: __switch 只存 ra/sp/s0-s11
- scheduler.rs: RR + 10ms timer 搶佔, fork 完整 copy MemorySet,
  exec 清表重載 ELF
- elf.rs: xmas-elf 解析 LOAD 段, map_va -> alloc_frame -> copy,
  建 argc/user_sp

## 5. Syscall (xv6 相容編號子集)
fork, exec, wait, exit, kill, sleep, getpid, sbrk/brk,
open, close, read, write, dup, pipe, chdir, mkdir, mknod,
fstat, link, unlink. a7 分發, a0-a2 傳參.

## 6. FS + virtio-blk
- virtio_blk.rs: MMIO 0x10001000 polling 驅動, 開機印 VIRTIO-BLK found
- RamFS + 目錄樹 (/bin, /etc, README), EasyFS 概念 (inode bitmap, direct/indirect)
  mkfs 在 host 打包 fs.img (若時程緊先 RamFS, 再升級磁碟持久化)
- pipe.rs: ring-buffer, 支援 sh | grep
- fd 0/1/2: stdin/console stdout/stderr

## 7. User ELF + Shell
- user/ 各程式獨立 riscv64gc-unknown-none-elf 編譯, mkfs/embed 打包
- kernel 以 include_bytes! 嵌入 ELF, loader 在 U-mode 執行
- init -> sh, sh 支援 ls/cat/echo/grep/fork_test/pipe_test/usertests, `| > < & ;`

## 8. run.sh / test.sh
- run.sh: cargo build --release + rust-objcopy -O binary -> kernel.bin
  + mkfs -> fs.img + qemu-system-riscv64 -machine virt -nographic
  -bios default -kernel kernel.bin -drive virtio-blk ...
- test.sh (set -x): cargo test (host: PTE/frame/sched) + build 檢查
  + timeout 20 qemu 斷言 SV39 enabled / spawn init / fork PASS / pipe PASS /
  usertests PASS / 無 PANIC

## 9. 執行順序
1. 骨架 + hello + run.sh 打通 QEMU
2. console/trap/timer 3. frame/SV39 4. process/sched
5. syscall/ELF 6. virtio/fs/shell 7. test.sh + host 單元測試
風險: virtio+fs 最大, 卡關先 RamFS 保 fork/exec/pipe 再升級.

## 10. v0.1 結算（實際落點）
- 1–9 全數完成，但與原計畫有三處取捨：
  1. `build-std` 未用（target 已預裝 core，零依賴即可編譯）；linker script 改由各 crate `build.rs` 按 `TARGET` 注入（kernel 與 user 布局不同，共用 `.cargo/config` 會衝突）。
  2. ELF 解析自幹（未用 xmas-elf），維持零依賴。
  3. FS 停在 RamFS + virtio-blk probe：`fs.img`（MUSEFS01）只有 mkfs 在寫，kernel 未掛載——即 plan §6 括號裡的降級路線，也是 v0.2 的主戰場。
- 實作中抓到的三個真 bug（供後人參考）：PTE branch 不可帶 A/D；`enter_user` 的 `sched().lock().procs[current_pid()]` 巢狀死鎖；`sbi::set_timer` 行內組語未宣告 clobber 造成 miscompile。
- 測試環境坑：QEMU `-nographic` 在 stdin 為 controlling terminal 且 stdout 為管線時零輸出，`test.sh` 以 `< /dev/null` 繞過；`run.sh` 保持互動式。
