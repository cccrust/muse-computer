#!/bin/bash
set -e
cd "$(dirname "$0")"
TARGET=riscv64gc-unknown-none-elf

echo "=== build user apps (independent ELF) ==="
cargo build --release --target $TARGET -p init -p sh -p ls -p cat -p echo -p grep -p fork_test -p pipe_test -p usertests -p persist -p printenv -p smp_test -p reclaim_test -p stress -p udpping -p webserver -p crashwrite -p ping -p nslookup -p wget -p curl -p ctr -p chroot_test

echo "=== mkfs fs.img ==="
cargo run --release -p mkfs -- fs.img

echo "=== build kernel ==="
cargo build --release --target $TARGET -p kernel

KELF="target/$TARGET/release/kernel"
KBIN="kernel.bin"
if command -v rust-objcopy >/dev/null 2>&1; then
  OBJCOPY="rust-objcopy"
elif command -v riscv64-unknown-elf-objcopy >/dev/null 2>&1; then
  OBJCOPY="riscv64-unknown-elf-objcopy"
else
  echo "no objcopy found"; exit 1
fi
$OBJCOPY -O binary "$KELF" "$KBIN"
ls -lh "$KBIN"

echo "=== run QEMU virt (Ctrl-A X to quit) ==="
# v1.3: net pinned to mmio bus.1 (0x10002000); the kernel probes by DEVID
# so any slot works, but pinning keeps layout stable. blk stays on bus.0.
# Start tools/udp_echo.py on the host for udpping.
exec qemu-system-riscv64 \
  -machine virt -smp 4 \
  -nographic \
  -bios default \
  -kernel "$KBIN" \
  -drive file=fs.img,if=none,format=raw,id=x0,file.locking=off \
  -device virtio-blk-device,drive=x0,bus=virtio-mmio-bus.0 \
  -device virtio-net-device,netdev=n0,bus=virtio-mmio-bus.1 \
  -netdev user,id=n0,hostfwd=tcp::8080-:80
