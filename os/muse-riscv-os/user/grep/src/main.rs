#![no_std]
#![no_main]
use core::arch::global_asm;
global_asm!(r#".section .text.entry
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
    // grep [pat]: filter stdin for pattern (default "h")
    let mut pat = b"h".as_slice();
    unsafe {
        if let Some(s) = user_lib::argv_str(_argv, 1, _argc) {
            if !s.is_empty() {
                pat = s;
            }
        }
    }
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
    if !pat.is_empty() && total >= pat.len() {
        for i in 0..=total - pat.len() {
            if &buf[i..i + pat.len()] == pat {
                matched = true;
                break;
            }
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
