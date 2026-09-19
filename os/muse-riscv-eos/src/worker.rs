//! Demo worker tasks: periodic printer (`blink`) and deterministic
//! CPU burner (`fib`). `fib24` is always 46368 (F24) — a canary proving
//! preemptive context switches preserve registers.

use crate::syscall::{sys_ticks, sys_yield};

pub(crate) extern "C" fn worker_blink() -> ! {
    let mut count: u32 = 0;
    let mut last = sys_ticks();
    loop {
        let now = sys_ticks();
        if now.wrapping_sub(last) >= 200 {
            last = now;
            count = count.wrapping_add(1);
            crate::println!("[blink] count={} tick={}", count, now);
        }
        // light cooperative hint so shell stays snappy; preemption also works
        if now.wrapping_sub(last) < 200 {
            sys_yield();
        }
    }
}

pub(crate) extern "C" fn worker_fib() -> ! {
    let mut n: u32 = 0;
    let mut last = sys_ticks();
    loop {
        // fib(24) burns some CPU deterministically
        let (mut a, mut b) = (0u32, 1u32);
        for _ in 0..24 {
            let c = a.wrapping_add(b);
            a = b;
            b = c;
        }
        n = n.wrapping_add(1);
        let now = sys_ticks();
        if now.wrapping_sub(last) >= 300 {
            last = now;
            crate::println!("[fib] iter={} fib24={} tick={}", n, a, now);
        } else {
            sys_yield();
        }
    }
}
