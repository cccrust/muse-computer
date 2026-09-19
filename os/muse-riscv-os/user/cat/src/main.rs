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
    // cat [file]: argv[1] or /README fallback
    let mut path = [0u8; 64];
    let use_arg = unsafe {
        match user_lib::argv_str(_argv, 1, _argc) {
            Some(s) if !s.is_empty() => {
                let n = s.len().min(62);
                path[..n].copy_from_slice(&s[..n]);
                path[n] = 0;
                true
            }
            _ => false,
        }
    };
    let pp: *const u8 = if use_arg { path.as_ptr() } else { b"/README\0".as_ptr() };
    let fd = user_lib::open(pp, 0);
    if fd < 0 {
        user_lib::print("cat: cannot open\n");
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
    if use_arg {
        user_lib::print("[TEST] cat ARG PASS\n");
    } else {
        user_lib::print("[TEST] cat PASS\n");
    }
    user_lib::exit(0);
}
