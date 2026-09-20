// PLIC for QEMU virt (S-mode contexts: hart h -> context 2h+1).
// UART0 -> IRQ 10, virtio-blk @0x10001000 -> IRQ 1.

const BASE: usize = 0x0c00_0000;

pub const UART_IRQ: u32 = 10;
pub const VIRTIO_IRQ: u32 = 1;

fn prio_reg(irq: u32) -> *mut u32 {
    (BASE + 4 * irq as usize) as *mut u32
}

/// MMIO bases for an S-mode claim context (threshold + claim/complete).
fn ctx_regs(ctx: u32) -> (usize, usize) {
    let enable = BASE + 0x2000 + 0x80 * ctx as usize;
    let context = BASE + 0x200000 + 0x1000 * ctx as usize;
    (enable, context)
}

fn enable_irq_ctx(irq: u32, hart: usize) {
    // v1.0: enable on the given hart's S context (idempotent, priority
    // write is chip-global per IRQ)
    let ctx = (2 * hart + 1) as u32;
    unsafe {
        core::ptr::write_volatile(prio_reg(irq), 1);
        let (enable_base, thresh_base) = ctx_regs(ctx);
        let e = core::ptr::read_volatile(enable_base as *const u32);
        core::ptr::write_volatile(enable_base as *mut u32, e | (1 << irq));
        // threshold 0 (accept all)
        core::ptr::write_volatile(thresh_base as *mut u32, 0);
    }
}

fn enable(irq: u32) {
    enable_irq_ctx(irq, 0);
}

/// v1.0: enable UART+VIRTIO on an AP's S context.
pub fn enable_ctx(hart: usize) {
    enable_irq_ctx(UART_IRQ, hart);
    enable_irq_ctx(VIRTIO_IRQ, hart);
    crate::println!(
        "[PLIC] hart{} ctx irq{}+{} enabled",
        hart,
        UART_IRQ,
        VIRTIO_IRQ
    );
}

pub fn init() {
    enable(UART_IRQ);
    enable(VIRTIO_IRQ);
    crate::println!(
        "[PLIC] uart irq{} + virtio irq{} enabled",
        UART_IRQ, VIRTIO_IRQ
    );
}

/// Claim highest-priority pending interrupt on this hart's S context
/// (0 = none).
pub fn claim() -> u32 {
    claim_ctx(crate::task::hartid())
}

fn claim_ctx(hart: usize) -> u32 {
    let (_, context_base) = ctx_regs((2 * hart + 1) as u32);
    unsafe { core::ptr::read_volatile((context_base + 4) as *const u32) }
}

pub fn complete(irq: u32) {
    let (_, context_base) = ctx_regs((2 * crate::task::hartid() + 1) as u32);
    unsafe {
        core::ptr::write_volatile((context_base + 4) as *mut u32, irq);
    }
}
