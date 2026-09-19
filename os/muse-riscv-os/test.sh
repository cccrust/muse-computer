#!/bin/bash
set -x
cd "$(dirname "$0")"
TARGET=riscv64gc-unknown-none-elf
PASS=1

echo "=== 1. host unit tests ==="
cargo test -p kernel -p host-tests || PASS=0

echo "=== 2. build user ELFs ==="
cargo build --release --target $TARGET -p init -p sh -p ls -p cat -p echo -p grep -p fork_test -p pipe_test -p usertests || PASS=0

echo "=== 3. mkfs ==="
cargo run --release -p mkfs -- fs.img || PASS=0

echo "=== 4. build kernel ==="
cargo build --release --target $TARGET -p kernel || PASS=0

KELF="target/$TARGET/release/kernel"
KBIN="kernel.bin"
if command -v rust-objcopy >/dev/null 2>&1; then
  OBJCOPY="rust-objcopy"
elif command -v riscv64-unknown-elf-objcopy >/dev/null 2>&1; then
  OBJCOPY="riscv64-unknown-elf-objcopy"
else
  echo "FAIL: no objcopy"; exit 1
fi
$OBJCOPY -O binary "$KELF" "$KBIN" || PASS=0
test -s "$KBIN" || { echo "FAIL: kernel.bin empty"; PASS=0; }

echo "=== 5. QEMU boot test (25s) ==="
rm -f qemu.log
if command -v timeout >/dev/null 2>&1; then
  TO="timeout 25"
elif command -v gtimeout >/dev/null 2>&1; then
  TO="gtimeout 25"
else
  TO=""
fi
START=$(date +%s)
# NOTE: stdin must come from /dev/null. With `-nographic`, if stdin is the
# user's controlling terminal while stdout is a pipe (tee), QEMU stays silent
# (no output at all, guest never boots). run.sh keeps interactive stdin.
$TO qemu-system-riscv64 \
  -machine virt \
  -nographic \
  -bios default \
  -kernel "$KBIN" \
  -drive file=fs.img,if=none,format=raw,id=x0,file.locking=off \
  -device virtio-blk-device,drive=x0,bus=virtio-mmio-bus.0 \
  < /dev/null 2>&1 | tee qemu.log || true
END=$(date +%s)
echo "QEMU ran for $((END-START))s, qemu.log bytes: $(wc -c < qemu.log)"

if [ ! -s qemu.log ]; then
  echo "FAIL: qemu.log empty (QEMU produced no output)"
  qemu-system-riscv64 --version | head -n 1 || true
  ls -lh "$KBIN" fs.img || true
  PASS=0
fi

echo "=== 6. assertions ==="
check() {
  if grep -q "$1" qemu.log; then
    echo "OK: $1"
  else
    echo "FAIL: missing [$1]";
    PASS=0
  fi
}
check "SV39 enabled"
check "spawn init"
check "fork PASS"
check "pipe PASS"
check "usertests PASS"
check "VIRTIO"
if grep -q "PANIC" qemu.log; then
  echo "FAIL: PANIC found"; PASS=0
else
  echo "OK: no PANIC"
fi

if [ "$PASS" = "1" ]; then
  echo "ALL TESTS PASSED"
  exit 0
else
  echo "SOME TESTS FAILED (see qemu.log)"
  exit 1
fi
