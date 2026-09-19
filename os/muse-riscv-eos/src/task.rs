//! Tasks: TrapFrame, Task table, init, round-robin scheduler.
//!
//! Single hart: task state is `static mut`, mutated only in trap context
//! (interrupts off) or during boot before the scheduler starts, except
//! 32-bit aligned reads which are atomic on RV32.

use crate::syscall::sys_yield;

pub(crate) const STACK_SIZE: usize = 4096;
pub(crate) const MAX_TASKS: usize = 3;

#[repr(C)]
pub struct TrapFrame {
    pub(crate) regs: [u32; 32],
    pub(crate) mepc: u32,
    pub(crate) mstatus: u32,
}

pub struct Task {
    pub(crate) frame: TrapFrame,
    kstack: [u8; STACK_SIZE],
    pub(crate) name: &'static str,
    pub(crate) alive: bool,
    pub(crate) runs: u32,
}

pub(crate) static mut TASKS: [Task; MAX_TASKS] = [
    Task {
        frame: TrapFrame { regs: [0; 32], mepc: 0, mstatus: 0 },
        kstack: [0; STACK_SIZE],
        name: "shell",
        alive: false,
        runs: 0,
    },
    Task {
        frame: TrapFrame { regs: [0; 32], mepc: 0, mstatus: 0 },
        kstack: [0; STACK_SIZE],
        name: "blink",
        alive: false,
        runs: 0,
    },
    Task {
        frame: TrapFrame { regs: [0; 32], mepc: 0, mstatus: 0 },
        kstack: [0; STACK_SIZE],
        name: "fib",
        alive: false,
        runs: 0,
    },
];

pub(crate) static mut CURRENT: usize = 0;
pub(crate) static mut TICKS: u64 = 0;
pub(crate) static mut NEXT_TIMECMP: u64 = 0;

extern "C" fn task_exit() -> ! {
    let id = unsafe { CURRENT };
    let name = unsafe { TASKS[id].name };
    crate::println!("[task {} \"{}\"] exited, parking", id, name);
    loop {
        sys_yield();
    }
}

pub(crate) fn task_init(idx: usize, entry: extern "C" fn() -> !, name: &'static str) {
    unsafe {
        let t = &mut TASKS[idx];
        let base = t.kstack.as_ptr() as usize;
        let top = (base + STACK_SIZE) & !0xF;
        t.frame.regs[0] = 0;
        t.frame.regs[1] = task_exit as *const () as usize as u32; // ra
        t.frame.regs[2] = top as u32; // sp
        t.frame.mepc = entry as usize as u32;
        // MPP=M, MPIE=1, MIE=1
        t.frame.mstatus = (3 << 11) | (1 << 7) | (1 << 3);
        t.name = name;
        t.alive = true;
        t.runs = 0;
    }
}

/// Round-robin to next alive task. Must be called with interrupts off
/// (we are inside a trap).
pub(crate) fn schedule(cur: *mut TrapFrame) -> *mut TrapFrame {
    unsafe {
        let n = MAX_TASKS;
        let mut nxt = (CURRENT + 1) % n;
        for _ in 0..n {
            if TASKS[nxt].alive {
                break;
            }
            nxt = (nxt + 1) % n;
        }
        if nxt == CURRENT {
            return cur;
        }
        TASKS[nxt].runs = TASKS[nxt].runs.wrapping_add(1);
        CURRENT = nxt;
        core::ptr::addr_of_mut!(TASKS[nxt].frame)
    }
}
