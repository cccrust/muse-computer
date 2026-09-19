//! muse-riscv-eos: RV32IMAC bare-metal M-mode embedded OS.
//!
//! Crate root: module wiring, `print!`/`println!`, `rust_main` entry,
//! panic handler. Subsystems live in their own modules:
//! `csr`, `clint`, `memory`, `uart`, `task`, `syscall`, `trap`,
//! `shell`, `worker`.

#![no_std]
#![no_main]
#![allow(static_mut_refs)]

mod clint;
mod csr;
mod memory;
mod shell;
mod syscall;
mod task;
mod trap;
mod uart;
mod worker;

use core::arch::asm;
use core::ptr::addr_of;

use clint::{TICK_HZ, TICK_INTERVAL, timer_init};
use csr::{r_mhartid, w_mscratch, w_mtvec};
use memory::{_ebss, _edata, _erodata, _etext, _sbss, _sdata, _srodata, _stext};
use shell::shell_task;
use task::{CURRENT, TASKS, task_init};
use trap::{enter_first_task, trap_entry};
use uart::uart_init;
use worker::{worker_blink, worker_fib};

// ---------------------------------------------------------------- print macros

#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => {
        $crate::uart::_print(core::format_args!($($arg)*))
    };
}

#[macro_export]
macro_rules! println {
    () => {
        $crate::uart::_print(core::format_args!("\n"))
    };
    ($($arg:tt)*) => {
        $crate::uart::_print(core::format_args!("{}\n", core::format_args!($($arg)*)))
    };
}

// ---------------------------------------------------------------- entry

#[no_mangle]
pub extern "C" fn rust_main() -> ! {
    uart_init();
    println!("==============================================");
    println!(" embed-os 0.1.0 — RV32IMAC bare-metal, M-mode");
    println!("==============================================");
    let hart = r_mhartid();
    println!("[boot] hart={} (expect 0 on qemu virt)", hart);
    {
        println!(
            "[boot] mem: text {:#x}..{:#x} ro {:#x}..{:#x} data {:#x}..{:#x} bss {:#x}..{:#x}",
            addr_of!(_stext) as u32,
            addr_of!(_etext) as u32,
            addr_of!(_srodata) as u32,
            addr_of!(_erodata) as u32,
            addr_of!(_sdata) as u32,
            addr_of!(_edata) as u32,
            addr_of!(_sbss) as u32,
            addr_of!(_ebss) as u32,
        );
    }

    task_init(0, shell_task, "shell");
    task_init(1, worker_blink, "blink");
    task_init(2, worker_fib, "fib");

    unsafe {
        CURRENT = 0;
        TASKS[0].runs = 1;
        let first = core::ptr::addr_of!(TASKS[0].frame);
        w_mtvec(trap_entry as *const () as usize as u32);
        w_mscratch(first as u32);
        timer_init();
        println!(
            "[boot] mtvec={:#x} mscratch={:#x} interval={}",
            trap_entry as *const () as usize as u32,
            first as u32,
            TICK_INTERVAL
        );
        println!("[boot] scheduler start: 3 tasks @ {} Hz", TICK_HZ);
        enter_first_task(first);
    }
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    // interrupts may be in any state; raw print is safe here
    unsafe {
        crate::uart::_print_raw(format_args!("\n[PANIC] {}\n[PANIC] halting hart\n", info));
        loop {
            asm!("wfi");
        }
    }
}
