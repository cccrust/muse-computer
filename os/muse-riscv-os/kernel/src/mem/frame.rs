use crate::sync::SpinMutex;

pub const PAGE_SIZE: usize = 4096;
pub const MEMORY_END: usize = 0x8800_0000;

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
}

impl FrameAlloc {
    const fn empty() -> Self {
        Self {
            cur: 0,
            end: 0,
            recycled: [0; 32768],
            rlen: 0,
        }
    }
    fn init(&mut self, start: usize, end: usize) {
        let s = (start + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);
        self.cur = s;
        self.end = end;
        self.rlen = 0;
    }
    fn alloc(&mut self) -> Option<usize> {
        if self.rlen > 0 {
            self.rlen -= 1;
            return Some(self.recycled[self.rlen]);
        }
        if self.cur + PAGE_SIZE <= self.end {
            let p = self.cur;
            self.cur += PAGE_SIZE;
            // zero the frame
            unsafe {
                core::ptr::write_bytes(p as *mut u8, 0, PAGE_SIZE);
            }
            Some(p)
        } else {
            None
        }
    }
    fn dealloc(&mut self, ppn_addr: usize) {
        if self.rlen < 32768 {
            unsafe {
                core::ptr::write_bytes(ppn_addr as *mut u8, 0, PAGE_SIZE);
            }
            self.recycled[self.rlen] = ppn_addr;
            self.rlen += 1;
        }
    }
}

pub fn init(kernel_end: usize) {
    ALLOC.lock().init(kernel_end, MEMORY_END);
}

pub fn alloc_frame() -> Option<usize> {
    ALLOC.lock().alloc()
}

pub fn dealloc_frame(pa: usize) {
    ALLOC.lock().dealloc(pa)
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
