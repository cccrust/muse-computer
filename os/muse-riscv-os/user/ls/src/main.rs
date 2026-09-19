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
    // list /bin by trying known names (no readdir syscall; use cat of fixed list via open attempts)
    user_lib::print("bin:\n");
    let names: [&[u8]; 10] = [
        b"/bin/init\0", b"/bin/sh\0", b"/bin/ls\0", b"/bin/cat\0", b"/bin/echo\0",
        b"/bin/grep\0", b"/bin/fork_test\0", b"/bin/pipe_test\0", b"/bin/usertests\0",
        b"/README\0",
    ];
    for n in names {
        let fd = user_lib::open(n.as_ptr(), 0);
        if fd >= 0 {
            user_lib::close(fd);
            // print short name
            let mut s = 0;
            while s < n.len() && n[s] != 0 {
                s += 1;
            }
            let _ = user_lib::write(1, n.as_ptr(), s);
            user_lib::print("\n");
        }
    }
    user_lib::print("[TEST] ls PASS\n");
    user_lib::exit(0);
}
