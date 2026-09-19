// PLIC for QEMU virt (hart0 S-mode = context 1).
// UART0 -> IRQ 10, virtio-blk @0x10001000 -> IRQ 1 (v0.6: completion interrupt).

const BASE: usize = 0x0c00_0000;
const ENABLE_C1: usize = BASE + 0x2080;
const THRESH_C1: usize = BASE + 0x201000;
const CLAIM_C1: usize = BASE + 0x201004;

pub const UART_IRQ: u32 = 10;
pub const VIRTIO_IRQ: u32 = 1;

fn prio_reg(irq: u32) -> *mut u32 {
    (BASE + 4 * irq as usize) as *mut u32
}

fn enable(irq: u32) {
    unsafe {
        core::ptr::write_volatile(prio_reg(irq), 1);
        let e = core::ptr::read_volatile(ENABLE_C1 as *const u32);
        core::ptr::write_volatile(ENABLE_C1 as *mut u32, e | (1 << irq));
    }
}

pub fn init() {
    unsafe {
        // threshold 0 (accept all)
        core::ptr::write_volatile(THRESH_C1 as *mut u32, 0);
    }
    enable(UART_IRQ);
    enable(VIRTIO_IRQ);
    crate::println!(
        "[PLIC] uart irq{} + virtio irq{} enabled",
        UART_IRQ, VIRTIO_IRQ
    );
}

/// Claim highest-priority pending interrupt (0 = none).
pub fn claim() -> u32 {
    unsafe { core::ptr::read_volatile(CLAIM_C1 as *const u32) }
}

pub fn complete(irq: u32) {
    unsafe {
        core::ptr::write_volatile(CLAIM_C1 as *mut u32, irq);
    }
}
