#![cfg_attr(not(test), no_std, no_main)]
#![feature(naked_functions, asm_const)]
#![allow(dead_code, unused_variables)]

#[cfg(not(test))]
extern crate alloc;

#[cfg(not(test))]
pub const MAX_HART: usize = 4;

#[cfg(not(test))]
extern "C" {
    fn _start_secondary();
    static mut BOOT_DONE: u32;
}

#[cfg(not(test))]
use core::arch::{asm, global_asm};

#[cfg(not(test))]
global_asm!(include_str!("entry.S"));

#[cfg(not(test))]
mod sbi;
#[cfg(not(test))]
mod uart;
#[cfg(not(test))]
mod console;
#[cfg(not(test))]
mod sync;
#[cfg(not(test))]
mod mem;
#[cfg(not(test))]
mod trap;
#[cfg(not(test))]
mod plic;
#[cfg(not(test))]
mod timer;
#[cfg(not(test))]
mod task;
#[cfg(not(test))]
mod syscall;
#[cfg(not(test))]
mod fs;
#[cfg(not(test))]
mod net;
mod embed;

#[cfg(not(test))]
extern "C" {
    fn _stext();
    fn _etext();
    fn _srodata();
    fn _erodata();
    fn _sdata();
    fn _edata();
    fn _sbss();
    fn _ebss();
    fn _ekernel();
}

#[cfg(not(test))]
pub fn kernel_end_pa() -> usize {
    _ekernel as usize
}

#[cfg(not(test))]
#[no_mangle]
pub extern "C" fn rust_main() -> ! {
    clear_bss();
    console::init();
    println!("--- muse-riscv-os (RV64, SV39, Unix-v6) ---");
    println!("[BOOT] hart booted in S-mode, kernel @ 0x80200000");
    mem::heap::init();
    println!("[MM] heap init ok");
    mem::frame::init(kernel_end_pa());
    println!("[MM] frame allocator ok");
    mem::init_kernel_space();
    println!("[MMU] SV39 enabled (satp MODE=8, 3-level)");
    trap::init();
    println!("[TRAP] stvec set");
    timer::init();
    println!("[TIMER] enabled (10ms tick)");
    fs::init();
    task::init();
    println!("[PROC] spawn init");
    // v1.0: shared init done -- release parked APs, then HSM-start any
    // hart OpenSBI parked (already-running ones report ALREADY_AVAILABLE).
    unsafe {
        core::ptr::write_volatile(
            core::ptr::addr_of_mut!(BOOT_DONE),
            1,
        );
    }
    let me = task::hartid() % MAX_HART;
    println!("[SMP] boot hart{}", me);
    // v1.0: the boot hart counts as up too (test.sh asserts hartN up for
    // all harts; with first-hart-wins the boot hart is whichever won,
    // observed hart3 under QEMU -smp 4).
    println!("[SMP] hart{} up", me);
    // v1.0: wake APs via SBI HSM *after* all shared init (mount, allocator,
    // task table) is done; APs only enter the scheduler (never re-init).
    let me = task::hartid() % MAX_HART;
    // v1.1: remember which harts actually started (under -smp 1 there are
    // no APs; the IPI test must only wait for started harts).
    let mut started_mask = 0usize;
    for h in 0..MAX_HART {
        if h == me {
            continue;
        }
        let rc = sbi::hart_start(h, _start_secondary as usize, 0);
        if rc == 0 {
            println!("[SMP] hart{} starting", h);
            started_mask |= 1 << h;
        } else {
            println!("[SMP] hart{} start FAILED (rc={}), continuing degraded", h, rc);
        }
    }
    // v1.1: IPI delivery self-test. APs ack in the soft-irq handler
    // (trap.rs); an AP still spinning in run_on (SIE=0) holds the IPI
    // pending until its first SIE=1 context. Wait on mtime (advances
    // without ISRs, so the bound holds even with SIE=0), wfi-ing between
    // checks to yield the vCPU to the APs under MTTCG. Timeout reports
    // FAIL but boots on (degraded: no prompt wakeups).
    // HSM start is asynchronous: an AP may still be STOPPED when its IPI
    // is first sent (OpenSBI answers -3 INVALID_PARAM until STARTED), so
    // retry each hart until the send succeeds (bounded; the ids are valid
    // -- these harts just started above).
    let send_deadline = crate::timer::now().wrapping_add(crate::timer::freq() * 5);
    let mut send_mask = started_mask;
    while send_mask != 0 && crate::timer::now() < send_deadline {
        for h in 0..MAX_HART {
            if send_mask & (1 << h) != 0 {
                if sbi::send_ipi(1 << h) == 0 {
                    send_mask &= !(1 << h);
                }
            }
        }
        core::hint::spin_loop();
    }
    for h in 0..MAX_HART {
        if started_mask & (1 << h) != 0 && send_mask & (1 << h) != 0 {
            println!("[DBG] ipi send hart{} failed, continuing", h);
        }
    }
    let deadline = crate::timer::now().wrapping_add(crate::timer::freq() * 15);
    loop {
        let mut done = true;
        for h in 0..MAX_HART {
            if started_mask & (1 << h) != 0 && !trap::soft_acked(h) {
                done = false;
                break;
            }
        }
        if done {
            break;
        }
        if crate::timer::now() >= deadline {
            break;
        }
        // wfi with SIE=0: no trap is taken, but the vCPU sleeps until an
        // interrupt pends (own timer is armed) -- APs get CPU time.
        unsafe {
            core::arch::asm!("wfi");
        }
    }
    let mut missing = false;
    for h in 0..MAX_HART {
        if started_mask & (1 << h) != 0 && !trap::soft_acked(h) {
            missing = true;
        }
    }
    if missing {
        println!("[TEST] ipi FAIL (ack timeout)");
    } else {
        println!("[TEST] ipi PASS");
    }
    println!("[TEST] boot markers ready");
    // v1.3: zero the contention verdict counters here: everything before
    // this point is boot-time run_on spinning (all misses, no signal).
    // What halt prints later reflects post-boot operation only.
    crate::task::contention_reset();
    task::run();
    unreachable!();
}

/// v1.0: AP entry (from _start_secondary, tp/a0 = hartid). MM, FS, tasks
/// are already up (boot hart did them); set up this hart's trap/timer
/// and join the scheduler.
#[cfg(not(test))]
#[no_mangle]
pub extern "C" fn rust_secondary_main(hart: usize) -> ! {
    let h = hart % MAX_HART;
    trap::init_on(h);
    timer::init_on();
    println!("[SMP] hart{} up", h);
    task::run_on(h);
    unreachable!();
}

#[cfg(not(test))]
fn clear_bss() {
    unsafe {
        let s = _sbss as usize as *mut u8;
        let e = _ebss as usize as *mut u8;
        let mut p = s;
        while p < e {
            core::ptr::write_volatile(p, 0);
            p = p.add(1);
        }
    }
}

#[cfg(not(test))]
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    println!("[PANIC] {}", info);
    sbi::shutdown();
    loop {
        unsafe { asm!("wfi") };
    }
}

#[cfg(test)]
fn main() {}
