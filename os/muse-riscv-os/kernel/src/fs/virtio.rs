// virtio-blk legacy-MMIO driver (QEMU virt @ 0x10001000), polling, 1 queue.
// Only used before any user trap runs (single hart, no concurrency).

const BASE: usize = 0x1000_1000;
const MAGIC: u32 = 0x74726976; // "virt"

// legacy MMIO register offsets
const R_MAGIC: usize = 0x000;
const R_VER: usize = 0x004;
const R_DEVID: usize = 0x008;
const R_DEVFEAT: usize = 0x010;
const R_DEVFEATSEL: usize = 0x014;
const R_DRVFEAT: usize = 0x020;
const R_DRVFEATSEL: usize = 0x024;
const R_PAGESZ: usize = 0x028; // GuestPageSize (legacy): PFN unit, must set
const R_QSEL: usize = 0x030;const R_QMAX: usize = 0x034;
const R_QNUM: usize = 0x038;
const R_QALIGN: usize = 0x03c;
const R_QPFN: usize = 0x040;
const R_QNOTIFY: usize = 0x050;const R_INTSTAT: usize = 0x060;
const R_INTACK: usize = 0x064;
const R_STATUS: usize = 0x070;
const R_CONFIG: usize = 0x100; // blk: capacity u64 at +0

const ST_ACK: u32 = 1;
const ST_DRIVER: u32 = 2;
const ST_OK: u32 = 4;
const ST_FEAT_OK: u32 = 8;

const QDEPTH: usize = 8;

const D_NEXT: u16 = 1;
const D_WRITE: u16 = 2; // device writes (device-writable)

const T_IN: u32 = 0;
const T_OUT: u32 = 1;

static mut Q_BASE: usize = 0; // pa of desc+avail frame
static mut Q_USED: usize = 0; // pa of used-ring frame
static mut AVAIL_IDX: u16 = 0;
static mut LAST_USED: u16 = 0;
static mut READY: bool = false;
static mut NCAP: u64 = 0;

// single-flight request buffers (identity-mapped .bss, whole RAM mapped)
static mut HDR: [u8; 16] = [0; 16];
static mut DATA: [u8; 512] = [0; 512];
static mut STB: [u8; 1] = [0; 1];

unsafe fn r32(off: usize) -> u32 {
    core::ptr::read_volatile((BASE + off) as *const u32)
}
unsafe fn w32(off: usize, v: u32) {
    core::ptr::write_volatile((BASE + off) as *mut u32, v)
}
unsafe fn w16(pa: usize, v: u16) {
    core::ptr::write_volatile(pa as *mut u16, v)
}
unsafe fn r16(pa: usize) -> u16 {
    core::ptr::read_volatile(pa as *const u16)
}
unsafe fn w32pa(pa: usize, v: u32) {
    core::ptr::write_volatile(pa as *mut u32, v)
}
unsafe fn r32pa(pa: usize) -> u32 {
    core::ptr::read_volatile(pa as *const u32)
}
unsafe fn w64pa(pa: usize, v: u64) {
    core::ptr::write_volatile(pa as *mut u64, v)
}
#[inline(always)]
unsafe fn fence() {
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
        let magic = r32(R_MAGIC);
        let ver = r32(R_VER);
        let dtype = r32(R_DEVID);
        if magic != MAGIC || (ver != 1 && ver != 2) || dtype != 2 {
            crate::println!(
                "[VIRTIO] no blk magic (magic={:#x} ver={} type={}), skip blk test",
                magic, ver, dtype
            );
            crate::println!("[TEST] virtio-blk SKIP (no device)");
            return;
        }
        // reset + ack + driver
        w32(R_STATUS, 0);
        w32(R_STATUS, ST_ACK | ST_DRIVER);
        // negotiate no features
        w32(R_DEVFEATSEL, 0);
        let _feat = r32(R_DEVFEAT);
        w32(R_DRVFEATSEL, 0);
        w32(R_DRVFEAT, 0);
        w32(R_STATUS, r32(R_STATUS) | ST_FEAT_OK);
        if r32(R_STATUS) & ST_FEAT_OK == 0 {
            crate::println!("[VIRTIO] FEATURES_OK rejected");
            return;
        }
        // queue 0
        w32(R_QSEL, 0);
        let max = r32(R_QMAX) as usize;
        if max == 0 {
            crate::println!("[VIRTIO] queue 0 unavailable");
            return;
        }
        let q = core::cmp::min(max, QDEPTH);
        w32(R_QNUM, q as u32);
        let f0 = match crate::mem::frame::alloc_frame() {
            Some(p) => p,
            None => {
                crate::println!("[VIRTIO] oom queue");
                return;
            }
        };
        let f1 = match crate::mem::frame::alloc_frame() {
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
        w32(R_PAGESZ, 4096);
        w32(R_QALIGN, 4096);
        w32(R_QPFN, (f0 >> 12) as u32);
        fence();
        w32(R_STATUS, r32(R_STATUS) | ST_OK);
        // capacity (sectors of 512B)
        let lo = r32(R_CONFIG) as u64;
        let hi = r32(R_CONFIG + 4) as u64;
        NCAP = lo | (hi << 32);
        AVAIL_IDX = 0;
        LAST_USED = 0;
        READY = true;
        crate::println!(
            "[VIRTIO] blk device found @ {:#x} (ver={}, {} sectors)",
            BASE, ver, NCAP
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
    w32(R_QNOTIFY, 0);
    // poll used idx
    let u = used_pa();
    let mut spins = 0u32;
    loop {
        fence();
        let idx = r16(u + 2);
        if idx != LAST_USED {
            break;
        }
        spins += 1;
        if spins > 20_000_000 {
            return false;
        }
    }
    fence();
    let slot = (LAST_USED as usize) % QDEPTH;
    let id = r32pa(u + 4 + slot * 8);
    LAST_USED = LAST_USED.wrapping_add(1);
    // ack interrupt
    w32(R_INTACK, r32(R_INTSTAT));
    fence();
    id == 0 && STB[0] == 0
}

/// Read one 512B sector.
pub fn read_block(lba: u32, out: &mut [u8; 512]) -> bool {
    unsafe {
        if !READY {
            return false;
        }
        if !submit(false, lba) {
            return false;
        }
        out.copy_from_slice(&DATA);
        true
    }
}

/// Write one 512B sector.
pub fn write_block(lba: u32, data: &[u8; 512]) -> bool {
    unsafe {
        if !READY {
            return false;
        }
        DATA.copy_from_slice(data);
        fence();
        submit(true, lba)
    }
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
