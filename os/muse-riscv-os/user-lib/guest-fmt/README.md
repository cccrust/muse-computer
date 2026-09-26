# guest-fmt

Allocation-free number formatting for muse-riscv-os guest programs
(and anywhere else that formats into stack buffers without `std`).

No dependencies, `core`-only: builds for host and `riscv64gc` alike.
Part of the muse-riscv-os v3.8 guest-library set (see `_doc/v3.8.md`).

```rust
let mut buf = [0u8; 20];
let n = guest_fmt::fmt_dec(&mut buf, 12345);
assert_eq!(&buf[..n], b"12345");
```

All functions saturate into the given buffer (never panic, never wrap);
callers that need exactness size buffers at 20 (dec), 16 (hex), 23 (oct)
bytes for a full `usize` on 64-bit.
