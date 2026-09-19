//! System calls: M-mode `ecall` demo ABI (number in a7).
//!
//! | a7 | name   | args / return                          |
//! |----|--------|----------------------------------------|
//! | 0  | yield  | —                                      |
//! | 1  | putc   | a0: byte to print                      |
//! | 2  | getc   | returns byte, or 0xFFFFFFFF if none    |
//! | 3  | ticks  | returns u64 tick in a0 (lo) / a1 (hi)  |
//! | 4  | taskid | returns current task id in a0          |

use core::arch::asm;

pub(crate) const SYS_YIELD: u32 = 0;
pub(crate) const SYS_PUTC: u32 = 1;
pub(crate) const SYS_GETC: u32 = 2;
pub(crate) const SYS_TICKS: u32 = 3;
pub(crate) const SYS_TASKID: u32 = 4;

pub(crate) fn sys_yield() {
    unsafe {
        asm!("li a7, 0", "ecall", out("a0") _, out("a1") _, out("a7") _);
    }
}

pub(crate) fn sys_taskid() -> u32 {
    let id: u32;
    unsafe {
        asm!("li a7, 4", "ecall", "mv {0}, a0", out(reg) id, out("a0") _, out("a1") _, out("a7") _);
    }
    id
}

pub(crate) fn sys_ticks() -> u64 {
    let lo: u32;
    let hi: u32;
    unsafe {
        asm!(
            "li a7, 3",
            "ecall",
            "mv {0}, a0",
            "mv {1}, a1",
            out(reg) lo,
            out(reg) hi,
            out("a0") _,
            out("a1") _,
            out("a7") _,
        );
    }
    ((hi as u64) << 32) | (lo as u64)
}
