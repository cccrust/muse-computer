pub mod frame;
pub mod heap;
pub mod pagetable;

use pagetable as pt;

extern "C" {
    fn _stext();
    fn _etext();
    fn _srodata();
    fn _erodata();
    fn _sdata();
    fn _edata();
    fn _sbss();
    fn _ebss();
}

static mut KERNEL_ROOT: usize = 0;

pub fn kernel_root() -> usize {
    unsafe { KERNEL_ROOT }
}

fn addr(f: unsafe extern "C" fn()) -> usize {
    f as usize
}

pub fn init_kernel_space() {
    unsafe {
        let root = crate::mem::frame::alloc_frame_cg(0).expect("no frame for root");
        crate::println!("[MM] root={:#x}", root);
        // .text RX
        assert!(pt::map(
            root,
            addr(_stext),
            addr(_stext),
            addr(_etext) - addr(_stext),
            pt::PTE_R | pt::PTE_X,
            0,
        ));
        crate::println!("[MM] text mapped");
        // .rodata R
        assert!(pt::map(
            root,
            addr(_srodata),
            addr(_srodata),
            (addr(_erodata) - addr(_srodata)).max(4096),
            pt::PTE_R,
            0,
        ));
        crate::println!("[MM] rodata mapped");
        // .data RW
        assert!(pt::map(
            root,
            addr(_sdata),
            addr(_sdata),
            (addr(_edata) - addr(_sdata)).max(4096),
            pt::PTE_R | pt::PTE_W,
            0,
        ));
        crate::println!("[MM] data mapped");
        // .bss RW
        assert!(pt::map(
            root,
            addr(_sbss),
            addr(_sbss),
            (addr(_ebss) - addr(_sbss)).max(4096),
            pt::PTE_R | pt::PTE_W,
            0,
        ));
        crate::println!("[MM] bss mapped");
        // whole RAM identity RW (frames, stacks)
        // 0x80000000..0x88000000
        let mut a = 0x8000_0000usize;
        while a < 0x8800_0000 {
            assert!(pt::map_one(root, a, a, pt::PTE_R | pt::PTE_W | pt::PTE_X, 0));
            a += 4096;
        }
        crate::println!("[MM] ram mapped");
        // UART + VIRTIO MMIO
        assert!(pt::map_one(root, 0x1000_0000, 0x1000_0000, pt::PTE_R | pt::PTE_W, 0));
        assert!(pt::map_one(root, 0x1000_1000, 0x1000_1000, pt::PTE_R | pt::PTE_W, 0));
        // test MMIO range for virtio (0x10001000..0x10008000)
        let mut m = 0x1000_1000usize;
        while m < 0x1000_8000 {
            assert!(pt::map_one(root, m, m, pt::PTE_R | pt::PTE_W, 0));
            m += 4096;
        }
        // PLIC (0x0c000000..0x0c300000): priority + enable + threshold/claim
        let mut p = 0x0c00_0000usize;
        while p < 0x0c30_0000 {
            assert!(pt::map_one(root, p, p, pt::PTE_R | pt::PTE_W, 0));
            p += 4096;
        }
        crate::println!("[MM] mmio mapped, activating...");
        KERNEL_ROOT = root;
        pt::activate(root);
        crate::println!("[MM] activated");
    }
}

/// Build a fresh user-capable address space: clones kernel mappings
/// (by re-mapping same kernel ranges) and returns new root.
/// v2.2: fallible (fresh table pages charge to cg); None on OOM/cap-hit.
pub fn new_user_space(cg: usize) -> Option<usize> {
    unsafe {
        let root = crate::mem::frame::alloc_frame_cg(cg)?;
        // copy kernel mappings wholesale (non-U leaves + tables)
        if !copy_kernel_tables(KERNEL_ROOT, root, cg) {
            crate::mem::frame::dealloc_frame(root);
            return None;
        }
        Some(root)
    }
}

fn copy_kernel_tables(src: usize, dst: usize, cg: usize) -> bool {
    // copy only non-U entries (tables + leaves)
    for v2 in 0..512 {
        let e2 = unsafe { *((src as *const u64).add(v2)) };
        if e2 & pt::PTE_V == 0 {
            continue;
        }
        // kernel lives at high VA (0x8020_0000 -> vpn2 = 2) and MMIO high
        // just copy table structure for non-U
        if e2 & (pt::PTE_R | pt::PTE_W | pt::PTE_X) != 0 {
            // leaf (e.g. big?) copy directly if not U
            if e2 & pt::PTE_U == 0 {
                unsafe {
                    *((dst as *mut u64).add(v2)) = e2;
                }
            }
            continue;
        }
        let s1 = pt::pte_pa(e2);
        // check if this subtree contains any non-U leaf; if so clone table
        let d1 = match ensure_table(dst, v2, cg) {
            Some(t) => t,
            None => return false,
        };
        for v1 in 0..512 {
            let e1 = unsafe { *((s1 as *const u64).add(v1)) };
            if e1 & pt::PTE_V == 0 {
                continue;
            }
            if e1 & (pt::PTE_R | pt::PTE_W | pt::PTE_X) != 0 {
                if e1 & pt::PTE_U == 0 {
                    unsafe {
                        *((d1 as *mut u64).add(v1)) = e1;
                    }
                }
                continue;
            }
            let s0 = pt::pte_pa(e1);
            let d0 = match ensure_table(d1, v1, cg) {
                Some(t) => t,
                None => return false,
            };
            for v0 in 0..512 {
                let e0 = unsafe { *((s0 as *const u64).add(v0)) };
                if e0 & pt::PTE_V == 0 {
                    continue;
                }
                if e0 & pt::PTE_U == 0 {
                    unsafe {
                        *((d0 as *mut u64).add(v0)) = e0;
                    }
                }
            }
        }
    }
    true
}

fn ensure_table(root: usize, idx: usize, cg: usize) -> Option<usize> {
    unsafe {
        let t = root as *mut u64;
        let e = *t.add(idx);
        if e & pt::PTE_V == 0 {
            let npa = crate::mem::frame::alloc_frame_cg(cg)?;
            *t.add(idx) = pt::pte_new(npa, pt::PTE_V);
            Some(npa)
        } else {
            Some(pt::pte_pa(e))
        }
    }
}

/// Map user ELF segment + stack + trapframe helpers.
/// v2.2: fallible (false on OOM/cap-hit); caller cleans up + propagates.
pub fn map_user(root: usize, va: usize, data: &[u8], flags: u64, cg: usize) -> bool {
    let start = va & !0xfff;
    let end = (va + data.len() + 0xfff) & !0xfff;
    let mut cur = start;
    while cur < end {
        if pt::translate(root, cur).is_none() {
            let pa = match crate::mem::frame::alloc_frame_cg(cg) {
                Some(p) => p,
                None => return false,
            };
            if !pt::map_one(root, cur, pa, flags | pt::PTE_U, cg) {
                crate::mem::frame::dealloc_frame(pa);
                return false;
            }
        }
        cur += 4096;
    }
    // copy data
    for (i, b) in data.iter().enumerate() {
        let v = va + i;
        let p = pt::translate(root, v).expect("just mapped");
        unsafe {
            *(p as *mut u8) = *b;
        }
    }
    true
}

pub fn alloc_map_user(root: usize, va: usize, len: usize, flags: u64, cg: usize) -> bool {
    let start = va & !0xfff;
    let end = (va + len + 0xfff) & !0xfff;
    let mut cur = start;
    while cur < end {
        if pt::translate(root, cur).is_none() {
            let pa = match crate::mem::frame::alloc_frame_cg(cg) {
                Some(p) => p,
                None => return false,
            };
            if !pt::map_one(root, cur, pa, flags | pt::PTE_U, cg) {
                crate::mem::frame::dealloc_frame(pa);
                return false;
            }
        }
        cur += 4096;
    }
    true
}

/// v1.2: tear down a dead user address space. Walks L2/L1/L0 (mirror of
/// clone_user/copy_kernel_tables), deallocating every U leaf frame and
/// every intermediate table page, then the root itself. Non-U leaves
/// (kernel copies, TF mapping) are SKIPPED -- their frames are borrowed,
/// not owned (TF is freed separately via tf_pa).
/// The intermediate table PAGES are always owned: every valid non-leaf
/// PTE in a user root was allocated for that root (fresh root + ensure
/// tables), even tables that also hold borrowed non-U leaves.
/// Caller: waitpid reap, AFTER the slot is gone and WITHOUT the sched
/// lock; followed by pt::remote_flush_all() (freed frames are reused
/// under ASID 0 -- stale remote TLB entries would alias the next mapping).
pub fn free_user_space(root: usize) {
    unsafe {
        for v2 in 0..512 {
            let e2 = *((root as *const u64).add(v2));
            if e2 & pt::PTE_V == 0 {
                continue;
            }
            if e2 & (pt::PTE_R | pt::PTE_W | pt::PTE_X) != 0 {
                // L2 leaf (1G; we never create U ones, but be thorough)
                if e2 & pt::PTE_U != 0 {
                    crate::mem::frame::dealloc_frame(pt::pte_pa(e2));
                }
                continue;
            }
            let l1 = pt::pte_pa(e2);
            for v1 in 0..512 {
                let e1 = *((l1 as *const u64).add(v1));
                if e1 & pt::PTE_V == 0 {
                    continue;
                }
                if e1 & (pt::PTE_R | pt::PTE_W | pt::PTE_X) != 0 {
                    if e1 & pt::PTE_U != 0 {
                        crate::mem::frame::dealloc_frame(pt::pte_pa(e1));
                    }
                    continue;
                }
                let l0 = pt::pte_pa(e1);
                for v0 in 0..512 {
                    let e0 = *((l0 as *const u64).add(v0));
                    if e0 & pt::PTE_V == 0 {
                        continue;
                    }
                    // U leaf with permissions = owned page; anything else
                    // (borrowed TF/kernel leaf, table pointers) skipped
                    if e0 & pt::PTE_U != 0
                        && e0 & (pt::PTE_R | pt::PTE_W | pt::PTE_X) != 0
                    {
                        crate::mem::frame::dealloc_frame(pt::pte_pa(e0));
                    }
                }
                crate::mem::frame::dealloc_frame(l0);
            }
            crate::mem::frame::dealloc_frame(l1);
        }
        crate::mem::frame::dealloc_frame(root);
    }
}
/// v0.8: unmap user pages in [va, va+len) and recycle their frames.
/// Skips unmapped / non-U pages (partial ranges are safe).
/// v1.1: remote-flushes all harts afterwards (freed frames are reused for
/// other address spaces; with ASID 0 a stale remote TLB entry would alias
/// the next mapping at the same VA).
pub fn unmap_free_user(root: usize, va: usize, len: usize) {
    let start = va & !0xfff;
    let end = (va + len + 0xfff) & !0xfff;
    let mut cur = start;
    while cur < end {
        if let Some(pa) = pt::unmap_page(root, cur) {
            crate::mem::frame::dealloc_frame(pa);
        }
        cur += 4096;
    }
    pt::remote_flush_all();
}
