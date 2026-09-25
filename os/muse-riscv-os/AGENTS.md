# muse-riscv-os — agent notes

Workspace root is `os/muse-riscv-os/`. Run all commands from there.
Members (`Cargo.toml`): `kernel`, `user-lib`, `user/*` (~27 apps), `tools/mkfs`, `host-tests`.
Entrypoints: `kernel/src/main.rs::rust_main`, `user-lib/` (syscalls), `user/sh/` + `user/init/`, `tools/mkfs/` (builds `fs.img`). Design notes: `_doc/v*.md`.

## Build — order matters

`kernel/src/embed.rs` does `include_bytes!("../../target/riscv64gc-unknown-none-elf/release/<app>")`, so user ELFs must exist **before** the kernel builds. `run.sh` encodes the order:

```bash
TARGET=riscv64gc-unknown-none-elf
cargo build --release --target $TARGET -p init -p sh -p ls -p cat -p echo -p grep -p fork_test -p pipe_test -p usertests -p persist -p printenv -p smp_test -p reclaim_test -p stress -p udpping -p webserver -p crashwrite -p ping -p nslookup -p wget -p curl -p ctr -p sleeper -p linger -p chroot_test -p nstest -p cgtest
cargo run --release -p mkfs -- fs.img
cargo build --release --target $TARGET -p kernel
rust-objcopy -O binary target/$TARGET/release/kernel kernel.bin  # or riscv64-unknown-elf-objcopy
```

- Toolchain comes from `rust-toolchain.toml` (nightly + riscv target). Do not override.
- `.cargo/config.toml` sets `target-feature=+m,+a,+zaamo,+zmmul` for RISC-V only and intentionally has **no default target** (host-tests must build for host). Do not add one.
- Linker layouts differ per crate: `kernel/build.rs` injects `kernel/linker.ld`; user apps use root `user-linker.ld` (base `0x10000`).
- `kernel` sets `panic = "abort"`; host-testable logic lives in `kernel/src/lib.rs` (`cfg(test)` stubs for `embed.rs`), `main.rs` is `no_std` under `cfg(not(test))`.

## Run (QEMU)

`./run.sh` is interactive (`Ctrl-A X` quits). Scripted/QEMU flags:

```bash
qemu-system-riscv64 -machine virt -smp 4 -nographic -bios default -kernel kernel.bin \
  -drive file=fs.img,if=none,format=raw,id=x0,file.locking=off \
  -device virtio-blk-device,drive=x0,bus=virtio-mmio-bus.0 \
  -device virtio-net-device,netdev=n0,bus=virtio-mmio-bus.1 \
  -netdev user,id=n0,hostfwd=tcp::8080-:80
```

- blk stays on `virtio-mmio-bus.0`, net pinned to `bus.1`.
- Background/piped QEMU **must** get `< /dev/null` or it goes silent (see `test.sh` note). Prompt-gated runs poll for `^sh\$ ` / `redirenv PASS`, never fixed sleeps — autorun length varies with host load.

## Verify

- Fast gate first: `cargo test -p kernel -p host-tests -p mkfs`
- Full suite `./test.sh` is expensive (11 QEMU boots, 10+ min, `qemu.log`-`qemu7.log` assertions). Needs `qemu-system-riscv64`, `rust-objcopy`, `python3` (stub servers in `tools/`), `timeout`/`gtimeout`. Same-spot wedge twice under light load = real bug; do not just raise timeouts.

## Add a user program — 3 touches

1. `Cargo.toml` workspace `members` (+ new dir under `user/`).
2. `-p <name>` lists in **both** `run.sh` and `test.sh`.
3. `kernel/src/embed.rs`: `include_bytes!` const + `get_by_name` arm. Missing any one = stale binary or boot-time "not found".

## Do not commit

`target/`, `fs.img`, `kernel.bin`, `qemu*.log` — all regenerated (`git status` already shows them dirty).
