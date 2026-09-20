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
    let mut fds = [0i32; 2];
    if user_lib::pipe(fds.as_mut_ptr()) != 0 {
        user_lib::print("[TEST] pipe FAIL (pipe)\n");
        user_lib::exit(1);
    }
    let pid = user_lib::fork();
    if pid == 0 {
        user_lib::close(fds[0] as isize);
        let msg = b"hello-pipe";
        let _ = user_lib::write(fds[1] as isize, msg.as_ptr(), msg.len());
        user_lib::close(fds[1] as isize);
        user_lib::exit(0);
    } else if pid > 0 {
        user_lib::close(fds[1] as isize);
        let mut buf = [0u8; 32];
        // wait a bit then read
        let mut code: i32 = 0;
        loop {
            let w = user_lib::wait(&mut code as *mut i32);
            if w == -2 {
                // try read anyway
                break;
            }
            break;
        }
        let n = user_lib::read(fds[0] as isize, buf.as_mut_ptr(), 10);
        user_lib::close(fds[0] as isize);
        if n == 10 && buf[0] == b'h' {
            user_lib::print("[TEST] pipe PASS\n");
        } else {
            user_lib::print("[TEST] pipe FAIL (data)\n");
        }
    } else {
        user_lib::print("[TEST] pipe FAIL (fork)\n");
    }
    user_lib::exit(0);
}
