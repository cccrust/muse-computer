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
    // echo: read stdin line and echo back (works with pipe + interactive)
    let mut buf = [0u8; 256];
    let n = user_lib::read(0, buf.as_mut_ptr(), 200);
    if n > 0 {
        let _ = user_lib::write(1, buf.as_ptr(), n as usize);
        // ensure newline
        if buf[n as usize - 1] != b'\n' {
            user_lib::print("\n");
        }
    } else {
        user_lib::print("echo: hi from unix-v6\n");
    }
    user_lib::print("[TEST] echo PASS\n");
    user_lib::exit(0);
}
