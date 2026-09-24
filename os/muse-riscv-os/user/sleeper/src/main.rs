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

// v2.4: quiet long-runner for the `ctr run -d/ps/stop/rm` suite.
// `sleeper <secs>` sleeps (blocking sleep syscall, timer-woken) then
// exits 0. Prints nothing: a detached container sharing the console must
// not interleave output with the shell (and the suite asserts on ctr's
// own markers, not on ours).
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
                // cap: a typo must not park a container for hours.
                secs = v.min(300);
            }
        }
    }
    // sleep(1) ~= 10ms (v1.8: SYS_TIME ticks are 10ms units; the sleep
    // syscall takes the same tick units -- see sh's `sleep(5)` ~= 50ms).
    // secs*100 ticks; yield the remainder in slices so a kill lands fast.
    let mut left = secs.saturating_mul(100);
    while left > 0 {
        let step = left.min(100);
        user_lib::sleep(step);
        // killed tasks exit at the next trap entry, so a stop() during
        // the sleep still terminates us promptly -- we never observe it.
        left -= step;
    }
    user_lib::exit(0);
}
