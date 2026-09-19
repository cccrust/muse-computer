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
    // cat /README (or stdin if no file support for args; simplified)
    let p = b"/README\0";
    let fd = user_lib::open(p.as_ptr(), 0);
    if fd < 0 {
        user_lib::print("cat: cannot open /README\n");
        user_lib::exit(1);
    }
    let mut buf = [0u8; 256];
    loop {
        let n = user_lib::read(fd, buf.as_mut_ptr(), buf.len());
        if n <= 0 {
            break;
        }
        let _ = user_lib::write(1, buf.as_ptr(), n as usize);
    }
    user_lib::close(fd);
    user_lib::print("[TEST] cat PASS\n");
    user_lib::exit(0);
}
