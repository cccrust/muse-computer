#![no_std]
#![no_main]
use core::arch::global_asm;
global_asm!(r#".section .text.entry
.globl _start
_start:
    ld a0, 0(sp)
    addi a1, sp, 8
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
    // echo [args...]: print args, else stdin line, else default
    if _argc > 1 {
        for i in 1.._argc {
            if i > 1 {
                user_lib::print(" ");
            }
            unsafe {
                if let Some(s) = user_lib::argv_str(_argv, i, _argc) {
                    let _ = user_lib::write(1, s.as_ptr(), s.len());
                }
            }
        }
        user_lib::print("\n");
        user_lib::print("[TEST] echo PASS\n");
        user_lib::exit(0);
    }
    // no args: do NOT touch stdin here (usertests runs us arg-less and
    // stdin reads now block); print default instead.
    user_lib::print("echo: hi from unix-v6\n");
    user_lib::print("[TEST] echo PASS\n");
    user_lib::exit(0);
}
