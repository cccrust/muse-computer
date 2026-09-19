const UART_BASE: usize = 0x1000_0000;

#[inline(always)]
fn reg(offset: usize) -> *mut u8 {
    (UART_BASE + offset) as *mut u8
}

pub fn init() {}

pub fn putchar(c: u8) {
    unsafe {
        // THR offset 0; LSR offset 5 bit5 = THR empty
        while core::ptr::read_volatile(reg(5)) & (1 << 5) == 0 {}
        core::ptr::write_volatile(reg(0), c);
    }
    // also via SBI for good measure on some QEMU configs
    // (kept UART-only to avoid double chars; SBI fallback in console)
}

pub fn getchar() -> Option<u8> {
    unsafe {
        if core::ptr::read_volatile(reg(5)) & 1 == 0 {
            None
        } else {
            Some(core::ptr::read_volatile(reg(0)))
        }
    }
}
