// 512B block cache over virtio-blk (write-through, 8 slots, clock eviction).
// Single hart + polling: no concurrency beyond the kernel lock.

use crate::sync::SpinMutex;

const NSLOT: usize = 8;

#[derive(Clone, Copy)]
struct Slot {
    lba: u32,
    valid: bool,
    data: [u8; 512],
}

const EMPTY: Slot = Slot {
    lba: 0,
    valid: false,
    data: [0; 512],
};

static CACHE: SpinMutex<[Slot; NSLOT]> = SpinMutex::new([EMPTY; NSLOT]);
static HAND: SpinMutex<usize> = SpinMutex::new(0);

pub fn read(lba: u32, out: &mut [u8; 512]) {
    let mut c = CACHE.lock();
    for s in c.iter() {
        if s.valid && s.lba == lba {
            out.copy_from_slice(&s.data);
            return;
        }
    }
    // miss: evict clock slot (write-through => no flush needed)
    let mut h = HAND.lock();
    let i = *h % NSLOT;
    *h += 1;
    let slot = &mut c[i];
    if crate::fs::virtio::read_block(lba, &mut slot.data) {
        slot.lba = lba;
        slot.valid = true;
        out.copy_from_slice(&slot.data);
    } else {
        slot.valid = false;
    }
}

pub fn write(lba: u32, data: &[u8; 512]) {
    // write-through: device first, then cache
    crate::fs::virtio::write_block(lba, data);
    let mut c = CACHE.lock();
    for s in c.iter_mut() {
        if s.valid && s.lba == lba {
            s.data.copy_from_slice(data);
            return;
        }
    }
    let mut h = HAND.lock();
    let i = *h % NSLOT;
    *h += 1;
    c[i].lba = lba;
    c[i].valid = true;
    c[i].data.copy_from_slice(data);
}
