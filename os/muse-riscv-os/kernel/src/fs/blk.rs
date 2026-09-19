// 512B block cache over virtio-blk (v0.10: 64 slots, second-chance,
// 1-block readahead, stats; writes stay write-through -- see _doc/v0.10.md).
// Single hart: no concurrency beyond the kernel lock.

use crate::sync::SpinMutex;

const NSLOT: usize = 64;

#[derive(Clone, Copy)]
struct Slot {
    lba: u32,
    valid: bool,
    referenced: bool,
    data: [u8; 512],
}

const EMPTY: Slot = Slot {
    lba: 0,
    valid: false,
    referenced: false,
    data: [0; 512],
};

static CACHE: SpinMutex<[Slot; NSLOT]> = SpinMutex::new([EMPTY; NSLOT]);
static HAND: SpinMutex<usize> = SpinMutex::new(0);

// stats (saturating; informational only)
static mut HITS: u64 = 0;
static mut MISS: u64 = 0;
static mut DEV_RD: u64 = 0;
static mut DEV_WR: u64 = 0;

fn bump_hit() {
    unsafe {
        HITS = HITS.saturating_add(1);
    }
}
fn bump_miss() {
    unsafe {
        MISS = MISS.saturating_add(1);
    }
}
fn bump_rd() {
    unsafe {
        DEV_RD = DEV_RD.saturating_add(1);
    }
}
fn bump_wr() {
    unsafe {
        DEV_WR = DEV_WR.saturating_add(1);
    }
}

fn lookup(c: &[Slot; NSLOT], lba: u32) -> Option<usize> {
    for (i, s) in c.iter().enumerate() {
        if s.valid && s.lba == lba {
            return Some(i);
        }
    }
    None
}

// second-chance victim selection. Caller holds CACHE lock; takes HAND lock
// briefly (consistent order: CACHE -> HAND, same as read path).
fn victim(c: &mut [Slot; NSLOT]) -> usize {
    let mut h = HAND.lock();
    loop {
        let i = *h % NSLOT;
        *h = h.wrapping_add(1);
        if !c[i].valid {
            return i;
        }
        if c[i].referenced {
            c[i].referenced = false;
        } else {
            return i;
        }
    }
}

fn fetch(lba: u32, slot_data: &mut [u8; 512]) -> bool {
    bump_rd();
    crate::fs::virtio::read_block(lba, slot_data)
}

pub fn read(lba: u32, out: &mut [u8; 512]) {
    // fast path: hit
    {
        let mut c = CACHE.lock();
        if let Some(i) = lookup(&c, lba) {
            c[i].referenced = true;
            out.copy_from_slice(&c[i].data);
            bump_hit();
            return;
        }
    }
    bump_miss();
    // miss: fetch + install (victim), plus 1-block readahead
    let mut c = CACHE.lock();
    // re-check under lock (single hart: no race, but keeps logic uniform)
    if let Some(i) = lookup(&c, lba) {
        c[i].referenced = true;
        out.copy_from_slice(&c[i].data);
        bump_hit();
        return;
    }
    let i = victim(&mut c);
    if fetch(lba, &mut c[i].data) {
        c[i].lba = lba;
        c[i].valid = true;
        c[i].referenced = true;
        out.copy_from_slice(&c[i].data);
    } else {
        c[i].valid = false;
        return;
    }
    drop(c);
    // readahead n+1 (best effort; needs NCAP bound -- ask virtio)
    readahead(lba + 1);
}

fn readahead(lba: u32) {
    if lba as u64 >= crate::fs::virtio::capacity_sectors() {
        return;
    }
    let mut c = CACHE.lock();
    if lookup(&c, lba).is_some() {
        return;
    }
    let i = victim(&mut c);
    if fetch(lba, &mut c[i].data) {
        c[i].lba = lba;
        c[i].valid = true;
        c[i].referenced = false; // prefetched, not yet used
    } else {
        c[i].valid = false;
    }
}

pub fn write(lba: u32, data: &[u8; 512]) {
    // write-through: device first, then cache
    bump_wr();
    crate::fs::virtio::write_block(lba, data);
    let mut c = CACHE.lock();
    if let Some(i) = lookup(&c, lba) {
        c[i].data.copy_from_slice(data);
        c[i].referenced = true;
        return;
    }
    let i = victim(&mut c);
    c[i].lba = lba;
    c[i].valid = true;
    c[i].referenced = true;
    c[i].data.copy_from_slice(data);
}

/// v0.10: flush point for clean shutdown. No-op under write-through
/// (nothing dirty) except printing stats; kept as the hook where a
/// future write-back would drain.
pub fn sync() {
    unsafe {
        crate::println!(
            "[FS] cache stats hits={} miss={} dev_rd={} dev_wr={}",
            HITS,
            MISS,
            DEV_RD,
            DEV_WR
        );
    }
}
