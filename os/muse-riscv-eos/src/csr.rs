//! Machine-level CSR helpers (M-mode).

use core::arch::asm;

#[inline(always)]
pub(crate) fn r_mhartid() -> u32 {
    let x: u32;
    unsafe { asm!("csrr {}, mhartid", out(reg) x) };
    x
}

#[inline(always)]
pub(crate) fn r_mcause() -> u32 {
    let x: u32;
    unsafe { asm!("csrr {}, mcause", out(reg) x) };
    x
}

#[inline(always)]
pub(crate) fn r_mtval() -> u32 {
    let x: u32;
    unsafe { asm!("csrr {}, mtval", out(reg) x) };
    x
}

#[inline(always)]
pub(crate) fn w_mtvec(addr: u32) {
    unsafe { asm!("csrw mtvec, {}", in(reg) addr) };
}

#[inline(always)]
pub(crate) fn w_mie(v: u32) {
    unsafe { asm!("csrw mie, {}", in(reg) v) };
}

#[inline(always)]
pub(crate) fn w_mscratch(v: u32) {
    unsafe { asm!("csrw mscratch, {}", in(reg) v) };
}

#[inline(always)]
pub(crate) fn set_mstatus_mie() {
    unsafe { asm!("csrsi mstatus, 8") };
}
