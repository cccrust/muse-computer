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
#[no_mangle]
pub extern "C" fn main(_argc: usize, _argv: *const *const u8) {
    // print inherited environment, one "K=V" per line (v0.11).
    // With no args: all entries. With args: only entries whose key matches.
    let n = user_lib::env_count();
    for i in 0..n {
        unsafe {
            if let Some(s) = user_lib::env_str(i) {
                if _argc > 1 && !key_match(_argv, _argc, s) {
                    continue;
                }
                let _ = user_lib::write(1, s.as_ptr(), s.len());
                user_lib::print("\n");
            }
        }
    }
    user_lib::exit(0);
}

// true if s starts with "KEY=" for one of argv[1..]
fn key_match(argv: *const *const u8, argc: usize, s: &[u8]) -> bool {
    for i in 1..argc {
        unsafe {
            if let Some(k) = user_lib::argv_str(argv, i, argc) {
                if k.len() + 1 <= s.len()
                    && &s[..k.len()] == k
                    && s[k.len()] == b'='
                {
                    return true;
                }
            }
        }
    }
    false
}
