use core::arch::{asm, global_asm};

global_asm!(include_str!("trap.S"));

extern "C" {
    fn __trap_entry();
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct TrapFrame {
    pub x: [usize; 32],
    pub sstatus: usize,
    pub sepc: usize,
    pub kernel_sp: usize,
}

impl TrapFrame {
    pub const fn empty() -> Self {
        Self {
            x: [0; 32],
            sstatus: 0,
            sepc: 0,
            kernel_sp: 0,
        }
    }
    pub fn syscall_id(&self) -> usize {
        self.x[17]
    }
    pub fn arg(&self, i: usize) -> usize {
        self.x[10 + i]
    }
    pub fn set_ret(&mut self, v: usize) {
        self.x[10] = v;
    }
}

static mut TRAP_STACK: [u8; 16384] = [0; 16384];

pub fn trap_stack_top() -> usize {
    unsafe { TRAP_STACK.as_ptr() as usize + TRAP_STACK.len() }
}

pub fn init() {
    unsafe {
        asm!("csrw stvec, {0}", in(reg) __trap_entry as usize);
    }
}

#[no_mangle]
pub extern "C" fn rust_trap_handler(tf: *mut TrapFrame) {
    unsafe {
        let scause: usize;
        let stval: usize;
        asm!("csrr {0}, scause", out(reg) scause);
        asm!("csrr {0}, stval", out(reg) stval);
        let tfm = &mut *tf;
        let is_int = (scause >> 63) != 0;
        let code = scause & 0xfff;
        if is_int {
            match code {
                5 => {
                    crate::timer::tick();
                    crate::timer::set_next();
                    if crate::timer::should_preempt() {
                        crate::task::set_yield_flag();
                    }
                }
                _ => {
                    crate::println!("[TRAP] unknown interrupt {}", code);
                }
            }
        } else {
            match code {
                8 => {
                    tfm.sepc += 4;
                    let id = tfm.syscall_id();
                    let a0 = tfm.arg(0);
                    let a1 = tfm.arg(1);
                    let a2 = tfm.arg(2);
                    let ret = crate::syscall::handle(id, a0, a1, a2, tf);
                    tfm.set_ret(ret as usize);
                    if crate::task::take_yield_flag() {
                        crate::task::schedule_point(tfm);
                    }
                }
                2 | 12 | 13 | 15 => {
                    crate::println!(
                        "[TRAP] page fault cause={} sepc={:#x} stval={:#x} -> kill",
                        code, tfm.sepc, stval
                    );
                    crate::syscall::do_exit(-2);
                }
                _ => {
                    crate::println!(
                        "[TRAP] unknown exception {} sepc={:#x} stval={:#x}",
                        code, tfm.sepc, stval
                    );
                    crate::syscall::do_exit(-1);
                }
            }
        }
        if crate::task::take_yield_flag() {
            crate::task::schedule_point(tfm);
        }
    }
}
