#![no_std]
#![no_main]
use core::arch::global_asm;
// NOTE: keep in sync with the other user apps (v0.11 env capture).
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

// v1.2 SMP burner: 300 rounds of fork/exit churn (child sbrk's 64K and
// touches it), every 10th round a pipe echo for pipe-path contention.
// v2.0: trimmed to 100 rounds x 16K -- full weight made loaded-host suite
// runs exceed every budget (each round = fork-clone + faults + exit +
// teardown + RFENCE shootdown; ~1-2s emulated-loaded). Still a real burner
// (100 reaps + frame churn + pipe contention); use manual soak runs for more.
// Silent until the end (no log flood); survival is the assertion.
// Doubles as reclaim soak + sched-lock contention load for v1.4's verdict.
#[no_mangle]
pub extern "C" fn main(_argc: usize, _argv: *const *const u8) {
    let mut ok = true;
    let mut i = 0;
    while i < 100 {
        if i % 10 == 9 {
            if !pipe_round() {
                ok = false;
                break;
            }
        } else if !fork_round() {
            ok = false;
            break;
        }
        i += 1;
    }
    if ok {
        user_lib::print("[TEST] stress DONE\n");
    } else {
        user_lib::print("[TEST] stress FAIL\n");
    }
    user_lib::exit(0);
}

fn fork_round() -> bool {
    let pid = user_lib::fork();
    if pid == 0 {
        let r = user_lib::sbrk(16 * 1024);
        if r < 0 {
            user_lib::exit(10);
        }
        let p = r as *mut u8;
        let mut k = 0usize;
        while k < 16 * 1024 {
            unsafe {
                core::ptr::write_volatile(p.add(k), (k & 0xff) as u8);
            }
            k += 4096;
        }
        user_lib::exit(0);
    } else if pid > 0 {
        let mut code: i32 = -1;
        loop {
            let w = user_lib::waitpid(pid, &mut code as *mut i32, 0);
            if w == -2 {
                user_lib::yield_();
                continue;
            }
            return w == pid && code == 0;
        }
    } else {
        false
    }
}

fn pipe_round() -> bool {
    let mut fds = [0i32; 2];
    if user_lib::pipe(fds.as_mut_ptr()) != 0 {
        return false;
    }
    let pid = user_lib::fork();
    if pid == 0 {
        user_lib::close(fds[0] as isize);
        let msg = b"stress-ping";
        let r = user_lib::write(fds[1] as isize, msg.as_ptr(), msg.len());
        user_lib::close(fds[1] as isize);
        user_lib::exit(if r == msg.len() as isize { 0 } else { 11 });
    } else if pid > 0 {
        user_lib::close(fds[1] as isize);
        let mut buf = [0u8; 16];
        let mut n = 0usize;
        let mut dead = false;
        // pipe read is non-blocking (0 = empty, retry): poll until 11
        // bytes or the child dies (then fail via the reap path below).
        while n < 11 {
            let r = user_lib::read(fds[0] as isize, unsafe { buf.as_mut_ptr().add(n) }, 11 - n);
            if r > 0 {
                n += r as usize;
                continue;
            }
            let mut code: i32 = -1;
            let w = user_lib::waitpid(pid, &mut code as *mut i32, 1);
            if w == pid {
                dead = true;
                break;
            }
            user_lib::yield_();
        }
        user_lib::close(fds[0] as isize);
        if dead || n != 11 {
            return false;
        }
        let mut code: i32 = -1;
        loop {
            let w = user_lib::waitpid(pid, &mut code as *mut i32, 0);
            if w == -2 {
                user_lib::yield_();
                continue;
            }
            return w == pid && code == 0 && buf[..11] == *b"stress-ping";
        }
    } else {
        false
    }
}
