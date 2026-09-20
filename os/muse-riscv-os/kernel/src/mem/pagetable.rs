pub const PTE_V: u64 = 1 << 0;
pub const PTE_R: u64 = 1 << 1;
pub const PTE_W: u64 = 1 << 2;
pub const PTE_X: u64 = 1 << 3;
pub const PTE_U: u64 = 1 << 4;
pub const PTE_G: u64 = 1 << 5;
pub const PTE_A: u64 = 1 << 6;
pub const PTE_D: u64 = 1 << 7;

pub const PAGE_SIZE: usize = 4096;

#[inline(always)]
pub fn vpn(va: usize, level: usize) -> usize {
    (va >> (12 + 9 * level)) & 0x1ff
}

#[inline(always)]
pub fn pte_new(pa: usize, flags: u64) -> u64 {
    ((pa >> 12) as u64) << 10 | (flags & 0x3ff)
}

#[inline(always)]
pub fn pte_pa(pte: u64) -> usize {
    (((pte >> 10) & 0xfff_ffff_ffff) as usize) << 12
}

#[inline(always)]
pub fn pte_flags(pte: u64) -> u64 {
    pte & 0xff
}

/// Map a VA range in the page table rooted at `root_pa`.
/// root_pa: physical address of root page table (4K aligned).
pub fn map(root_pa: usize, va: usize, pa: usize, len: usize, flags: u64) {
    let mut off = 0usize;
    while off < len {
        map_one(root_pa, va + off, pa + off, flags);
        off += PAGE_SIZE;
    }
}

fn table_at(pa: usize) -> *mut u64 {
    pa as *mut u64
}

pub fn map_one(root_pa: usize, va: usize, pa: usize, flags: u64) {
    let mut cur = root_pa;
    for level in [2, 1].iter().cloned() {
        let idx = vpn(va, level);
        unsafe {
            let t = table_at(cur);
            let e = *t.add(idx);
            if e & PTE_V == 0 {
                match crate::mem::frame::alloc_frame() {
                    Some(npa) => {
                        *t.add(idx) = pte_new(npa, PTE_V);
                        cur = npa;
                    }
                    None => panic!("out of frames in map"),
                }
            } else {
                cur = pte_pa(e);
            }
        }
    }
    let idx = vpn(va, 0);
    unsafe {
        let t = table_at(cur);
        *t.add(idx) = pte_new(pa, flags | PTE_V | PTE_A | PTE_D);
    }
}

/// Translate VA -> PA using root table. Returns None if unmapped.
pub fn translate(root_pa: usize, va: usize) -> Option<usize> {
    let mut cur = root_pa;
    for level in [2, 1, 0].iter().cloned() {
        let idx = vpn(va, level);
        unsafe {
            let t = table_at(cur) as *const u64;
            let e = *t.add(idx);
            if e & PTE_V == 0 {
                return None;
            }
            if e & (PTE_R | PTE_W | PTE_X) != 0 {
                // leaf
                let base = pte_pa(e);
                return Some(base + (va & 0xfff));
            } else {
                cur = pte_pa(e);
            }
        }
    }
    None
}

/// Clone all user (U-flag) leaves from src root into a fresh root.
/// Returns new root pa. Kernel (non-U) mappings are re-created by caller.
pub fn clone_user(src_root: usize, dst_root: usize) {
    for v2 in 0..512 {
        let e2 = unsafe { *(table_at(src_root) as *const u64).add(v2) };
        if e2 & PTE_V == 0 {
            continue;
        }
        // if leaf at level2 (1G page) - not used; skip unless U
        if e2 & (PTE_R | PTE_W | PTE_X) != 0 {
            if e2 & PTE_U != 0 {
                // copy 1G? not used in our OS; ignore
            }
            continue;
        }
        let l1 = pte_pa(e2);
        // ensure dst l1
        let dst_l1 = ensure_next(dst_root, v2);
        for v1 in 0..512 {
            let e1 = unsafe { *(table_at(l1) as *const u64).add(v1) };
            if e1 & PTE_V == 0 {
                continue;
            }
            if e1 & (PTE_R | PTE_W | PTE_X) != 0 {
                continue; // no 2M leaves
            }
            let l0 = pte_pa(e1);
            let dst_l0 = ensure_next(dst_l1, v1);
            for v0 in 0..512 {
                let e0 = unsafe { *(table_at(l0) as *const u64).add(v0) };
                if e0 & PTE_V == 0 {
                    continue;
                }
                if e0 & PTE_U == 0 {
                    continue;
                }
                // copy page
                if let Some(npa) = crate::mem::frame::alloc_frame() {
                    unsafe {
                        core::ptr::copy_nonoverlapping(
                            pte_pa(e0) as *const u8,
                            npa as *mut u8,
                            PAGE_SIZE,
                        );
                        let flags = pte_flags(e0);
                        *(table_at(dst_l0).add(v0)) = pte_new(npa, flags);
                    }
                }
            }
        }
    }
}

fn ensure_next(root: usize, idx: usize) -> usize {
    unsafe {
        let t = table_at(root);
        let e = *t.add(idx);
        if e & PTE_V == 0 {
            let npa = crate::mem::frame::alloc_frame().expect("oom ensure");
            *t.add(idx) = pte_new(npa, PTE_V);
            npa
        } else {
            pte_pa(e)
        }
    }
}

/// Unmap one user page. Only clears U leaves (never kernel mappings).
/// Returns the freed PA (caller decides whether to dealloc). sfence included.
pub fn unmap_page(root_pa: usize, va: usize) -> Option<usize> {    let mut cur = root_pa;
    for level in [2, 1].iter().cloned() {
        let idx = vpn(va, level);
        unsafe {
            let t = table_at(cur);
            let e = *t.add(idx);
            if e & PTE_V == 0 {
                return None;
            }
            if e & (PTE_R | PTE_W | PTE_X) != 0 {
                return None; // unexpected mid-level leaf
            }
            cur = pte_pa(e);
        }
    }
    let idx = vpn(va, 0);
    unsafe {
        let t = table_at(cur);
        let e = *t.add(idx);
        if e & PTE_V == 0 || e & PTE_U == 0 {
            return None;
        }
        if e & (PTE_R | PTE_W | PTE_X) == 0 {
            return None; // table pointer where leaf expected
        }
        *t.add(idx) = 0;
        core::arch::asm!("sfence.vma");
        Some(pte_pa(e))
    }
}

/// OR permission flags into every mapped U leaf in [va, va+len).
/// Used for BSS tails that share pages with RX text/rodata: the first
/// mapper's flags win in map_one/alloc_map_user, so a zero-filesz RW
/// segment (or .bss tail) can end up on a non-writable page (v0.11 #1:
/// entry storing to .bss faulted with cause=15). Skips unmapped and
/// non-U entries. sfence included.
pub fn protect(root_pa: usize, va: usize, len: usize, flags: u64) {
    let start = va & !0xfff;
    let end = (va + len + 0xfff) & !0xfff;
    let mut cur = start;
    while cur < end {
        // walk to leaf
        let mut tab = root_pa;
        let mut ok = true;
        for level in [2, 1].iter().cloned() {
            let idx = vpn(cur, level);
            unsafe {
                let t = table_at(tab);
                let e = *t.add(idx);
                if e & PTE_V == 0 || e & (PTE_R | PTE_W | PTE_X) != 0 {
                    ok = false;
                    break;
                }
                tab = pte_pa(e);
            }
        }
        if ok {
            let idx = vpn(cur, 0);
            unsafe {
                let t = table_at(tab);
                let e = *t.add(idx);
                if e & PTE_V != 0
                    && e & PTE_U != 0
                    && e & (PTE_R | PTE_W | PTE_X) != 0
                {
                    *t.add(idx) = e | (flags & (PTE_R | PTE_W | PTE_X | PTE_U));
                }
            }
        }
        cur += 4096;
    }
    unsafe {
        core::arch::asm!("sfence.vma");
    }
}

pub fn activate(root_pa: usize) {
    let satp = (8usize << 60) | (root_pa >> 12);
    unsafe {
        core::arch::asm!("csrw satp, {0}", in(reg) satp);
        core::arch::asm!("sfence.vma");
    }
}

pub fn current_satp() -> usize {
    let s: usize;
    unsafe {
        core::arch::asm!("csrr {0}, satp", out(reg) s);
    }
    s
}
