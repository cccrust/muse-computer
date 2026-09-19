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
        // enable supervisor timer (5) + external (9) interrupts;
        // SIE bit itself stays 0 in kernel, set on sret to user.
        let mut sie: usize;
        asm!("csrr {0}, sie", out(reg) sie);
        sie |= (1 << 5) | (1 << 9);
        asm!("csrw sie, {0}", in(reg) sie);
    }
    crate::plic::init();
    crate::uart::irq_enable();
    crate::uart::tx_enable();
}

#[no_mangle]
pub extern "C" fn rust_trap_handler(tf: *mut TrapFrame) {
    unsafe {
        let scause: usize;
        let stval: usize;
        asm!("csrr {0}, scause", out(reg) scause);
        asm!("csrr {0}, stval", out(reg) stval);
        // killed tasks die on any trap entry (covers timer-only victims)
        let cur = crate::task::current_pid();
        if crate::task::is_killed(cur) {
            let code = crate::task::kill_code(cur);
            crate::println!("[PROC] pid={} killed", cur);
            crate::syscall::do_exit(code);
        }
        let tfm = &mut *tf;
        let is_int = (scause >> 63) != 0;
        let code = scause & 0xfff;
        if is_int {
            match code {
                5 => {
                    crate::timer::tick();
                    crate::timer::set_next();
                    crate::task::wake_sleepers(crate::timer::ticks() as u64);
                    // v0.6 watchdog: re-check virtio-blocked tasks every tick
                    // so a lost completion IRQ delays I/O by ~10ms, not forever
                    crate::task::wake_virtio();
                    if crate::timer::should_preempt() {
                        crate::task::set_yield_flag();
                    }
                }
                9 => {
                    // supervisor external: PLIC
                    loop {
                        let irq = crate::plic::claim();
                        if irq == 0 {
                            break;
                        }
                        if irq == crate::plic::UART_IRQ {
                            crate::uart::on_irq();
                        } else if irq == crate::plic::VIRTIO_IRQ {
                            // v0.6: first claimed virtio completion IRQ proves
                            // device->PLIC->trap delivery. (By claim time the
                            // submitter usually already consumed+acked via the
                            // fast path or tick watchdog, so gate on claim,
                            // not on INTSTAT.)
                            static mut VIRTIO_IRQ_SEEN: bool = false;
                            unsafe {
                                if !VIRTIO_IRQ_SEEN {
                                    VIRTIO_IRQ_SEEN = true;
                                    crate::println!("[TEST] virtio-irq PASS");
                                }
                            }
                            crate::fs::virtio::on_irq();
                        }
                        crate::plic::complete(irq);
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
                    let cur_pid = crate::task::current_pid();
                    let traced = crate::task::is_traced(cur_pid);
                    let ret = crate::syscall::handle(id, a0, a1, a2, tf);
                    tfm.set_ret(ret as usize);
                    // v0.9 strace-lite: one line per traced syscall
                    if traced {
                        crate::println!(
                            "[STRACE] pid={} id={} a0={} ret={}",
                            cur_pid, id, a0, ret
                        );
                    }
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
