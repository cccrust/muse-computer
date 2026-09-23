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

// v2.1 pid-namespace self-test (deterministic, no timing).
// Parent unshares before each fork (one-shot): 4 children each found a
// fresh ns. Each child asserts getpid()==1, yields a while, exits(40+i).
// The first child additionally forks a grandchild (same ns, lpid==2).
// Parent reaps all by GLOBAL pids (fork's return) and checks codes.
// Cross-ns value comparisons are deliberately avoided (global-vs-lpid
// confusion is a test-design pitfall, not kernel behavior).
fn fail(msg: &str) -> ! {
    user_lib::print("[TEST] ns FAIL (");
    user_lib::print(msg);
    user_lib::print(")\n");
    user_lib::exit(1);
}

fn child_body(i: i32) -> ! {
    if user_lib::getpid() != 1 {
        fail("child-lpid");
    }
    if i == 0 {
        // grandchild stays in this ns with lpid 2
        let g = user_lib::fork();
        if g == 0 {
            if user_lib::getpid() != 2 {
                fail("grand-lpid");
            }
            let mut k = 0;
            while k < 100 {
                user_lib::yield_();
                k += 1;
            }
            user_lib::exit(50);
        } else if g > 0 {
            let mut code: i32 = -1;
            loop {
                let w = user_lib::waitpid(g, &mut code as *mut i32, 0);
                if w == -2 {
                    user_lib::yield_();
                    continue;
                }
                if w != g || code != 50 {
                    fail("grand-reap");
                }
                break;
            }
        } else {
            fail("grand-fork");
        }
    }
    let mut k = 0;
    while k < 200 {
        user_lib::yield_();
        k += 1;
    }
    user_lib::exit(40 + i);
}

#[no_mangle]
pub extern "C" fn main(_argc: usize, _argv: *const *const u8) {
    // bad flags rejected, no side effects
    if user_lib::unshare(0) != -1 {
        fail("badflags");
    }
    if user_lib::unshare(12345) != -1 {
        fail("badflags2");
    }
    // parent's own pid unchanged (still root ns, global view)
    let me = user_lib::getpid();
    if me <= 1 {
        fail("parent-pid");
    }
    let mut kids = [0isize; 4];
    let mut i = 0;
    while i < 4 {
        if user_lib::unshare(user_lib::CLONE_NEWPID) != 0 {
            fail("unshare");
        }
        let pid = user_lib::fork();
        if pid == 0 {
            child_body(i);
        } else if pid > 0 {
            kids[i as usize] = pid;
        } else {
            fail("fork");
        }
        i += 1;
    }
    // reap all four by global pid, check codes
    let mut done = [false; 4];
    let mut code: i32 = 0;
    loop {
        let mut all = true;
        for d in done.iter() {
            if !*d {
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
            fail("wait");
        }
        let mut known = false;
        let mut j = 0;
        while j < 4 {
            if !done[j] && kids[j] == w {
                done[j] = true;
                known = true;
                if code != 40 + j as i32 {
                    fail("code");
                }
                break;
            }
            j += 1;
        }
        if !known {
            fail("unknown-child");
        }
    }
    // parent still itself (unshare affects children only)
    if user_lib::getpid() != me {
        fail("parent-changed");
    }
    user_lib::print("[TEST] ns PASS\n");
    user_lib::exit(0);
}
