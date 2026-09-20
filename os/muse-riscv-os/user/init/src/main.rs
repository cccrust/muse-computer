#![no_std]
#![no_main]
use core::arch::global_asm;
global_asm!(r#"
.section .text.entry
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
    user_lib::print("[USER] init: starting sh\n");
    let sh = b"/bin/sh\0";
    loop {
        let pid = user_lib::fork();
        if pid == 0 {
            let r = user_lib::exec(sh.as_ptr(), 0);
            user_lib::print("[USER] init: exec sh failed\n");
            user_lib::exit(-1);
        } else if pid > 0 {
            let mut code: i32 = 0;
            loop {
                let w = user_lib::wait(&mut code as *mut i32);
                if w == -2 {
                    user_lib::yield_();
                    continue;
                }
                break;
            }
            user_lib::print("[USER] init: sh exited, respawn\n");
        } else {
            user_lib::print("[USER] init: fork failed\n");
        }
    }
}
