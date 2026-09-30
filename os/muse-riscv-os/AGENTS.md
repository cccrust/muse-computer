# muse-riscv-os — agent notes

Workspace root is `os/muse-riscv-os/`. Run all commands from there.
Members (`Cargo.toml`): `kernel`, `user-lib`, `user-lib/guest-tls`, `user/*` (~28 apps), `tools/mkfs`, `host-tests`.
Entrypoints: `kernel/src/main.rs::rust_main`, `user-lib/` (syscalls), `user/sh/` + `user/init/`, `tools/mkfs/` (builds `fs.img`). Design notes: `_doc/v*.md`.

## Build — order matters

`kernel/src/embed.rs` does `include_bytes!("../../target/riscv64gc-unknown-none-elf/release/<app>")`, so user ELFs must exist **before** the kernel builds. `run.sh` encodes the order:

```bash
TARGET=riscv64gc-unknown-none-elf
cargo build --release --target $TARGET -p init -p sh -p ls -p cat -p echo -p grep -p fork_test -p pipe_test -p usertests -p persist -p printenv -p smp_test -p reclaim_test -p stress -p udpping -p webserver -p crashwrite -p ping -p nslookup -p wget -p wgets -p curl -p ctr -p sleeper -p linger -p chroot_test -p nstest -p cgtest
cargo run --release -p mkfs -- fs.img
cargo build --release --target $TARGET -p kernel
rust-objcopy -O binary target/$TARGET/release/kernel kernel.bin  # or riscv64-unknown-elf-objcopy
```

- Toolchain comes from `rust-toolchain.toml` (nightly + riscv target). Do not override.
- `.cargo/config.toml` sets `target-feature=+m,+a,+zaamo,+zmmul` for RISC-V only and intentionally has **no default target** (host-tests must build for host). Do not add one.
- Linker layouts differ per crate: `kernel/build.rs` injects `kernel/linker.ld`; user apps use root `user-linker.ld` (base `0x10000`).
- `kernel` sets `panic = "abort"`; host-testable logic lives in `kernel/src/lib.rs` (`cfg(test)` stubs for `embed.rs`), `main.rs` is `no_std` under `cfg(not(test))`.
- argv cap is 16 everywhere (sh `run_args`/interactive/pipe/bg, kernel `sys_exec`/`sys_execve`/`push_args`). A 12-token `ctr run -d --memory … --cpu … --weight …` needs it; do not reintroduce an 8-cap on any layer.

## Run (QEMU)

`./run.sh` is interactive (`Ctrl-A X` quits). Scripted/QEMU flags:

```bash
qemu-system-riscv64 -machine virt -smp 4 -nographic -bios default -kernel kernel.bin \
  -drive file=fs.img,if=none,format=raw,id=x0,file.locking=off \
  -device virtio-blk-device,drive=x0,bus=virtio-mmio-bus.0 \
  -device virtio-net-device,netdev=n0,bus=virtio-mmio-bus.1 \
  -netdev user,id=n0,hostfwd=tcp::8080-:80
```

(`test.sh` + `tools/web_fetch.py` use `8081`, not `8080`; `run.sh` keeps `8080`.)

- blk stays on `virtio-mmio-bus.0`, net pinned to `bus.1`.
- Background/piped QEMU **must** get `< /dev/null` or it goes silent (see `test.sh` note). Prompt-gated runs poll for `^sh\$ ` / `redirenv PASS`, never fixed sleeps — autorun length varies with host load.
- Suite markers must print in a SINGLE `write()` call: the console lock
  covers one syscall at a time, and a forked child exec-prints
  concurrently on another hart — per-byte prints split mid-line and
  whole-line greps fail (seen twice on the `detached` line).

## Verify

- Fast gate first: `cargo test -p kernel -p host-tests -p mkfs -p user-lib -p guest-tls`
  plus `cargo test` inside `user-lib/guest-args/` and `user-lib/guest-fmt/`
  (standalone crates, not `-p` addressable; `guest-tls` is a workspace
  member instead — `wgets` path-depends on it, and cargo rejects
  member→detached path deps)
- Full suite `./test.sh` is expensive (11 QEMU boots, 10+ min, `qemu.log`-`qemu7.log` assertions). Needs `qemu-system-riscv64`, `rust-objcopy`, `python3` (stub servers in `tools/`), `timeout`/`gtimeout`. Stub servers (udp/http/dns/registry) stay alive through §11 — killing them at §6 strands the later boots' full-autorun prefix in connect-retry burns (>280s for a 36s autorun). Prompt-feeder polls (360) + outer timeouts must cover loaded-host autorun (~250s at load ~5.8); a blind-fed command is nibbled and FAILs. Same-spot wedge twice under light load = real bug; do not just raise timeouts.

## Add a user program — 4 touches

1. `Cargo.toml` workspace `members` (+ new dir under `user/`).
2. `-p <name>` lists in **both** `run.sh` and `test.sh`.
3. `kernel/src/embed.rs`: `include_bytes!` const + `get_by_name` arm
   (+ `cfg(test)` stub const).
4. `tools/mkfs/src/main.rs` `names` list (`/bin/<name>` in `fs.img`).
   Missing any one = stale binary or boot-time "not found".
   NOTE: with disk mounted, `/bin/*` comes from `fs.img` (touch 4);
   embed (touch 3) is only the missing-file fallback + ramfs path.
   Build in SEPARATE cargo invocations (apps → mkfs → kernel):
   one `-p sh -p kernel` line races (kernel embeds a stale sh silently).

Exception: `tools/pkgdemo/` is a standalone crate (own `[workspace]`,
built by `tools/pkgbuild.py`, never in `fs.img`/`embed.rs`) — do NOT
add it to the workspace; the registry packs it at startup.

## Version commits

`target/` is never committed. `fs.img`, `kernel.bin`, `qemu*.log` are
regenerated every run but version commits snapshot them anyway (see v3.13)
— always as part of a green `test.sh` version commit, never standalone.
