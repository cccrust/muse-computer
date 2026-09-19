#!/bin/sh
# Run embed-os under QEMU (RISC-V 32-bit virt machine).
# Usage: ./run.sh [--release]
# Exit guest: type `poweroff` in the shell, or press Ctrl-A X.
set -u

ROOT="$(cd "$(dirname "$0")" && pwd)"
TARGET="riscv32imac-unknown-none-elf"

if [ "${1:-}" = "--release" ]; then
    ELF="$ROOT/target/$TARGET/release/embed-os"
    [ -x "$ELF" ] || (cd "$ROOT" && cargo build --release)
else
    ELF="$ROOT/target/$TARGET/debug/embed-os"
    [ -x "$ELF" ] || (cd "$ROOT" && cargo build)
fi

if ! command -v qemu-system-riscv32 >/dev/null 2>&1; then
    echo "error: qemu-system-riscv32 not found (brew install qemu)" >&2
    exit 2
fi

echo "tip: type 'poweroff' in the guest shell, or press Ctrl-A X to quit"
exec qemu-system-riscv32 -machine virt -nographic -bios none -kernel "$ELF"
