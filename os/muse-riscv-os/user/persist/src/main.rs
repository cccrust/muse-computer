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

const PAT: &[u8] = b"PERSIST-v0.2-0123456789abcdef";
const PATH: &[u8] = b"/TESTDATA\0";

fn read_all() -> (i32, [u8; 64]) {
    let mut buf = [0u8; 64];
    let fd = user_lib::open(PATH.as_ptr(), 0);
    if fd < 0 {
        return (-1, buf);
    }
    let n = user_lib::read(fd, buf.as_mut_ptr(), buf.len());
    user_lib::close(fd);
    (n as i32, buf)
}

#[no_mangle]
pub extern "C" fn main() {
    let (n, buf) = read_all();
    if n >= 0 {
        // file exists: verify persistence across boots
        if n as usize == PAT.len() && &buf[..n as usize] == PAT {
            user_lib::print("[TEST] persist READ PASS\n");
            user_lib::exit(0);
        } else {
            user_lib::print("[TEST] persist READ FAIL\n");
            user_lib::exit(1);
        }
    }
    // first boot: create + write, then read back to verify same-boot R/W
    let fd = user_lib::open(PATH.as_ptr(), 0x40);
    if fd < 0 {
        user_lib::print("[TEST] persist WRITE FAIL (open)\n");
        user_lib::exit(1);
    }
    let w = user_lib::write(fd, PAT.as_ptr(), PAT.len());
    user_lib::close(fd);
    if w as usize != PAT.len() {
        user_lib::print("[TEST] persist WRITE FAIL (write)\n");
        user_lib::exit(1);
    }
    let (n2, buf2) = read_all();
    if n2 as usize == PAT.len() && &buf2[..n2 as usize] == PAT {
        user_lib::print("[TEST] persist WRITE PASS\n");
        user_lib::exit(0);
    } else {
        user_lib::print("[TEST] persist WRITE FAIL (reread)\n");
        user_lib::exit(1);
    }
}
