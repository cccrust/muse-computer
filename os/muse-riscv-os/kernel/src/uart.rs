use crate::sync::SpinMutex;

const UART_BASE: usize = 0x1000_0000;

// 16550A offsets
const R_RBR: usize = 0; // rx (read) / thr (write)
const R_IER: usize = 1; // interrupt enable
const R_IIR: usize = 2; // interrupt ident (read)
const R_LSR: usize = 5; // line status

const IER_RX: u8 = 1;
const IER_TX: u8 = 1 << 1;

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
        core::ptr::write_volatile(reg(R_IER), ier | IER_RX);
    }
}

/// Enable TX-empty interrupt path. Before this, putchar() polls.
pub fn tx_enable() {
    TX_ON.store(true, core::sync::atomic::Ordering::SeqCst);
}

pub fn putchar(c: u8) {
    if !TX_ON.load(core::sync::atomic::Ordering::SeqCst) {
        // early boot: poll
        unsafe {
            while core::ptr::read_volatile(reg(R_LSR)) & LSR_TX_EMPTY == 0 {}
            core::ptr::write_volatile(reg(R_RBR), c);
        }
        return;
    }
    // v1.0: enqueue + arm + kick under ONE lock acquisition. The kick
    // (THR write) must be mutually exclusive with the ISR drain, or two
    // harts can both observe THR-empty and one byte gets overwritten.
    // Short critical section, never blocks: safe under SMP.
    let full = {
        let mut g = TX.lock();
        if g.n < TX_CAP {
            let w = g.w;
            g.buf[w] = c;
            g.w = (w + 1) % TX_CAP;
            g.n += 1;
            unsafe {
                let ier = core::ptr::read_volatile(reg(R_IER));
                core::ptr::write_volatile(reg(R_IER), ier | IER_TX);
                if core::ptr::read_volatile(reg(R_LSR)) & LSR_TX_EMPTY != 0
                    && g.n > 0
                {
                    let b = g.buf[g.r];
                    g.r = (g.r + 1) % TX_CAP;
                    g.n -= 1;
                    core::ptr::write_volatile(reg(R_RBR), b);
                }
            }
            false
        } else {
            true
        }
    };
    if full {
        // ring full: poll-write one byte directly (hardware always drains)
        unsafe {
            while core::ptr::read_volatile(reg(R_LSR)) & LSR_TX_EMPTY == 0 {}
            core::ptr::write_volatile(reg(R_RBR), c);
        }
    }
}

// ---- TX ring (drained by THRE ISR, v0.6) ----
const TX_CAP: usize = 512;

struct TxRing {
    buf: [u8; TX_CAP],
    r: usize,
    w: usize,
    n: usize,
}

static TX: SpinMutex<TxRing> = SpinMutex::new(TxRing {
    buf: [0; TX_CAP],
    r: 0,
    w: 0,
    n: 0,
});

static TX_ON: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
// v1.0: atomic -- two harts can enter tx_drain concurrently on first use
static TX_MARKED: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

fn tx_pop() -> Option<u8> {
    let mut g = TX.lock();
    if g.n == 0 {
        return None;
    }
    let c = g.buf[g.r];
    g.r = (g.r + 1) % TX_CAP;
    g.n -= 1;
    Some(c)
}

/// THRE ISR: move ring bytes to THR while it is empty. Disarms the THRE
/// IRQ when the ring runs dry (16550 THRE is level-ish: leaving it armed
/// with an empty ring + empty THR would trap on every return to user).
fn tx_drain() {
    // first THRE entry proves TX-empty IRQ delivery (the kick path may have
    // already moved the bytes, so don't gate the marker on moved > 0)
    if !TX_MARKED.swap(true, core::sync::atomic::Ordering::SeqCst) {
        crate::println!("[TEST] uart-tx-irq PASS");
    }
    loop {
        unsafe {
            if core::ptr::read_volatile(reg(R_LSR)) & LSR_TX_EMPTY == 0 {
                break;
            }
        }
        match tx_pop() {
            Some(b) => {
                unsafe {
                    core::ptr::write_volatile(reg(R_RBR), b);
                }
            }
            None => {
                unsafe {
                    let ier = core::ptr::read_volatile(reg(R_IER));
                    core::ptr::write_volatile(reg(R_IER), ier & !IER_TX);
                }
                break;
            }
        }
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

/// UART ISR: IIR-dispatched. RX fills the input ring (Ctrl-C kills
/// foreground, Ctrl-D = EOF); THRE drains the TX ring.
pub fn on_irq() {
    let mut woke = false;
    let mut n = 0u32;
    loop {
        let iir = unsafe { core::ptr::read_volatile(reg(R_IIR)) };
        if iir & 1 == 1 {
            break; // no interrupt pending
        }
        match (iir >> 1) & 0x7 {
            0x2 => tx_drain(), // THR empty
            0x4 | 0xC => {
                rx_drain();
                woke = true;
            }
            0x6 => {
                // receiver line status: read LSR to clear
                unsafe {
                    core::ptr::read_volatile(reg(R_LSR));
                }
            }
            _ => break,
        }
        n += 1;
        if n > 64 {
            break;
        }
    }
    // LSR fallback: an RX byte visible without IIR (shouldn't happen,
    // but keeps the old polling-drain behavior as insurance).
    unsafe {
        if core::ptr::read_volatile(reg(R_LSR)) & LSR_RX_READY != 0 {
            rx_drain();
            woke = true;
        }
    }
    if woke {
        crate::task::wake_stdin();
    }
}

/// RX drain: move hardware FIFO to the input ring.
fn rx_drain() {
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
            } else {
                // translate CR -> LF for terminal friendliness
                push(if c == b'\r' { b'\n' } else { c });
            }
        }
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
