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

// v1.0 SMP smoke test: fork 4 CPU-bound children (one per hart, hopefully),
// each spins N rounds yielding, then exits with 40+i. Parent reaps all and
// checks codes. Hart placement is informational only (printed, not asserted);
// completion of all four is the deterministic assertion.
#[no_mangle]
pub extern "C" fn main(_argc: usize, _argv: *const *const u8) {
    let mut kids = [0isize; 4];
    for i in 0..4 {
        let pid = user_lib::fork();
        if pid == 0 {
            let h0 = user_lib::gethart();
            let mut acc: usize = 0;
            let mut k = 0;
            while k < 400 {
                acc = acc.wrapping_add(k * (i + 1));
                if k % 50 == 0 {
                    user_lib::yield_();
                }
                k += 1;
            }
            let h1 = user_lib::gethart();
            // report harts as "[smp] child i hart a->b" (info only)
            user_lib::print("[smp] child on hart ");
            print_isize(h0);
            user_lib::print("->");
            print_isize(h1);
            user_lib::print("\n");
            let _ = acc;
            user_lib::exit(40 + i as i32);
        } else if pid > 0 {
            kids[i] = pid;
        } else {
            user_lib::print("[TEST] smp FAIL (fork)\n");
            user_lib::exit(1);
        }
    }
    let mut ok = true;
    let mut done = [false; 4];
    let mut code: i32 = 0;
    loop {
        let mut all = true;
        for i in 0..4 {
            if !done[i] {
                all = false;
            }
        }
        if all {
            break;
        }
        let w = user_lib::waitpid(-1, &mut code as *mut i32, 0);
        if w == -2 {
            user_lib::yield_();
            continue;
        }
        if w < 0 {
            ok = false;
            break;
        }
        let mut known = false;
        for i in 0..4 {
            if !done[i] && kids[i] == w {
                done[i] = true;
                known = true;
                if code != 40 + i as i32 {
                    ok = false;
                }
                break;
            }
        }
        if !known {
            ok = false;
        }
    }
    if ok {
        user_lib::print("[TEST] smp PASS\n");
    } else {
        user_lib::print("[TEST] smp FAIL\n");
    }
    user_lib::exit(0);
}

fn print_isize(v: isize) {
    // hart ids are single digits; keep this tiny and obviously correct
    let mut b = [b'?', 0u8];
    if (0..10).contains(&v) {
        b[0] = b'0' + v as u8;
        let _ = user_lib::write(1, b.as_ptr(), 1);
    } else {
        let _ = user_lib::write(1, b.as_ptr(), 1);
    }
}
