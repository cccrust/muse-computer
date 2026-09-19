use core::arch::global_asm;

global_asm!(include_str!("switch.S"));

extern "C" {
    pub fn __switch(cur: *mut TaskContext, nxt: *const TaskContext);
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct TaskContext {
    pub ra: usize,
    pub sp: usize,
    pub s: [usize; 12],
}

impl TaskContext {
    pub const fn empty() -> Self {
        Self {
            ra: 0,
            sp: 0,
            s: [0; 12],
        }
    }
    pub fn goto_ra_sp(ra: usize, sp: usize) -> Self {
        Self { ra, sp, s: [0; 12] }
    }
}
