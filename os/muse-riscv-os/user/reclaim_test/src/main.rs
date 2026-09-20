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

// v1.2 reclaim regression test: 20 rounds of (fork child that sbrk's 1MB,
// touches it, exits; parent precisely reaps). With teardown working, free
// frames must return to ~baseline (threshold 64 absorbs table/TF churn and
// concurrent-hart noise). On the old leaking kernel this loses 5000+ frames.
#[no_mangle]
pub extern "C" fn main(_argc: usize, _argv: *const *const u8) {
    let base = user_lib::memstat();
    if base < 0 {
        user_lib::print("[TEST] reclaim FAIL (memstat)\n");
        user_lib::exit(1);
    }
    let mut ok = true;
    let mut i = 0;
    while i < 20 {
        let pid = user_lib::fork();
        if pid == 0 {
            // child: grow 1MB and touch every page so frames materialize,
            // whether sbrk maps eagerly or faults lazily.
            let r = user_lib::sbrk(1024 * 1024);
            if r < 0 {
                user_lib::exit(10);
            }
            let p = r as *mut u8;
            let mut k = 0usize;
            while k < 1024 * 1024 {
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
                if w != pid || code != 0 {
                    ok = false;
                }
                break;
            }
        } else {
            ok = false;
            break;
        }
        i += 1;
    }
    let end = user_lib::memstat();
    // frames are fungible; only the net loss matters
    let loss = base - end;
    if ok && loss < 64 {
        user_lib::print("[TEST] reclaim PASS\n");
    } else {
        user_lib::print("[TEST] reclaim FAIL\n");
    }
    user_lib::exit(0);
}
