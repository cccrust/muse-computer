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
// v1.0: atomics -- shared by all harts (plain static mut is a data race).
static HITS: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
static MISS: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
static DEV_RD: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
static DEV_WR: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

fn bump_hit() {
    HITS.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
}
fn bump_miss() {
    MISS.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
}
fn bump_rd() {
    DEV_RD.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
}
fn bump_wr() {
    DEV_WR.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
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
    // fast path: hit under lock (short critical section, never blocks)
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
    // miss: fetch WITHOUT holding the cache lock. fetch() runs virtio I/O,
    // which may block the task (deschedule); holding a spinlock across that
    // would wedge any other hart spinning on it (v1.0).
    let mut tmp = [0u8; 512];
    if !fetch(lba, &mut tmp) {
        return;
    }
    out.copy_from_slice(&tmp);
    // install under lock, re-checking (another hart may have installed it
    // while we fetched; last writer wins, both copies identical).
    {
        let mut c = CACHE.lock();
        if let Some(i) = lookup(&c, lba) {
            c[i].referenced = true;
            out.copy_from_slice(&c[i].data);
            return;
        }
        let i = victim(&mut c);
        c[i].data.copy_from_slice(&tmp);
        c[i].lba = lba;
        c[i].valid = true;
        c[i].referenced = true;
    }
    // readahead n+1 (best effort; needs NCAP bound -- ask virtio)
    readahead(lba + 1);
}

fn readahead(lba: u32) {
    if lba as u64 >= crate::fs::virtio::capacity_sectors() {
        return;
    }
    // v1.0: never fetch under the cache lock (fetch may spin on the
    // device; a sleeper holding CACHE would wedge other harts). Check,
    // fetch, then install with re-check -- same pattern as read().
    {
        let c = CACHE.lock();
        if lookup(&c, lba).is_some() {
            return;
        }
    }
    let mut tmp = [0u8; 512];
    if !fetch(lba, &mut tmp) {
        return;
    }
    let mut c = CACHE.lock();
    if lookup(&c, lba).is_some() {
        return;
    }
    let i = victim(&mut c);
    c[i].data.copy_from_slice(&tmp);
    c[i].lba = lba;
    c[i].valid = true;
    c[i].referenced = false; // prefetched, not yet used
}

pub fn write(lba: u32, data: &[u8; 512]) {
    // write-through: device first WITHOUT holding the cache lock (virtio
    // I/O may block the task; v1.0 never holds a spinlock across that),
    // then a short locked cache update.
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
    crate::println!(
        "[FS] cache stats hits={} miss={} dev_rd={} dev_wr={}",
        HITS.load(core::sync::atomic::Ordering::Relaxed),
        MISS.load(core::sync::atomic::Ordering::Relaxed),
        DEV_RD.load(core::sync::atomic::Ordering::Relaxed),
        DEV_WR.load(core::sync::atomic::Ordering::Relaxed),
    );
}
