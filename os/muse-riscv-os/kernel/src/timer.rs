use core::arch::asm;
use core::sync::atomic::{AtomicU64, Ordering};

// v1.0: shared by all harts (per-hart arming via set_next, global count).
static TICKS: AtomicU64 = AtomicU64::new(0);
const FREQ: u64 = 10_000_000; // qemu virt time freq ~10MHz
const TICK_US: u64 = 10_000; // 10ms

fn r_time() -> u64 {
    let t: u64;
    unsafe {
        asm!("csrr {0}, time", out(reg) t);
    }
    t
}

/// v1.1: raw mtime (advances without ISRs; for bounded boot waits while
/// SIE=0). ~10MHz on QEMU virt.
pub fn now() -> u64 {
    r_time()
}

pub fn freq() -> u64 {
    FREQ
}

pub fn init() {
    unsafe {
        // enable supervisor timer interrupt (per-hart CSR; call on each hart)
        let mut sie: usize;
        asm!("csrr {0}, sie", out(reg) sie);
        sie |= 1 << 5;
        asm!("csrw sie, {0}", in(reg) sie);
    }
    set_next();
}

/// v1.0: per-hart init for APs (same as init; split for clarity).
pub fn init_on() {
    init();
}

pub fn set_next() {
    let nxt = r_time() + FREQ / 100;
    crate::sbi::set_timer(nxt);
}

pub fn tick() {
    TICKS.fetch_add(1, Ordering::SeqCst);
}

pub fn ticks() -> usize {
    TICKS.load(Ordering::SeqCst) as usize
}

pub fn should_preempt() -> bool {
    // preempt on roughly half of ticks (checked per hart on its own ticks)
    ticks() % 2 == 0
}

pub fn sleep_ticks(n: usize) {
    let target = ticks() + n;
    while ticks() < target {
        crate::task::yield_now();
    }
}
