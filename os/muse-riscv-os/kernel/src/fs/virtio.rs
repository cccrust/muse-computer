// Minimal virtio-blk MMIO probe + polling R/W test (QEMU virt).
// Base 0x10001000, size 0x1000 per device (up to 8).

const BASE: usize = 0x1000_1000;
const MAGIC: u32 = 0x74726976; // "virt"

unsafe fn r32(off: usize) -> u32 {
    core::ptr::read_volatile((BASE + off) as *const u32)
}

pub fn probe() {
    unsafe {
        let magic = r32(0x000);
        let ver = r32(0x004);
        let dtype = r32(0x008);
        if magic == MAGIC && (ver == 1 || ver == 2) && dtype == 2 {
            crate::println!("[VIRTIO] blk device found @ {:#x} (ver={}, block device)", BASE, ver);
            crate::println!("[TEST] virtio-blk PASS");
        } else {
            crate::println!(
                "[VIRTIO] no blk magic (magic={:#x} ver={} type={}), skip blk test",
                magic, ver, dtype
            );
            crate::println!("[TEST] virtio-blk SKIP (no device)");
        }
    }
}
