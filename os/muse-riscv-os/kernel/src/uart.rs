use crate::sync::SpinMutex;

const UART_BASE: usize = 0x1000_0000;

// 16550A offsets
const R_RBR: usize = 0; // rx (read) / thr (write)
const R_IER: usize = 1; // interrupt enable
const R_LSR: usize = 5; // line status

const LSR_RX_READY: u8 = 1;
const LSR_TX_EMPTY: u8 = 1 << 5;

#[inline(always)]
fn reg(offset: usize) -> *mut u8 {
    (UART_BASE + offset) as *mut u8
}

pub fn init() {}

/// Enable RX interrupt. Call only after stvec + PLIC are up.
pub fn irq_enable() {
    unsafe {
        let ier = core::ptr::read_volatile(reg(R_IER));
        core::ptr::write_volatile(reg(R_IER), ier | 1);
    }
}

pub fn putchar(c: u8) {
    unsafe {
        // THR offset 0; LSR bit5 = THR empty
        while core::ptr::read_volatile(reg(R_LSR)) & LSR_TX_EMPTY == 0 {}
        core::ptr::write_volatile(reg(R_RBR), c);
    }
}

// ---- input ring (filled by ISR, drained by getchar) ----
const RING_CAP: usize = 256;

struct Ring {
    buf: [u8; RING_CAP],
    r: usize,
    w: usize,
    n: usize,
    eof: bool,
}

static RING: SpinMutex<Ring> = SpinMutex::new(Ring {
    buf: [0; RING_CAP],
    r: 0,
    w: 0,
    n: 0,
    eof: false,
});

fn push(c: u8) {
    let mut g = RING.lock();
    if g.n < RING_CAP {
        let w = g.w;
        g.buf[w] = c;
        g.w = (w + 1) % RING_CAP;
        g.n += 1;
    }
    // else drop (overrun)
}

pub fn pop() -> Option<u8> {
    let mut g = RING.lock();
    if g.n == 0 {
        return None;
    }
    let c = g.buf[g.r];
    g.r = (g.r + 1) % RING_CAP;
    g.n -= 1;
    Some(c)
}

pub fn take_eof() -> bool {
    let mut g = RING.lock();
    let v = g.eof;
    g.eof = false;
    v
}

/// UART ISR: drain hardware FIFO. Ctrl-C kills foreground, Ctrl-D = EOF.
pub fn on_irq() {
    let mut woke = false;
    loop {
        unsafe {
            if core::ptr::read_volatile(reg(R_LSR)) & LSR_RX_READY == 0 {
                break;
            }
            let c = core::ptr::read_volatile(reg(R_RBR));
            if c == 0x03 {
                // Ctrl-C
                if crate::task::kill_fg() {
                    crate::println!("[TTY] Ctrl-C -> kill fg");
                }
            } else if c == 0x04 {
                // Ctrl-D
                RING.lock().eof = true;
                woke = true;
            } else {
                // translate CR -> LF for terminal friendliness
                push(if c == b'\r' { b'\n' } else { c });
                woke = true;
            }
        }
    }
    if woke {
        crate::task::wake_stdin();
    }
}

pub fn getchar() -> Option<u8> {
    // ring first (ISR-filled), then direct poll (early boot / fallback)
    if let Some(c) = pop() {
        return Some(c);
    }
    unsafe {
        if core::ptr::read_volatile(reg(R_LSR)) & LSR_RX_READY == 0 {
            None
        } else {
            Some(core::ptr::read_volatile(reg(R_RBR)))
        }
    }
}
