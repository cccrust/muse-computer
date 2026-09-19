use core::arch::asm;

static mut TICKS: usize = 0;
const FREQ: u64 = 10_000_000; // qemu virt time freq ~10MHz
const TICK_US: u64 = 10_000; // 10ms

fn r_time() -> u64 {
    let t: u64;
    unsafe {
        asm!("csrr {0}, time", out(reg) t);
    }
    t
}

pub fn init() {
    unsafe {
        // enable supervisor timer interrupt
        let mut sie: usize;
        asm!("csrr {0}, sie", out(reg) sie);
        sie |= 1 << 5;
        asm!("csrw sie, {0}", in(reg) sie);
    }
    set_next();
}

pub fn set_next() {
    let nxt = r_time() + FREQ / 100;
    crate::sbi::set_timer(nxt);
}

pub fn tick() {
    unsafe {
        TICKS += 1;
    }
}

pub fn ticks() -> usize {
    unsafe { TICKS }
}

pub fn should_preempt() -> bool {
    unsafe { TICKS % 2 == 0 }
}

pub fn sleep_ticks(n: usize) {
    let target = ticks() + n;
    while ticks() < target {
        crate::task::yield_now();
    }
}
