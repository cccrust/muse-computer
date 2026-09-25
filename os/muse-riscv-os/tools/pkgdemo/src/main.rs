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

// v3.2: pipeline demo program. `fortune [args...]` prints
// `fortune-sez <args...>` (one line; no args -> bare marker).
// Built by tools/pkgbuild.py straight from this crate dir
// (never through the main workspace build).
#[no_mangle]
pub extern "C" fn main(argc: usize, argv: *const *const u8) {
    user_lib::print("fortune-sez");
    let mut i = 1usize;
    while i < argc {
        if let Some(s) = unsafe { user_lib::argv_str(argv, i, argc) } {
            user_lib::print(" ");
            let _ = user_lib::write(1, s.as_ptr(), s.len());
        }
        i += 1;
    }
    user_lib::print("\n");
    user_lib::exit(0);
}
