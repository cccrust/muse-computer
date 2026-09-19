//! UART driver for QEMU virt (ns16550a @ 0x1000_0000).
//! Polling only. Print path disables M-mode interrupts briefly so a
//! timer preemption cannot interleave characters or deadlock on a lock
//! (single hart, so this gives per-line atomicity).

use core::arch::asm;
use core::fmt;
use core::ptr;

pub const UART_BASE: usize = 0x1000_0000;
const UART_THR: usize = UART_BASE; // transmitter holding (write)
const UART_RBR: usize = UART_BASE; // receiver buffer (read)
const UART_IER: usize = UART_BASE + 1; // interrupt enable
const UART_FCR: usize = UART_BASE + 2; // FIFO control
const UART_LCR: usize = UART_BASE + 3; // line control
const UART_LSR: usize = UART_BASE + 5; // line status
const LSR_DR: u8 = 1 << 0; // data ready
const LSR_THRE: u8 = 1 << 5; // transmitter holding empty

#[inline(always)]
fn reg_r(addr: usize) -> u8 {
    unsafe { ptr::read_volatile(addr as *const u8) }
}

#[inline(always)]
fn reg_w(addr: usize, v: u8) {
    unsafe { ptr::write_volatile(addr as *mut u8, v) }
}

/// Disable M-mode interrupts, return previous mstatus.
#[inline(always)]
pub fn intr_off() -> u32 {
    let old: u32;
    unsafe {
        asm!("csrrci {}, mstatus, 8", out(reg) old);
    }
    old
}

/// Restore mstatus previously saved by [`intr_off`].
#[inline(always)]
pub fn intr_restore(saved: u32) {
    unsafe {
        asm!("csrw mstatus, {}", in(reg) saved);
    }
}

pub fn uart_init() {
    // 8N1, FIFO on, no IRQs (we poll).
    reg_w(UART_IER, 0x00);
    reg_w(UART_LCR, 0x03);
    reg_w(UART_FCR, 0x07);
}

#[inline(always)]
fn lsr() -> u8 {
    reg_r(UART_LSR)
}

/// Raw blocking putc (caller must handle critical section).
pub fn putc_raw(c: u8) {
    if c == b'\n' {
        while lsr() & LSR_THRE == 0 {}
        reg_w(UART_THR, b'\r');
    }
    while lsr() & LSR_THRE == 0 {}
    reg_w(UART_THR, c);
}

/// Non-blocking getc. Returns None when no data.
pub fn getc_nb() -> Option<u8> {
    if lsr() & LSR_DR == 0 {
        None
    } else {
        Some(reg_r(UART_RBR))
    }
}

pub struct Console;

impl fmt::Write for Console {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for b in s.bytes() {
            putc_raw(b);
        }
        Ok(())
    }
}

pub fn _print(args: fmt::Arguments) {
    let saved = intr_off();
    {
        use core::fmt::Write;
        let _ = Console.write_fmt(args);
    }
    intr_restore(saved);
}

/// Raw print used inside trap/fault paths (interrupts already off).
pub fn _print_raw(args: fmt::Arguments) {
    use core::fmt::Write;
    let _ = Console.write_fmt(args);
}
