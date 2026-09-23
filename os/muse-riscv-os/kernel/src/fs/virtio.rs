// virtio-blk legacy-MMIO driver. v1.3: the MMIO/register helpers below
// are shared with the net driver (kernel/src/net.rs); the blk queue state
// itself stays single-device (BLK_BASE).
const BLK_BASE: usize = 0x1000_1000;
pub(crate) const MAGIC: u32 = 0x74726976; // "virt"

// legacy MMIO register offsets (shared with net)
pub(crate) const R_MAGIC: usize = 0x000;
pub(crate) const R_VER: usize = 0x004;
pub(crate) const R_DEVID: usize = 0x008;
pub(crate) const R_DEVFEAT: usize = 0x010;
pub(crate) const R_DEVFEATSEL: usize = 0x014;
pub(crate) const R_DRVFEAT: usize = 0x020;
pub(crate) const R_DRVFEATSEL: usize = 0x024;
pub(crate) const R_PAGESZ: usize = 0x028; // GuestPageSize (legacy): PFN unit, must set
pub(crate) const R_QSEL: usize = 0x030;
pub(crate) const R_QMAX: usize = 0x034;
pub(crate) const R_QNUM: usize = 0x038;
pub(crate) const R_QALIGN: usize = 0x03c;
pub(crate) const R_QPFN: usize = 0x040;
pub(crate) const R_QNOTIFY: usize = 0x050;
pub(crate) const R_INTSTAT: usize = 0x060;
pub(crate) const R_INTACK: usize = 0x064;
pub(crate) const R_STATUS: usize = 0x070;
pub(crate) const R_CONFIG: usize = 0x100; // blk: capacity u64 at +0
pub(crate) const R_CFG_MAC: usize = 0x100; // net: MAC bytes 0..6 at +0

pub(crate) const ST_ACK: u32 = 1;
pub(crate) const ST_DRIVER: u32 = 2;
pub(crate) const ST_OK: u32 = 4;
pub(crate) const ST_FEAT_OK: u32 = 8;

pub(crate) const QDEPTH: usize = 8;

pub(crate) const D_NEXT: u16 = 1;
pub(crate) const D_WRITE: u16 = 2; // device writes (device-writable)

const T_IN: u32 = 0;
const T_OUT: u32 = 1;

static mut Q_BASE: usize = 0; // pa of desc+avail frame
static mut Q_USED: usize = 0; // pa of used-ring frame
static mut AVAIL_IDX: u16 = 0;
static mut LAST_USED: u16 = 0;
static mut READY: bool = false;
static mut NCAP: u64 = 0;
// v0.6: completion-interrupt stats (v1.0: atomic, ISR races syscalls
// across harts; saturating to avoid any failure path)
static IRQ_COUNT: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

// v1.0: single-flight driver lock. The queue + HDR/DATA/STB buffers are
// shared by all harts; concurrent submit() calls would mix up requests
// and corrupt DATA (observed: "exec sh failed" under -smp 4).
// Pure spin (never block_current here): the holder always makes progress
// independently on its own hart, so a spinner holds nothing and cannot
// deadlock. Blocking while spinning would be worse than useless -- the
// task is still current[] on its hart, so a wake would queue it twice
// (see wake_locked) and two harts would run one task.
static DRV_BUSY: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

fn drv_lock() {
    use core::sync::atomic::Ordering::SeqCst;
    while DRV_BUSY
        .compare_exchange(false, true, SeqCst, SeqCst)
        .is_err()
    {
        core::hint::spin_loop();
    }
}

fn drv_unlock() {
    use core::sync::atomic::Ordering::SeqCst;
    DRV_BUSY.store(false, SeqCst);
}

// single-flight request buffers (identity-mapped .bss, whole RAM mapped)
static mut HDR: [u8; 16] = [0; 16];
static mut DATA: [u8; 512] = [0; 512];
static mut STB: [u8; 1] = [0; 1];

pub(crate) unsafe fn r32(base: usize, off: usize) -> u32 {
    core::ptr::read_volatile((base + off) as *const u32)
}
pub(crate) unsafe fn w32(base: usize, off: usize, v: u32) {
    core::ptr::write_volatile((base + off) as *mut u32, v)
}
pub(crate) unsafe fn r8(base: usize, off: usize) -> u8 {
    core::ptr::read_volatile((base + off) as *const u8)
}
pub(crate) unsafe fn w16(pa: usize, v: u16) {
    core::ptr::write_volatile(pa as *mut u16, v)
}
pub(crate) unsafe fn r16(pa: usize) -> u16 {
    core::ptr::read_volatile(pa as *const u16)
}
pub(crate) unsafe fn w32pa(pa: usize, v: u32) {
    core::ptr::write_volatile(pa as *mut u32, v)
}
pub(crate) unsafe fn r32pa(pa: usize) -> u32 {
    core::ptr::read_volatile(pa as *const u32)
}
pub(crate) unsafe fn w64pa(pa: usize, v: u64) {
    core::ptr::write_volatile(pa as *mut u64, v)
}
#[inline(always)]
pub(crate) unsafe fn fence() {
    core::arch::asm!("fence iorw, iorw");
}

fn desc_pa(i: usize) -> usize {
    unsafe { Q_BASE + i * 16 }
}
fn avail_pa() -> usize {
    unsafe { Q_BASE + QDEPTH * 16 }
}
fn used_pa() -> usize {
    unsafe { Q_USED }
}

unsafe fn desc_set(i: usize, addr: usize, len: u32, flags: u16, next: u16) {
    let d = desc_pa(i);
    w64pa(d, addr as u64);
    w32pa(d + 8, len);
    w16(d + 12, flags);
    w16(d + 14, next);
}

pub fn is_ready() -> bool {
    unsafe { READY }
}

pub fn capacity_sectors() -> u64 {
    unsafe { NCAP }
}

/// Probe + queue setup. Keeps old `[TEST] virtio-blk PASS` marker.
pub fn init() {
    unsafe {
        let magic = r32(BLK_BASE, R_MAGIC);
        let ver = r32(BLK_BASE, R_VER);
        let dtype = r32(BLK_BASE, R_DEVID);
        if magic != MAGIC || (ver != 1 && ver != 2) || dtype != 2 {
            crate::println!(
                "[VIRTIO] no blk magic (magic={:#x} ver={} type={}), skip blk test",
                magic, ver, dtype
            );
            crate::println!("[TEST] virtio-blk SKIP (no device)");
            return;
        }
        // reset + ack + driver
        w32(BLK_BASE, R_STATUS, 0);
        w32(BLK_BASE, R_STATUS, ST_ACK | ST_DRIVER);
        // negotiate no features
        w32(BLK_BASE, R_DEVFEATSEL, 0);
        let _feat = r32(BLK_BASE, R_DEVFEAT);
        w32(BLK_BASE, R_DRVFEATSEL, 0);
        w32(BLK_BASE, R_DRVFEAT, 0);
        w32(BLK_BASE, R_STATUS, r32(BLK_BASE, R_STATUS) | ST_FEAT_OK);
        if r32(BLK_BASE, R_STATUS) & ST_FEAT_OK == 0 {
            crate::println!("[VIRTIO] FEATURES_OK rejected");
            return;
        }
        // queue 0
        w32(BLK_BASE, R_QSEL, 0);
        let max = r32(BLK_BASE, R_QMAX) as usize;
        if max == 0 {
            crate::println!("[VIRTIO] queue 0 unavailable");
            return;
        }
        let q = core::cmp::min(max, QDEPTH);
        w32(BLK_BASE, R_QNUM, q as u32);
        let f0 = match crate::mem::frame::alloc_frame_cg(0) {
            Some(p) => p,
            None => {
                crate::println!("[VIRTIO] oom queue");
                return;
            }
        };
        let f1 = match crate::mem::frame::alloc_frame_cg(0) {
            Some(p) => p,
            None => {
                crate::println!("[VIRTIO] oom queue used");
                return;
            }
        };
        Q_BASE = f0;
        Q_USED = f1;
        // avail ring: flags u16 + idx u16 + ring[q] + used_event u16
        w16(avail_pa(), 0);
        w16(avail_pa() + 2, 0);
        // used ring header
        w16(used_pa(), 0);
        w16(used_pa() + 2, 0);
        fence();
        w32(BLK_BASE, R_PAGESZ, 4096);
        w32(BLK_BASE, R_QALIGN, 4096);
        w32(BLK_BASE, R_QPFN, (f0 >> 12) as u32);
        fence();
        w32(BLK_BASE, R_STATUS, r32(BLK_BASE, R_STATUS) | ST_OK);
        // capacity (sectors of 512B)
        let lo = r32(BLK_BASE, R_CONFIG) as u64;
        let hi = r32(BLK_BASE, R_CONFIG + 4) as u64;
        NCAP = lo | (hi << 32);
        AVAIL_IDX = 0;
        LAST_USED = 0;
        READY = true;
        crate::println!(
            "[VIRTIO] blk device found @ {:#x} (ver={}, {} sectors)",
            BLK_BASE, ver, NCAP
        );
        crate::println!("[TEST] virtio-blk PASS");
        // LBA round-trip self-test on last sector (save/restore)
        if NCAP > 16 {
            selftest();
        }
    }
}

unsafe fn submit(write: bool, lba: u32) -> bool {
    // header
    let h = HDR.as_mut_ptr();
    core::ptr::write_unaligned(h as *mut u32, if write { T_OUT } else { T_IN });
    core::ptr::write_unaligned(h.add(4) as *mut u32, 0);
    core::ptr::write_unaligned(h.add(8) as *mut u64, lba as u64);
    STB[0] = 0xff;
    let hdr_pa = HDR.as_ptr() as usize;
    let dat_pa = DATA.as_ptr() as usize;
    let st_pa = STB.as_ptr() as usize;
    desc_set(0, hdr_pa, 16, D_NEXT, 1);
    if write {
        desc_set(1, dat_pa, 512, D_NEXT, 2);
    } else {
        desc_set(1, dat_pa, 512, D_NEXT | D_WRITE, 2);
    }
    desc_set(2, st_pa, 1, D_WRITE, 0);
    let a = avail_pa();
    // ring[avail_idx % q] = head(0)
    w16(a + 4 + (AVAIL_IDX as usize % QDEPTH) * 2, 0);
    fence();
    AVAIL_IDX = AVAIL_IDX.wrapping_add(1);
    w16(a + 2, AVAIL_IDX);
    fence();
    w32(BLK_BASE, R_QNOTIFY, 0);
    let u = used_pa();
    // ---- completion wait: pure poll (v1.0) ----
    // NOTE: this runs inside a syscall (trap) or at boot -- kernel context
    // with SIE=0, so this hart takes no interrupts and can never deschedule
    // here. block_current() in this loop would only mark the task Blocked
    // while it is still current[] and executing; a wake from another hart
    // would then queue it a second time and two harts would run one task.
    // So: poll the used ring (the device advances it on its own) with a
    // deadline from the global tick (other harts advance it) plus a local
    // iteration bound in case all harts are spinning and ticks freeze.
    let deadline = crate::timer::ticks().wrapping_add(500);
    let mut local = 0u32;
    loop {
        fence();
        if r16(u + 2) != LAST_USED {
            break;
        }
        if crate::timer::ticks() >= deadline {
            break;
        }
        local = local.wrapping_add(1);
        if local > 100_000_000 {
            break;
        }
    }
    if r16(u + 2) == LAST_USED {
        return false;
    }
    fence();
    let slot = (LAST_USED as usize) % QDEPTH;
    let id = r32pa(u + 4 + slot * 8);
    LAST_USED = LAST_USED.wrapping_add(1);
    // ack interrupt
    w32(BLK_BASE, R_INTACK, r32(BLK_BASE, R_INTSTAT));
    fence();
    id == 0 && STB[0] == 0
}

/// v0.6: completion ISR. Call from the external trap for PLIC IRQ 1.
/// Acks a pending used-ring update (if still un-acked) and wakes blocked
/// submitters. NOTE: the fast path / tick watchdog usually consumes+acks
/// first, so this mostly observes INTSTAT==0; the delivery marker lives
/// on the claim side (trap.rs).
pub fn on_irq() {
    unsafe {
        let st = r32(BLK_BASE, R_INTSTAT);
        if st & 1 != 0 {
            // saturating: cap at u64::MAX instead of wrapping
            let _ = IRQ_COUNT.fetch_update(
                core::sync::atomic::Ordering::SeqCst,
                core::sync::atomic::Ordering::SeqCst,
                |v| v.checked_add(1),
            );
            w32(BLK_BASE, R_INTACK, st);
            fence();
        }
    }
    crate::task::wake_virtio();
}

pub fn irq_count() -> u64 {
    IRQ_COUNT.load(core::sync::atomic::Ordering::SeqCst)
}

/// Read one 512B sector. Single-flight: the queue + DATA buffer are
/// shared, so the whole submit+copy runs under the driver lock.
pub fn read_block(lba: u32, out: &mut [u8; 512]) -> bool {
    drv_lock();
    let ok = unsafe {
        if !READY {
            false
        } else if !submit(false, lba) {
            false
        } else {
            out.copy_from_slice(&DATA);
            true
        }
    };
    drv_unlock();
    ok
}

/// Write one 512B sector (single-flight, see read_block).
pub fn write_block(lba: u32, data: &[u8; 512]) -> bool {
    drv_lock();
    let ok = unsafe {
        if !READY {
            false
        } else {
            DATA.copy_from_slice(data);
            fence();
            submit(true, lba)
        }
    };
    drv_unlock();
    ok
}

unsafe fn selftest() {
    let scratch = (NCAP - 1) as u32;
    let mut orig = [0u8; 512];
    let mut back = [0u8; 512];
    if !read_block(scratch, &mut orig) {
        crate::println!("[TEST] virtio-blk RW FAIL (read)");
        return;
    }
    let mut pat = [0u8; 512];
    for i in 0..512 {
        pat[i] = (i ^ 0xa5) as u8;
    }
    if !write_block(scratch, &pat) {
        crate::println!("[TEST] virtio-blk RW FAIL (write)");
        return;
    }
    if !read_block(scratch, &mut back) || back != pat {
        crate::println!("[TEST] virtio-blk RW FAIL (verify)");
        return;
    }
    if !write_block(scratch, &orig) {
        crate::println!("[TEST] virtio-blk RW FAIL (restore)");
        return;
    }
    crate::println!("[TEST] virtio-blk RW PASS");
}
