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
    // grep hi: filter stdin for 'h'
    let mut buf = [0u8; 512];
    let mut total = 0;
    loop {
        let n = user_lib::read(0, unsafe { buf.as_mut_ptr().add(total) }, buf.len() - total);
        if n <= 0 {
            break;
        }
        total += n as usize;
        if total >= buf.len() {
            break;
        }
        // check newline -> process line
        if buf[total - 1] == b'\n' {
            break;
        }
    }
    let mut matched = false;
    for i in 0..total {
        if buf[i] == b'h' {
            matched = true;
            break;
        }
    }
    if matched {
        let _ = user_lib::write(1, buf.as_ptr(), total);
        user_lib::print("[TEST] grep PASS\n");
    } else {
        user_lib::print("grep: no match\n");
        user_lib::print("[TEST] grep DONE\n");
    }
    user_lib::exit(0);
}
