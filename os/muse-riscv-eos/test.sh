#!/bin/bash
# embed-os 詳細自動測試：
#   S  靜態檢查：編譯、ELF 格式、關鍵符號、位址、大小、反組譯
#   A  動態測試：QEMU 開機、多工、shell 全部指令、poweroff 正常關機
#   B  動態測試：fault 觸發、暫存器 dump、halt 後安靜（無洗版）
# 用法：./test.sh
set -u -o pipefail

ROOT="$(cd "$(dirname "$0")" && pwd)"
TARGET="riscv32imac-unknown-none-elf"
ELF_DEV="$ROOT/target/$TARGET/debug/embed-os"
ELF_REL="$ROOT/target/$TARGET/release/embed-os"
QEMU="qemu-system-riscv32"
TMPD="$(mktemp -d /tmp/embed-os-test-XXXXXX)"
trap 'rm -rf "$TMPD"' EXIT

PASS=0; FAIL=0; SKIP=0

pass() { PASS=$((PASS+1)); echo "PASS $1 ${2:-}"; }
fail() { FAIL=$((FAIL+1)); echo "FAIL $1 ${2:-}${3:+ -- $3}"; }
skip() { SKIP=$((SKIP+1)); echo "SKIP $1 ${2:-}${3:+ -- $3}"; }
section() { echo; echo "=== $1 ==="; }

# ---------- 0. 依賴 ----------
section "0. dependencies"
command -v cargo >/dev/null 2>&1 \
    && pass T0-cargo "cargo available" \
    || { fail T0-cargo "cargo not found"; echo "need Rust toolchain"; exit 2; }
command -v "$QEMU" >/dev/null 2>&1 \
    && pass T0-qemu "$QEMU available" \
    || { fail T0-qemu "$QEMU not found"; echo "hint: brew install qemu"; exit 2; }
command -v python3 >/dev/null 2>&1 \
    && pass T0-python3 "python3 available (optional)" \
    || echo "note: python3 not found (not required)"
command -v timeout >/dev/null 2>&1 \
    && pass T0-timeout "timeout available" \
    || { fail T0-timeout "timeout not found"; echo "hint: brew install coreutils"; exit 2; }
NM=""; command -v riscv64-unknown-elf-nm >/dev/null 2>&1 && NM="riscv64-unknown-elf-nm"
OBJDUMP=""; command -v riscv64-unknown-elf-objdump >/dev/null 2>&1 && OBJDUMP="riscv64-unknown-elf-objdump"
SIZEBIN=""; command -v riscv64-unknown-elf-size >/dev/null 2>&1 && SIZEBIN="riscv64-unknown-elf-size"

# ---------- S. 靜態 ----------
section "S. static checks"

if (cd "$ROOT" && cargo build 2>"$TMPD/build-dev.log"); then
    pass S1-build-dev "cargo build (dev) ok"
else
    fail S1-build-dev "cargo build (dev) failed" "$(tail -5 "$TMPD/build-dev.log")"
fi

if (cd "$ROOT" && cargo build --release 2>"$TMPD/build-rel.log"); then
    pass S2-build-release "cargo build --release ok"
else
    fail S2-build-release "cargo build --release failed" "$(tail -5 "$TMPD/build-rel.log")"
fi

if [ -f "$ELF_DEV" ]; then
    INFO="$(file -b "$ELF_DEV")"
    case "$INFO" in
        *"ELF 32-bit"*RISC-V*) pass S3-elf-rv32 "ELF 32-bit UCB RISC-V" ;;
        *) fail S3-elf-rv32 "unexpected file type" "$INFO" ;;
    esac
    # 檔案大小 < 1 MiB（bare-metal 應遠小於此）
    BYTES="$(wc -c < "$ELF_DEV" | tr -d ' ')"
    if [ "$BYTES" -lt 1048576 ]; then
        pass S3b-elf-size "ELF ${BYTES} bytes < 1 MiB"
    else
        fail S3b-elf-size "ELF too big" "${BYTES} bytes"
    fi
else
    fail S3-elf-rv32 "missing $ELF_DEV" "build first"
fi

if [ -n "$NM" ] && [ -f "$ELF_DEV" ]; then
    SYMS="$("$NM" "$ELF_DEV" 2>/dev/null | awk '{print $3}')"
    for s in _start trap_entry enter_first_task rust_main rust_trap \
             _trap_stack_top _boot_stack_top _sbss _ebss; do
        if printf '%s\n' "$SYMS" | grep -qx "$s"; then
            pass "S4-sym-$s" "symbol $s present"
        else
            fail "S4-sym-$s" "symbol $s MISSING"
        fi
    done
    START_ADDR="$("$NM" "$ELF_DEV" 2>/dev/null | awk '$3=="_start"{print $1}')"
    if [ "$START_ADDR" = "80000000" ]; then
        pass S5-entry-addr "_start @ 0x80000000"
    else
        fail S5-entry-addr "_start address wrong" "got ${START_ADDR:-?}"
    fi
else
    skip S4-symbols "riscv64-unknown-elf-nm not installed"
    skip S5-entry-addr "nm not installed"
fi

if [ -n "$SIZEBIN" ] && [ -f "$ELF_DEV" ]; then
    # text+data+bss 總量應 < 128 KiB（含 3×4 KiB 任務堆疊 + 2×8 KiB 系統堆疊）
    TOTAL="$("$SIZEBIN" -B "$ELF_DEV" 2>/dev/null | awk 'NR==2{print $1+$2+$3}')"
    if [ -n "$TOTAL" ] && [ "$TOTAL" -lt 131072 ]; then
        pass S6-mem-footprint "text+data+bss = ${TOTAL} bytes < 128 KiB"
    else
        fail S6-mem-footprint "footprint too big" "${TOTAL:-?} bytes"
    fi
else
    skip S6-mem-footprint "riscv64-unknown-elf-size not installed"
fi

if [ -n "$OBJDUMP" ] && [ -f "$ELF_DEV" ]; then
    DUMP="$TMPD/disasm.txt"
    "$OBJDUMP" -d "$ELF_DEV" > "$DUMP" 2>/dev/null
    for insn in mret csrrw wfi ecall unimp; do
        if grep -qw "$insn" "$DUMP"; then
            pass "S7-asm-$insn" "disassembly contains $insn"
        else
            fail "S7-asm-$insn" "disassembly missing $insn"
        fi
    done
    # RV32：不該出現 rv64 專用 ld/sd（排除 c.ld/c.sd 误判用詞邊界）
    if grep -E -q '(^|[[:space:]])ld[[:space:]]|(^|[[:space:]])sd[[:space:]]' "$DUMP"; then
        fail S7b-no-rv64 "found 64-bit ld/sd in RV32 image"
    else
        pass S7b-no-rv64 "no 64-bit ld/sd (pure RV32)"
    fi
else
    skip S7-asm "riscv64-unknown-elf-objdump not installed"
fi

# ---------- A/B. 動態 ----------
# QEMU stdio 有個特性：stdin 若是「開啟中、暫無資料」的 pipe 且 stdout
# 也是 pipe 時，guest 輸出不會流動；但 stdin=paced feeder + stdout=file
# 的組合是驗證過可行的。因此 driver 策略：用帶 sleep 的 feeder 依序送
# 入指令、輸出存檔、結束後再 grep 斷言（不定時互動、簡單可重現）。
section "A/B. dynamic checks (QEMU)"

QEMU_ARGS="-machine virt -nographic -bios none -kernel $ELF_DEV"

# -- run A：功能全測，以 poweroff 正常關機收尾 --
{
    sleep 2; printf 'help\r'
    sleep 1; printf 'info\r'
    sleep 1; printf 'ps\r'
    sleep 1; printf 'ticks\r'
    sleep 1; printf 'echo hello123\r'
    sleep 1; printf 'whoami\r'
    sleep 1; printf 'yield\r'
    sleep 1; printf 'timer\r'
    sleep 2; printf 'poweroff\r'
    sleep 2
} | timeout 60 $QEMU $QEMU_ARGS > "$TMPD/run-a.log" 2>&1
RC_A=$?

expect() { # $1=id $2=desc $3=grep-pattern [$4=log]
    if grep -a -E -q "$3" "${4:-$TMPD/run-a.log}"; then
        pass "$1" "$2"
    else
        fail "$1" "$2" "pattern /$3/ not found"
    fi
}

expect A1-boot-banner  "boot banner"            "embed-os 0\.1\.0"
expect A2-scheduler     "scheduler start"        "scheduler start: 3 tasks @ 100 Hz"
expect A3-shell-prompt  "shell ready"            "shell>"
expect A4-blink         "timer preempts blink"   "\[blink\] count=[0-9]+ tick=[0-9]+"
expect A5-fib           "timer preempts fib"     "\[fib\] iter=[0-9]+ fib24=46368 tick=[0-9]+"
expect A6-help          "help lists commands"    "commands:"
expect A7-info          "info shows uart map"    "uart=0x10000000"
expect A8a-ps-shell     "ps shows shell alive"   '\*0 +shell +1'
expect A8b-ps-blink     "ps shows blink alive"   '1 +blink +1'
expect A8c-ps-fib       "ps shows fib alive"     '2 +fib +1'
expect A9-ticks         "tick counter + uptime"  "ticks=[0-9]+ uptime=[0-9]+\.[0-9]+s"
expect A11-whoami       "syscall taskid"         "task 0"
expect A12-yield        "ecall yield works"      "yielded \(task 0\)"
expect A13-timer        "raw CLINT readout"      "mtime=[0-9]+ mtimecmp=[0-9]+ ticks=[0-9]+"
expect A14-poweroff-msg "poweroff announces"     "poweroff via SiFive"
# echo 斷言：輸入回顯 "echo hello123" 含 1 次 + 指令輸出 1 次 = 至少 2 次
if [ "$(grep -a -o 'hello123' "$TMPD/run-a.log" | wc -l | tr -d ' ')" -ge 2 ]; then
    pass A10-echo "echo round-trip"
else
    fail A10-echo "echo round-trip" "hello123 appears < 2 times"
fi
if [ "$RC_A" -eq 0 ]; then
    pass A15-poweroff-exit "qemu self-exits rc=0"
else
    fail A15-poweroff-exit "qemu exit code wrong" "rc=$RC_A (want 0)"
fi

# -- run B：fault 注入 --
{
    sleep 2; printf 'fault\r'
    sleep 4
} | timeout 30 $QEMU $QEMU_ARGS > "$TMPD/run-b.log" 2>&1
RC_B=$?
# B run 預期被 timeout 砍掉（fault 後 halt 不退出），rc=124 正常
if [ "$RC_B" -eq 124 ]; then
    pass B0-halt-hang "guest halts after fault (timeout kill rc=124)"
else
    fail B0-halt-hang "unexpected qemu exit" "rc=$RC_B (want 124)"
fi
expect B2-fault-msg  "fault announced"      "triggering illegal instruction" "$TMPD/run-b.log"
expect B3-fatal-dump 'mcause=0x2 + task'    'FATAL. illegal instruction \(mcause=0x2 mepc=0x[0-9a-f]+ mtval=0x0\) on task 0 "shell"' "$TMPD/run-b.log"
expect B4-reg-dump   "register dump"        "ra=0x[0-9a-f]+ sp=0x[0-9a-f]+" "$TMPD/run-b.log"
# halt 後應安靜：FATAL 恰 1 次、總量 < 4 KiB（防巢狀 trap 洗版迴歸）
if [ "$(grep -a -c 'FATAL' "$TMPD/run-b.log" | tr -d ' ')" -eq 1 ]; then
    pass B5-single-fatal "exactly one FATAL"
else
    fail B5-single-fatal "FATAL count != 1" "$(grep -a -c 'FATAL' "$TMPD/run-b.log") times"
fi
BYTES_B="$(wc -c < "$TMPD/run-b.log" | tr -d ' ')"
if [ "$BYTES_B" -lt 4096 ]; then
    pass B6-halt-quiet "post-halt quiet (${BYTES_B}B < 4KiB)"
else
    fail B6-halt-quiet "output flood after halt" "${BYTES_B} bytes"
fi

# ---------- 總結 ----------
section "RESULT"
echo "passed=$PASS failed=$FAIL skipped=$SKIP"
if [ "$FAIL" -eq 0 ]; then
    echo "ALL GREEN"
    exit 0
else
    echo "SOME CHECKS FAILED (logs: $TMPD)"
    trap - EXIT  # 保留現場供除錯
    echo "kept: $TMPD"
    exit 1
fi
