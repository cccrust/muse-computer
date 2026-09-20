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
    for h in 0..MAX_HART {
        if h == me {
            continue;
        }
        let rc = sbi::hart_start(h, _start_secondary as usize, 0);
        if rc == 0 {
            println!("[SMP] hart{} starting", h);
        } else {
            println!("[SMP] hart{} start FAILED (rc={}), continuing degraded", h, rc);
        }
    }
    println!("[TEST] boot markers ready");
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
