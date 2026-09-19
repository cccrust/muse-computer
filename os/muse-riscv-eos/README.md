# embed-os — RISC-V 32-bit 嵌入式作業系統 (Rust, bare-metal)

在 QEMU `virt` 機器上跑的最小多工嵌入式 OS：RV32IMAC、`no_std`、零外部依賴，
全跑在 M-mode，100 Hz 搶佔式 round-robin 排程 + UART shell。

## 功能

- 開機：清 `.bss`、設 `mtvec`、經 linker script 載入到 `0x80000000`
- 主控台：QEMU virt NS16550A UART (`0x10000000`)，polling，`print!`/`println!`
- 中斷/例外：`trap_entry` (asm) 保存 31 個暫存器 + `mepc`/`mstatus` 到 `TrapFrame`，
  `rust_trap` 分派 timer / software / external / ecall / fault
- 時鐘：CLINT `mtime`/`mtimecmp` (`0x0200BFF8`/`0x02004000`)，
  10 MHz timebase，每 10 ms 一個 tick (100 Hz)
- 排程：3 個任務 (`shell`/`blink`/`fib`)，timer 搶佔 + `ecall` 自願讓出
- 系統呼叫 (`ecall`, M-mode 內示範用)：`0 yield / 1 putc / 2 getc / 3 ticks / 4 taskid`
- Shell：`help info ps ticks echo yield whoami timer fault poweroff clear`
- Fault 處理：印 `mcause/mepc/mtval` + 全暫存器 dump；shell fault 則 halt，
  其他任務 fault 則殺掉該任務並繼續排程

## 目錄結構

```
embed-os/
├── Cargo.toml            # no_std, panic=abort, 零依賴
├── rust-toolchain.toml   # nightly + riscv32imac target
├── .cargo/config.toml    # 預設 target + -Tlinker.ld
├── linker.ld             # 載入位址 0x8000_0000, boot/trap 雙堆疊
├── src/
│   ├── main.rs           # crate root：mod＋print!宏＋rust_main＋panic
│   ├── csr.rs            # M-mode CSR 讀寫小幫手
│   ├── clint.rs          # CLINT 時鐘：mtime/mtimecmp，100 Hz
│   ├── memory.rs         # linker.ld 符號集中宣告
│   ├── uart.rs           # NS16550A 驅動 + 關中斷原子列印
│   ├── task.rs           # TrapFrame／任務表／round-robin 排程
│   ├── syscall.rs        # ecall ABI：yield/putc/getc/ticks/taskid
│   ├── trap.rs           # _start/trap_entry 組語＋rust_trap 分派
│   ├── shell.rs          # shell 行編輯＋12 個內建指令
│   └── worker.rs         # blink／fib 示範任務
├── Makefile              # build / run / test / disasm / size
├── run.sh                # QEMU 啟動腳本
└── test.sh               # 詳細自動測試 (靜態 + QEMU 動態)
```

## 快速開始

```sh
cargo build            # 或 make build
./run.sh               # QEMU 開機, shell> 出現即成功
./test.sh              # 詳細自動測試 (約 30 秒)
```

QEMU 內按 `Ctrl-A X` 強制離開，或在 shell 打 `poweroff` 正常關機。

## 記憶體配置 (QEMU virt, RAM 128 MB @ 0x80000000)

| 區段 | 位址 | 說明 |
|---|---|---|
| `.text` | `0x80000000` 起 | `_start`, `trap_entry`, 核心程式 |
| `.rodata` / `.data` | 其後 | 字串、`TASKS` (frame + 4 KiB stack × 3) |
| `.bss` | 其後 | `TICKS` 等零初始化變數 |
| boot stack | 8 KiB | `rust_main` 執行前暫用 |
| trap stack | 8 KiB | `rust_trap` 專用 (見下方) |

關鍵周邊：UART `0x10000000`，CLINT `0x02000000` (`mtime` `0x0200BFF8`,
`mtimecmp0` `0x02004000`)，SiFive test (reset/poweroff) `0x100000`。

## Trap 流程

```
任務執行中 ──timer/svc/fault──▶ trap_entry (asm)
  1. csrrw sp, mscratch, sp      # sp -> TrapFrame, 舊 sp 暫存 mscratch
  2. 存 x1..x31 (+舊 sp), mepc, mstatus
  3. sp 切到 _trap_stack_top      # ★ 一定要有，見慘痛教訓
  4. call rust_trap(frame) -> 新 frame
  5. 由新 frame 還原, mscratch 指新 frame, mret
```

列印互斥：單 hart，`_print` 以 `csrrci/csrw mstatus` 開關 MIE，
每行輸出原子完成，不用 spinlock（也避開「持鎖被搶佔」死結）。

## 開發中踩到的 bug（已修）

`trap_entry` 最早直接拿 `sp = TrapFrame` 呼叫 `rust_trap`，
Rust 的呼叫堆疊就從 frame 位址往下長，直接蓋掉隔壁 `.data`/`.rodata`：
`TASKS[0].name` 指標被改寫成 `0x88000000`，`ps` 一印任務名就
`load access fault (mcause=0x5)`，fault-dump 再讀壞掉的名字造成巢狀
trap 洗版（5 MB 的 `0xFF`）。修正：

1. 獨立 8 KiB trap stack，進 `rust_trap` 前切 `sp` 過去；
2. fault 路徑先 `mie=0` 再 dump，dump 期間不再被 timer 打斷；
3. `IN_TRAP_DUMP` 重入保護：dump 中再 fault 就安靜 `wfi` halt。

## 系統需求

- Rust nightly（含 `riscv32imac-unknown-none-elf`，見 `rust-toolchain.toml`）
- `qemu-system-riscv32`（`brew install qemu`）
- `riscv64-unknown-elf-{nm,objdump}`（`brew install riscv-gnu-toolchain`，
  僅 `make disasm/size` 與 `test.sh` 靜態檢查用；沒有的話動態測試仍可跑）
