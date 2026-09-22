use crate::sync::SpinMutex;
use core::alloc::{GlobalAlloc, Layout};
use core::ptr::null_mut;

const HEAP_SIZE: usize = 8 * 1024 * 1024;
static mut HEAP: [u8; HEAP_SIZE] = [0; HEAP_SIZE];

struct Node {
    next: *mut Node,
    size: usize,
}

static mut FREE_HEAD: *mut Node = null_mut();
static mut HEAP_INITED: bool = false;

// v1.0: the heap is shared by all harts; the free list is only ever
// touched for short non-blocking list ops, so a spinlock suffices.
static HEAP_LOCK: SpinMutex<()> = SpinMutex::new(());

pub fn init() {
    unsafe {
        if HEAP_INITED {
            return;
        }
        let start = HEAP.as_mut_ptr() as usize;
        let aligned = (start + 15) & !15;
        let end = start + HEAP_SIZE;
        let node = aligned as *mut Node;
        (*node).next = null_mut();
        (*node).size = end - aligned;
        FREE_HEAD = node;
        HEAP_INITED = true;
    }
}

struct HeapAlloc;
unsafe impl GlobalAlloc for HeapAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let _g = HEAP_LOCK.lock();
        let size = (layout.size() + 15) & !15;
        let need = size + 16;
        let mut prev: *mut Node = null_mut();
        let mut cur = FREE_HEAD;
        while !cur.is_null() {
            let csize = (*cur).size;
            if csize >= need + 16 {
                let rest = (cur as usize) + need;
                let rn = rest as *mut Node;
                (*rn).next = (*cur).next;
                (*rn).size = csize - need;
                if prev.is_null() {
                    FREE_HEAD = rn;
                } else {
                    (*prev).next = rn;
                }
                (*cur).size = need;
                return (cur as usize + 16) as *mut u8;
            } else if csize >= need {
                if prev.is_null() {
                    FREE_HEAD = (*cur).next;
                } else {
                    (*prev).next = (*cur).next;
                }
                return (cur as usize + 16) as *mut u8;
            }
            prev = cur;
            cur = (*cur).next;
        }
        null_mut()
    }
    unsafe fn dealloc(&self, ptr: *mut u8, _layout: Layout) {
        if ptr.is_null() {
            return;
        }
        let _g = HEAP_LOCK.lock();
        // v1.6: insert sorted by address + coalesce neighbors. Without
        // this the LIFO list fragments into slivers under steady syscall
        // churn (Strings/Vecs per op) and large contiguours requests fail
        // while megabytes sit free (observed: 32KB fail on an 8MB heap
        // after a full autorun + writer soak).
        let mut node = (ptr as usize - 16) as *mut Node;
        let mut prev: *mut Node = null_mut();
        let mut cur = FREE_HEAD;
        while !cur.is_null() && (cur as usize) < node as usize {
            prev = cur;
            cur = (*cur).next;
        }
        // merge with next?
        if !cur.is_null() && (node as usize) + (*node).size == cur as usize {
            (*node).size += (*cur).size;
            (*node).next = (*cur).next;
        } else {
            (*node).next = cur;
        }
        // merge with prev?
        if !prev.is_null() && (prev as usize) + (*prev).size == node as usize {
            (*prev).size += (*node).size;
            (*prev).next = (*node).next;
        } else if prev.is_null() {
            FREE_HEAD = node;
        } else {
            (*prev).next = node;
        }
    }
}

#[global_allocator]
static A: HeapAlloc = HeapAlloc;

/// v1.6: heap watermark for OOM forensics (total free + largest run).
/// Called on the halt path; a shrinking largest-run with steady total
/// means fragmentation (fixed by dealloc coalescing above).
pub fn stats() -> (usize, usize) {
    unsafe {
        let _g = HEAP_LOCK.lock();
        let mut total = 0usize;
        let mut largest = 0usize;
        let mut cur = FREE_HEAD;
        while !cur.is_null() {
            total += (*cur).size;
            if (*cur).size > largest {
                largest = (*cur).size;
            }
            cur = (*cur).next;
        }
        (total, largest)
    }
}
