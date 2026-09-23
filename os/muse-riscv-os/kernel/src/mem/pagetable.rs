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
/// v2.2: `cg` charges fresh table pages (false on OOM instead of panic).
pub fn map(root_pa: usize, va: usize, pa: usize, len: usize, flags: u64, cg: usize) -> bool {
    let mut off = 0usize;
    while off < len {
        if !map_one(root_pa, va + off, pa + off, flags, cg) {
            return false;
        }
        off += PAGE_SIZE;
    }
    true
}

fn table_at(pa: usize) -> *mut u64 {
    pa as *mut u64
}

pub fn map_one(root_pa: usize, va: usize, pa: usize, flags: u64, cg: usize) -> bool {
    let mut cur = root_pa;
    for level in [2, 1].iter().cloned() {
        let idx = vpn(va, level);
        unsafe {
            let t = table_at(cur);
            let e = *t.add(idx);
            if e & PTE_V == 0 {
                match crate::mem::frame::alloc_frame_cg(cg) {
                    Some(npa) => {
                        *t.add(idx) = pte_new(npa, PTE_V);
                        cur = npa;
                    }
                    // v2.2: OOM/cap-hit returns false (was panic).
                    None => return false,
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
    true
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
/// Fresh frames (copies + tables) charge to `cg`. Returns false if any
/// allocation failed (v2.2: was silent partial-copy; caller cleans up).
/// Kernel (non-U) mappings are re-created by caller.
pub fn clone_user(src_root: usize, dst_root: usize, cg: usize) -> bool {
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
        let dst_l1 = match ensure_next(dst_root, v2, cg) {
            Some(t) => t,
            None => return false,
        };
        for v1 in 0..512 {
            let e1 = unsafe { *(table_at(l1) as *const u64).add(v1) };
            if e1 & PTE_V == 0 {
                continue;
            }
            if e1 & (PTE_R | PTE_W | PTE_X) != 0 {
                continue; // no 2M leaves
            }
            let l0 = pte_pa(e1);
            let dst_l0 = match ensure_next(dst_l1, v1, cg) {
                Some(t) => t,
                None => return false,
            };
            for v0 in 0..512 {
                let e0 = unsafe { *(table_at(l0) as *const u64).add(v0) };
                if e0 & PTE_V == 0 {
                    continue;
                }
                if e0 & PTE_U == 0 {
                    continue;
                }
                // copy page
                if let Some(npa) = crate::mem::frame::alloc_frame_cg(cg) {
                    unsafe {
                        core::ptr::copy_nonoverlapping(
                            pte_pa(e0) as *const u8,
                            npa as *mut u8,
                            PAGE_SIZE,
                        );
                        let flags = pte_flags(e0);
                        *(table_at(dst_l0).add(v0)) = pte_new(npa, flags);
                    }
                } else {
                    return false;
                }
            }
        }
    }
    true
}

fn ensure_next(root: usize, idx: usize, cg: usize) -> Option<usize> {
    unsafe {
        let t = table_at(root);
        let e = *t.add(idx);
        if e & PTE_V == 0 {
            // v2.2: fallible (was expect); table pages charge to cg.
            let npa = crate::mem::frame::alloc_frame_cg(cg)?;
            *t.add(idx) = pte_new(npa, PTE_V);
            Some(npa)
        } else {
            Some(pte_pa(e))
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

/// v1.1: flush this hart (local sfence) plus every other hart (SBI RFENCE,
/// full address space) after changing a LIVE user address space's
/// mappings. Required because ASIDs are all 0: a stale TLB entry on
/// another hart silently aliases the new mapping at the same VA.
/// Callers (only live roots need it; fresh roots/frames are uncached):
/// - mem::unmap_free_user (munmap / sbrk shrink: frames are recycled);
/// - task::exec root switch (old root's VAs collide with the new root's).
/// protect() callers: spawn uses a fresh root (skip); exec is covered here.
/// Call context is always trap/kernel with valid tp (hartid).
pub fn remote_flush_all() {
    unsafe {
        core::arch::asm!("sfence.vma");
    }
    let me = crate::task::hartid() % crate::MAX_HART;
    let mut mask = 0usize;
    for h in 0..crate::MAX_HART {
        if h != me {
            mask |= 1 << h;
        }
    }
    crate::sbi::remote_sfence_vma(mask, 0, 0);
}

pub fn current_satp() -> usize {
    let s: usize;
    unsafe {
        core::arch::asm!("csrr {0}, satp", out(reg) s);
    }
    s
}
