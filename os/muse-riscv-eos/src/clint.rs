//! CLINT timer (QEMU virt): mtime/mtimecmp, 100 Hz tick.

use crate::csr::{set_mstatus_mie, w_mie};
use crate::task::NEXT_TIMECMP;

pub(crate) const CLINT_MTIME: usize = 0x0200_BFF8;
pub(crate) const CLINT_MTIMECMP0: usize = 0x0200_4000;
pub(crate) const CLINT_MSIP0: usize = 0x0200_0000;
pub(crate) const TIMEBASE_HZ: u64 = 10_000_000; // QEMU virt
pub(crate) const TICK_HZ: u64 = 100; // 10ms tick
pub(crate) const TICK_INTERVAL: u64 = TIMEBASE_HZ / TICK_HZ;

pub(crate) fn mtime_get() -> u64 {
    unsafe {
        let lo = CLINT_MTIME as *const u32;
        let hi = (CLINT_MTIME + 4) as *const u32;
        loop {
            let h1 = core::ptr::read_volatile(hi);
            let l = core::ptr::read_volatile(lo);
            let h2 = core::ptr::read_volatile(hi);
            if h1 == h2 {
                return ((h2 as u64) << 32) | (l as u64);
            }
        }
    }
}

pub(crate) fn mtimecmp_set(next: u64) {
    unsafe {
        let base = CLINT_MTIMECMP0 as *mut u32;
        // Avoid spurious interrupt: max hi first, then lo, then real hi.
        core::ptr::write_volatile(base.add(1), 0xFFFF_FFFF);
        core::ptr::write_volatile(base, next as u32);
        core::ptr::write_volatile(base.add(1), (next >> 32) as u32);
    }
}

pub(crate) fn timer_init() {
    unsafe {
        NEXT_TIMECMP = mtime_get().wrapping_add(TICK_INTERVAL);
        mtimecmp_set(NEXT_TIMECMP);
    }
    // MSIE | MTIE | MEIE
    w_mie((1 << 3) | (1 << 7) | (1 << 11));
    set_mstatus_mie();
}
