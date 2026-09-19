#![cfg_attr(not(test), no_std, no_main)]
#![feature(naked_functions, asm_const)]
#![allow(dead_code, unused_variables)]

#[cfg(not(test))]
extern crate alloc;

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
    println!("[TEST] boot markers ready");
    task::run();
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
