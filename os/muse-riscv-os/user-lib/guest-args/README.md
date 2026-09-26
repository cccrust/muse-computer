# guest-args

Tiny argv helpers for muse-riscv-os guest programs (and anywhere else
that parses byte-slice args without `std`).

No dependencies, `core`-only: builds for host and `riscv64gc` alike.
Part of the muse-riscv-os v3.8 guest-library set (see `_doc/v3.8.md`).

```rust
let (name, want) = guest_args::split_vreq(b"hello>=1.0");
assert_eq!(name, b"hello");
assert_eq!(guest_args::parse_mem(b"16M"), Some(4096));
```
