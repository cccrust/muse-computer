use crate::sync::SpinMutex;

pub const PAGE_SIZE: usize = 4096;
pub const RAM_BASE: usize = 0x8000_0000;
pub const MEMORY_END: usize = 0x8800_0000;
// v2.2: max tracked frames + cgroups (frame index = (pa-RAM_BASE)/4096).
pub const MAX_FRAMES: usize = (MEMORY_END - RAM_BASE) / PAGE_SIZE; // 32768
pub const MAX_CG: usize = 256;

static ALLOC: SpinMutex<FrameAlloc> = SpinMutex::new(FrameAlloc::empty());

struct FrameAlloc {
    cur: usize,
    end: usize,
    // v1.2: recycle cache sized for all of RAM (128M = 32768 frames, 256K
    // .bss). The old 4096 cap silently DROPPED frees past 16MB recycled --
    // fine when nothing was ever freed, fatal once teardown frees whole
    // address spaces. Within physical limits dealloc can no longer drop.
    recycled: [usize; 32768],
    rlen: usize,
    // v2.2: cgroup accounting, all under this same lock (no new lock, no
    // new order -- alloc/dealloc never touch the sched lock from here).
    // owner[i] = owning cgid of frame i (u16::MAX = untracked/out-of-range).
    owner: [u16; MAX_FRAMES],
    // use[cg] = frames charged; lim[cg] = cap, 0 = unlimited. Mirrored here
    // from the task-side table (single-lock check in alloc).
    use_: [u64; MAX_CG],
    lim: [u64; MAX_CG],
}

impl FrameAlloc {
    const fn empty() -> Self {
        Self {
            cur: 0,
            end: 0,
            recycled: [0; 32768],
            rlen: 0,
            owner: [u16::MAX; MAX_FRAMES],
            use_: [0; MAX_CG],
            lim: [0; MAX_CG],
        }
    }
    fn init(&mut self, start: usize, end: usize) {
        let s = (start + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);
        self.cur = s;
        self.end = end;
        self.rlen = 0;
    }
    fn idx(pa: usize) -> Option<usize> {
        if pa < RAM_BASE || pa >= MEMORY_END || pa & (PAGE_SIZE - 1) != 0 {
            return None;
        }
        let i = (pa - RAM_BASE) / PAGE_SIZE;
        if i < MAX_FRAMES {
            Some(i)
        } else {
            None
        }
    }
    fn alloc(&mut self, cg: usize) -> Option<usize> {
        // v2.2: cap enforcement, atomic with the charge below (same lock).
        // lim 0 = unlimited; over-cap returns None (callers propagate as
        // -1/errno, never panic on the user path -- see _doc/v2.2.md).
        let cg = cg.min(MAX_CG - 1);
        if self.lim[cg] != 0 && self.use_[cg] >= self.lim[cg] {
            return None;
        }
        let pa = if self.rlen > 0 {
            self.rlen -= 1;
            self.recycled[self.rlen]
        } else if self.cur + PAGE_SIZE <= self.end {
            let p = self.cur;
            self.cur += PAGE_SIZE;
            // zero the frame
            unsafe {
                core::ptr::write_bytes(p as *mut u8, 0, PAGE_SIZE);
            }
            p
        } else {
            return None;
        };
        if let Some(i) = Self::idx(pa) {
            self.owner[i] = cg as u16;
            self.use_[cg] = self.use_[cg].saturating_add(1);
        }
        Some(pa)
    }
    fn dealloc(&mut self, ppn_addr: usize) {
        if self.rlen < 32768 {
            unsafe {
                core::ptr::write_bytes(ppn_addr as *mut u8, 0, PAGE_SIZE);
            }
            self.recycled[self.rlen] = ppn_addr;
            self.rlen += 1;
        }
        // v2.2: uncharge the recorded owner (unknown/out-of-range: skip).
        if let Some(i) = Self::idx(ppn_addr) {
            let cg = self.owner[i] as usize;
            self.owner[i] = u16::MAX;
            if cg < MAX_CG {
                self.use_[cg] = self.use_[cg].saturating_sub(1);
            }
        }
    }
}

pub fn init(kernel_end: usize) {
    ALLOC.lock().init(kernel_end, MEMORY_END);
}

/// v2.2: allocate one frame charged to cgroup `cg` (0 = root/kernel).
/// Over-cap or physical OOM returns None (never panics here; callers on
/// user-reachable paths propagate -1/errno, boot paths may expect()).
/// NOTE: replaces alloc_frame(); all call sites name their cg explicitly
/// so no hidden sched-lock is taken under ALLOC (see _doc/v2.2.md).
pub fn alloc_frame_cg(cg: usize) -> Option<usize> {
    ALLOC.lock().alloc(cg)
}

pub fn dealloc_frame(pa: usize) {
    ALLOC.lock().dealloc(pa)
}

/// v2.2: mirror a cgroup limit into the allocator (called after the
/// task-side table update; sequential locks, never nested).
pub fn set_cg_limit(cg: usize, lim: u64) {
    if cg < MAX_CG {
        ALLOC.lock().lim[cg] = lim;
    }
}

/// v2.2: frames currently charged to a cgroup (stats/debug).
pub fn cg_use(cg: usize) -> u64 {
    if cg < MAX_CG {
        ALLOC.lock().use_[cg]
    } else {
        0
    }
}

/// v3.6: frame cap of a cgroup (0 = unlimited). Backs SYS_CGSTAT.
pub fn cg_lim(cg: usize) -> u64 {
    if cg < MAX_CG {
        ALLOC.lock().lim[cg]
    } else {
        0
    }
}

pub fn frames_used() -> usize {
    let a = ALLOC.lock();
    (a.cur) as usize
}

/// v1.2: free frames = untouched high region + recycle cache.
/// Backs SYS_MEMSTAT (reclaim_test asserts post-reap recovery).
pub fn free_frames() -> usize {
    let a = ALLOC.lock();
    a.rlen + a.end.saturating_sub(a.cur) / PAGE_SIZE
}
