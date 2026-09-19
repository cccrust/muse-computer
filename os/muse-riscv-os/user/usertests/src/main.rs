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

fn run(path: &[u8]) {
    let pid = user_lib::fork();
    if pid == 0 {
        let _ = user_lib::exec(path.as_ptr(), 0);
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
    }
}

#[no_mangle]
pub extern "C" fn main(_argc: usize, _argv: *const *const u8) {
    user_lib::print("[USER] usertests: start\n");
    run(b"/bin/fork_test\0");
    run(b"/bin/pipe_test\0");
    run(b"/bin/ls\0");
    run(b"/bin/cat\0");
    run(b"/bin/echo\0");
    user_lib::print("[TEST] usertests PASS\n");
    user_lib::exit(0);
}
