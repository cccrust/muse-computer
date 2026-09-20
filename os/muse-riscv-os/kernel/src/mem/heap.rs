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
        let node = (ptr as usize - 16) as *mut Node;
        (*node).next = FREE_HEAD;
        FREE_HEAD = node;
    }
}

#[global_allocator]
static A: HeapAlloc = HeapAlloc;
