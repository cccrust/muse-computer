#![no_std]
#![no_main]
use core::arch::global_asm;
// NOTE: keep in sync with the other user apps (v0.11 env capture).
global_asm!(r#".section .text.entry
.globl _start
_start:
    ld a0, 0(sp)
    addi a1, sp, 8
    slli t0, a0, 3
    addi t0, t0, 16
    add t1, a1, t0
    la t2, ENVIRON_P
    sd t1, 0(t2)
    ld t0, -8(t1)
    la t2, ENVIRON_C
    sd t0, 0(t2)
    call main
    li a0, 0
    li a7, 2
    ecall
"#);
#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    user_lib::exit(-1);
    loop {}
}

// v3.9: crates.io end-to-end demo. `repeat <n> <word>` prints
// `1 word` .. `n word` (one per line) using guest-args (parse) and
// guest-fmt (line numbers) from crates.io. Built by tools/pkgbuild.py
// straight from this crate dir (never through the workspace build).
#[no_mangle]
pub extern "C" fn main(argc: usize, argv: *const *const u8) {
    if argc != 3 {
        user_lib::print("usage: repeat <n> <word>\n");
        user_lib::exit(1);
    }
    let ns = unsafe { user_lib::argv_str(argv, 1, argc).unwrap_or(b"") };
    let word = unsafe { user_lib::argv_str(argv, 2, argc).unwrap_or(b"") };
    let n = match guest_args::parse_dec(ns) {
        Some(v) => v,
        None => {
            user_lib::print("repeat: bad count\n");
            user_lib::exit(1);
        }
    };
    if n > 1000 || word.is_empty() {
        user_lib::print("repeat: bad count\n");
        user_lib::exit(1);
    }
    let mut i = 1usize;
    while i <= n {
        let mut line = [0u8; 96];
        let mut m = guest_fmt::fmt_dec(&mut line, i);
        if m < line.len() {
            line[m] = b' ';
            m += 1;
        }
        let w = word.len().min(line.len() - m);
        line[m..m + w].copy_from_slice(&word[..w]);
        m += w;
        if m < line.len() {
            line[m] = b'\n';
            m += 1;
        }
        let mut o = 0;
        while o < m {
            let r = user_lib::write(1, unsafe { line.as_ptr().add(o) }, m - o);
            if r <= 0 {
                user_lib::exit(1);
            }
            o += r as usize;
        }
        i += 1;
    }
    user_lib::exit(0);
}
