#!/bin/bash
set -x
cd "$(dirname "$0")"
TARGET=riscv64gc-unknown-none-elf
PASS=1

echo "=== 1. host unit tests ==="
cargo test -p kernel -p host-tests -p mkfs || PASS=0

echo "=== 2. build user ELFs ==="
cargo build --release --target $TARGET -p init -p sh -p ls -p cat -p echo -p grep -p fork_test -p pipe_test -p usertests -p persist -p printenv -p smp_test -p reclaim_test -p stress -p udpping -p webserver || PASS=0

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

echo "=== 5. QEMU boot test (180s; v1.0: -smp 4 MTTCG is slower) ==="
rm -f qemu.log
if command -v timeout >/dev/null 2>&1; then
  TO="timeout 45"
  TO1="timeout 180"
  TO2="timeout 60"
elif command -v gtimeout >/dev/null 2>&1; then
  TO="gtimeout 45"
  TO1="gtimeout 180"
  TO2="gtimeout 60"
else
  TO=""
  TO1=""
  TO2=""
fi
START=$(date +%s)
# NOTE: stdin must come from /dev/null. With `-nographic`, if stdin is the
# user's controlling terminal while stdout is a pipe (tee), QEMU stays silent
# (no output at all, guest never boots). run.sh keeps interactive stdin.
# v1.0: run1 uses TO1 (120s) -- the full autorun no longer fits in 25s on -smp 4.
# v1.3: TO1 180s -- loaded hosts (load>4) slow MTTCG several-fold. Rule of
# thumb: wedging at the SAME spot twice under light load = real bug and
# must be dug, not papered with more timeout.
# v1.3: net pinned to mmio bus.1 (0x10002000); the kernel probes by DEVID
# (QEMU auto-attach otherwise fills from the top -- observed bus.7).
# v1.3: UDP echo server for udpping (background, killed after §6).
python3 tools/udp_echo.py > /tmp/muse-echo.log 2>&1 &
ECHO_PID=$!
sleep 1
# v1.5: run1 keeps QEMU alive in background so the host web client can
# fetch from the guest webserver mid-run (a timeout-killed QEMU can't be
# fetched from afterwards). Sequence: boot bg -> web_fetch (30s) -> wait
# for autorun end (redirenv, ~150s budget) -> kill QEMU -> assertions.
qemu-system-riscv64 \
  -machine virt -smp 4 \
  -nographic \
  -bios default \
  -kernel "$KBIN" \
  -drive file=fs.img,if=none,format=raw,id=x0 \
  -device virtio-blk-device,drive=x0,bus=virtio-mmio-bus.0 \
  -device virtio-net-device,netdev=n0,bus=virtio-mmio-bus.1 \
  -netdev user,id=n0,hostfwd=tcp::8080-:80 \
  < /dev/null > qemu.log 2>&1 &
QEMU_PID=$!
echo "QEMU bg pid $QEMU_PID (log qemu.log)"
# v1.5: wait for the guest webserver (backgrounded in autorun) before
# fetching -- boot+autorun take ~60-90s even unloaded.
for i in $(seq 1 150); do
  if grep -q "webserver listening" qemu.log 2>/dev/null; then
    echo "webserver up"
    break
  fi
  if ! kill -0 $QEMU_PID 2>/dev/null; then
    echo "QEMU exited early"
    break
  fi
  sleep 1
done
python3 tools/web_fetch.py || PASS=0
# wait for autorun to finish (or budget out)
for i in $(seq 1 150); do
  if grep -q "redirenv PASS" qemu.log 2>/dev/null; then
    echo "autorun finished"
    break
  fi
  if ! kill -0 $QEMU_PID 2>/dev/null; then
    echo "QEMU exited early"
    break
  fi
  sleep 1
done
kill $QEMU_PID 2>/dev/null || true
wait $QEMU_PID 2>/dev/null || true
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
check "smp PASS"
check "reclaim PASS"
check "stress DONE"
check "net PASS"
check "net-dev PASS"
check "web PASS"
check "ipi PASS"
check "hart0 up"
check "hart1 up"
check "hart2 up"
check "hart3 up"
check "mmap PASS"
check "VIRTIO"
check "virtio-irq PASS"
check "virtio-blk RW PASS"
check "disk mount ok"
check "fsck: bitmap rebuilt"
check "persist WRITE PASS"
check "link PASS"
check "cat ARG PASS"
check "hello-arg"
check "bg PASS"
check "kill PASS"
check "sleep PASS"
check "chdir PASS"
check "append PASS"
check "lseek PASS"
check "dup2 PASS"
check "waitpid PASS"
check "df PASS"
check "quote PASS"
check "env PASS"
check "fg PASS"
check "history PASS"
check "ps PASS"
check "STRACE"
check "glob PASS"
check "envp PASS"
check "redirenv PASS"
if grep -q "PANIC" qemu.log; then
  echo "FAIL: PANIC found"; PASS=0
else
  echo "OK: no PANIC"
fi
# v1.3: stop the UDP echo server (run1 only)
kill $ECHO_PID 2>/dev/null || true

echo "=== 7. persistence: second boot on SAME fs.img (no rebuild) ==="
# v1.4: 60s -- persist READ needs deep autorun (past stress), which loaded
# hosts can't reach in 25s.
rm -f qemu2.log
$TO2 qemu-system-riscv64 \
  -machine virt -smp 4 \
  -nographic \
  -bios default \
  -kernel "$KBIN" \
  -drive file=fs.img,if=none,format=raw,id=x0 \
  -device virtio-blk-device,drive=x0,bus=virtio-mmio-bus.0 \
  -device virtio-net-device,netdev=n0,bus=virtio-mmio-bus.1 \
  -netdev user,id=n0,hostfwd=tcp::8080-:80 \
  < /dev/null 2>&1 | tee qemu2.log || true

if [ ! -s qemu2.log ]; then
  echo "FAIL: qemu2.log empty"
  PASS=0
else
  if grep -q "persist READ PASS" qemu2.log; then
    echo "OK: persist READ PASS"
  else
    echo "FAIL: missing [persist READ PASS] (data did not survive reboot)";
    PASS=0
  fi
  if grep -q "PANIC" qemu2.log; then
    echo "FAIL: PANIC in second boot"; PASS=0
  else
    echo "OK: no PANIC (second boot)"
  fi
fi

echo "=== 8. interactive: Ctrl-C kills foreground sh, respawn, halt ==="
rm -f qemu3.log
# NOTE: stdin is a pipe here (not a tty), so no QEMU silence issue.
# Ctrl-C at ~15s hits sh blocked at prompt; respawned sh then reads halt.
(sleep 15; printf '\003'; sleep 7; printf 'halt\n') | $TO qemu-system-riscv64 \
  -machine virt -smp 4 \
  -nographic \
  -bios default \
  -kernel "$KBIN" \
  -drive file=fs.img,if=none,format=raw,id=x0 \
  -device virtio-blk-device,drive=x0,bus=virtio-mmio-bus.0 \
  -device virtio-net-device,netdev=n0,bus=virtio-mmio-bus.1 \
  -netdev user,id=n0,hostfwd=tcp::8080-:80 \
  2>&1 | tee qemu3.log || true

if [ ! -s qemu3.log ]; then
  echo "FAIL: qemu3.log empty"
  PASS=0
else
  if grep -q "killed" qemu3.log; then
    echo "OK: Ctrl-C killed fg"
  else
    echo "FAIL: missing [killed] (Ctrl-C did nothing)";
    PASS=0
  fi
  if grep -q "respawn" qemu3.log; then
    echo "OK: sh respawned"
  else
    echo "FAIL: missing [respawn]";
    PASS=0
  fi
  if grep -q "halting" qemu3.log; then
    echo "OK: halted cleanly"
  else
    echo "FAIL: missing [halting]";
    PASS=0
  fi
  # v0.10: cache stats print on clean shutdown (numbers vary; presence only)
  if grep -q "cache stats" qemu3.log; then
    echo "OK: cache stats (run3)"
  else
    echo "FAIL: missing [cache stats] in qemu3.log";
    PASS=0
  fi
  # v1.2: scheduler balance + contention verdict lines on clean shutdown
  # (numbers vary; presence only -- values feed the v1.4 lock decision)
  if grep -q "steals=" qemu3.log; then
    echo "OK: steals line (run3)"
  else
    echo "FAIL: missing [steals=] in qemu3.log";
    PASS=0
  fi
  if grep -q "contention sched=" qemu3.log; then
    echo "OK: contention line (run3)"
  else
    echo "FAIL: missing [contention sched=] in qemu3.log";
    PASS=0
  fi
  # v0.6: THRE delivery is only observable when an external trap claims it;
  # run3's RX traffic (Ctrl-C + halt) forces claim passes (see _doc/v0.6.md)
  if grep -q "uart-tx-irq PASS" qemu3.log; then
    echo "OK: uart-tx-irq delivered (run3)"
  else
    echo "FAIL: missing [uart-tx-irq PASS] in qemu3.log";
    PASS=0
  fi
  if grep -q "PANIC" qemu3.log; then
    echo "FAIL: PANIC in third boot"; PASS=0
  else
    echo "OK: no PANIC (third boot)"
  fi
fi

echo "=== 9. clean-halt reboot: halt again on the same image ==="
rm -f qemu4.log
# v0.12: prompt is up by ~18s (autorun grew); early input buffers in the
# UART ring, so an 18s halt is safe even if the prompt is not ready yet.
(sleep 18; printf 'halt\n') | $TO qemu-system-riscv64 \
  -machine virt -smp 4 \
  -nographic \
  -bios default \
  -kernel "$KBIN" \
  -drive file=fs.img,if=none,format=raw,id=x0 \
  -device virtio-blk-device,drive=x0,bus=virtio-mmio-bus.0 \
  -device virtio-net-device,netdev=n0,bus=virtio-mmio-bus.1 \
  -netdev user,id=n0,hostfwd=tcp::8080-:80 \
  2>&1 | tee qemu4.log || true

if [ ! -s qemu4.log ]; then
  echo "FAIL: qemu4.log empty"
  PASS=0
else
  if grep -q "halting" qemu4.log; then
    echo "OK: halted cleanly (run4)"
  else
    echo "FAIL: missing [halting] in qemu4.log";
    PASS=0
  fi
  if grep -q "PANIC" qemu4.log; then
    echo "FAIL: PANIC in fourth boot"; PASS=0
  else
    echo "OK: no PANIC (fourth boot)"
  fi
fi

echo "=== 10. reboot after clean halt: mount must NOT need fsck ==="
rm -f qemu5.log
$TO qemu-system-riscv64 \
  -machine virt -smp 4 \
  -nographic \
  -bios default \
  -kernel "$KBIN" \
  -drive file=fs.img,if=none,format=raw,id=x0 \
  -device virtio-blk-device,drive=x0,bus=virtio-mmio-bus.0 \
  -device virtio-net-device,netdev=n0,bus=virtio-mmio-bus.1 \
  -netdev user,id=n0,hostfwd=tcp::8080-:80 \
  < /dev/null 2>&1 | tee qemu5.log || true

if [ ! -s qemu5.log ]; then
  echo "FAIL: qemu5.log empty"
  PASS=0
else
  if grep -q "disk mount ok" qemu5.log; then
    echo "OK: disk mount ok (run5)"
  else
    echo "FAIL: missing [disk mount ok] in qemu5.log";
    PASS=0
  fi
  # absence assertion (v0.12): clean shutdown cleared the dirty flag,
  # so mount must NOT rebuild the bitmap
  if grep -q "fsck: bitmap rebuilt" qemu5.log; then
    echo "FAIL: unexpected [fsck: bitmap rebuilt] in qemu5.log (dirty flag not cleared)";
    PASS=0
  else
    echo "OK: clean mount, no fsck rebuild (run5)"
  fi
  if grep -q "PANIC" qemu5.log; then
    echo "FAIL: PANIC in fifth boot"; PASS=0
  else
    echo "OK: no PANIC (fifth boot)"
  fi
fi

if [ "$PASS" = "1" ]; then
  echo "ALL TESTS PASSED"
  exit 0
else
  echo "SOME TESTS FAILED (see qemu.log)"
  exit 1
fi
