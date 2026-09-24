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

// v2.6: two-living-souls sleeper for the cgroup-kill suite.
// `linger <secs>`: fork a child that sleeps <secs>, then sleep <secs>
// self. Both stay in the container cgroup (fork inherits, v2.2), so
// `ctr stop` must report killed 2 -- no timing involved: the child is
// in the group whether or not it has been scheduled yet, and the parent
// cannot exit early (it sleeps the full term). Prints nothing
// (detached-console discipline, same as sleeper).
#[no_mangle]
pub extern "C" fn main(argc: usize, argv: *const *const u8) {
    let mut secs: usize = 30;
    if argc > 1 {
        if let Some(s) = unsafe { user_lib::argv_str(argv, 1, argc) } {
            let mut v = 0usize;
            let mut nd = 0;
            for &c in s {
                if c >= b'0' && c <= b'9' {
                    v = v.saturating_mul(10).saturating_add((c - b'0') as usize);
                    nd += 1;
                } else {
                    break;
                }
            }
            if nd > 0 {
                secs = v.min(300);
            }
        }
    }
    let pid = user_lib::fork();
    if pid < 0 {
        user_lib::exit(1);
    }
    // both parent and child sleep the term (10ms ticks, sliced so a kill
    // lands fast -- same discipline as sleeper).
    let mut left = secs.saturating_mul(100);
    while left > 0 {
        let step = left.min(100);
        user_lib::sleep(step);
        left -= step;
    }
    user_lib::exit(0);
}
