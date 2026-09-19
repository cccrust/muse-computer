#![no_std]
#![no_main]
use core::arch::global_asm;
global_asm!(r#".section .text.entry
.globl _start
_start:
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
pub extern "C" fn main() {
    let me = user_lib::getpid();
    let pid = user_lib::fork();
    if pid == 0 {
        let c = user_lib::getpid();
        user_lib::print("[USER] fork_test child alive\n");
        // prove sbrk works
        let b = user_lib::sbrk(4096);
        if b > 0 {
            user_lib::print("[USER] fork_test child sbrk ok\n");
        }
        user_lib::exit(42);
    } else if pid > 0 {
        let mut code: i32 = 0;
        loop {
            let w = user_lib::wait(&mut code as *mut i32);
            if w == -2 {
                user_lib::yield_();
                continue;
            }
            if w == pid {
                break;
            }
            if w < 0 {
                break;
            }
        }
        if code == 42 {
            user_lib::print("[TEST] fork PASS\n");
        } else {
            user_lib::print("[TEST] fork FAIL\n");
        }
    } else {
        user_lib::print("[TEST] fork FAIL (fork<0)\n");
    }
    let _ = me;
    user_lib::exit(0);
}
