//! Trap handling: `_start` / `trap_entry` assembly plus `rust_trap`.
//!
//! Layout contract with the assembly below: [`TrapFrame`] is `#[repr(C)]`
//! with `regs[32]` at offset 0, `mepc` at +128, `mstatus` at +132.
//! `rust_trap` runs on the dedicated trap stack (`_trap_stack_top`),
//! never on a TrapFrame — see README for why that matters.

use core::arch::asm;

use crate::clint::{CLINT_MSIP0, TICK_INTERVAL, mtimecmp_set};
use crate::csr::{r_mcause, r_mtval, w_mie};
use crate::syscall::{SYS_GETC, SYS_PUTC, SYS_TASKID, SYS_TICKS, SYS_YIELD};
use crate::task::{CURRENT, NEXT_TIMECMP, TASKS, TICKS, TrapFrame, schedule};
use crate::uart::{_print_raw, getc_nb, putc_raw};

core::arch::global_asm!(r#"
    .option arch, rv32imac_zicsr
    .option nopic
    .section .text.init, "ax"
    .global _start
    .align 2
_start:
    la sp, _boot_stack_top
    la t0, _sbss
    la t1, _ebss
1:
    bge t0, t1, 2f
    sw zero, 0(t0)
    addi t0, t0, 4
    j 1b
2:
    csrw mscratch, zero
    call rust_main
3:
    wfi
    j 3b

    .section .text, "ax"
    .global trap_entry
    .align 2
trap_entry:
    csrrw sp, mscratch, sp
    sw x1, 1*4(sp)
    sw x5, 5*4(sp)
    csrr t0, mscratch
    sw t0, 2*4(sp)
    sw x3, 3*4(sp)
    sw x4, 4*4(sp)
    sw x6, 6*4(sp)
    sw x7, 7*4(sp)
    sw x8, 8*4(sp)
    sw x9, 9*4(sp)
    sw x10, 10*4(sp)
    sw x11, 11*4(sp)
    sw x12, 12*4(sp)
    sw x13, 13*4(sp)
    sw x14, 14*4(sp)
    sw x15, 15*4(sp)
    sw x16, 16*4(sp)
    sw x17, 17*4(sp)
    sw x18, 18*4(sp)
    sw x19, 19*4(sp)
    sw x20, 20*4(sp)
    sw x21, 21*4(sp)
    sw x22, 22*4(sp)
    sw x23, 23*4(sp)
    sw x24, 24*4(sp)
    sw x25, 25*4(sp)
    sw x26, 26*4(sp)
    sw x27, 27*4(sp)
    sw x28, 28*4(sp)
    sw x29, 29*4(sp)
    sw x30, 30*4(sp)
    sw x31, 31*4(sp)
    csrr t0, mepc
    sw t0, 32*4(sp)
    csrr t0, mstatus
    sw t0, 33*4(sp)
    mv a0, sp
    la sp, _trap_stack_top
    call rust_trap
    mv t0, a0
    lw t1, 33*4(t0)
    csrw mstatus, t1
    lw t1, 32*4(t0)
    csrw mepc, t1
    lw x1, 1*4(t0)
    lw x3, 3*4(t0)
    lw x4, 4*4(t0)
    lw x6, 6*4(t0)
    lw x7, 7*4(t0)
    lw x8, 8*4(t0)
    lw x9, 9*4(t0)
    lw x10, 10*4(t0)
    lw x11, 11*4(t0)
    lw x12, 12*4(t0)
    lw x13, 13*4(t0)
    lw x14, 14*4(t0)
    lw x15, 15*4(t0)
    lw x16, 16*4(t0)
    lw x17, 17*4(t0)
    lw x18, 18*4(t0)
    lw x19, 19*4(t0)
    lw x20, 20*4(t0)
    lw x21, 21*4(t0)
    lw x22, 22*4(t0)
    lw x23, 23*4(t0)
    lw x24, 24*4(t0)
    lw x25, 25*4(t0)
    lw x26, 26*4(t0)
    lw x27, 27*4(t0)
    lw x28, 28*4(t0)
    lw x29, 29*4(t0)
    lw x30, 30*4(t0)
    lw x31, 31*4(t0)
    csrw mscratch, t0
    lw sp, 2*4(t0)
    lw t0, 5*4(t0)
    mret

    .global enter_first_task
    .align 2
enter_first_task:
    mv t0, a0
    lw t1, 33*4(t0)
    csrw mstatus, t1
    lw t1, 32*4(t0)
    csrw mepc, t1
    lw x1, 1*4(t0)
    lw x3, 3*4(t0)
    lw x4, 4*4(t0)
    lw x6, 6*4(t0)
    lw x7, 7*4(t0)
    lw x8, 8*4(t0)
    lw x9, 9*4(t0)
    lw x10, 10*4(t0)
    lw x11, 11*4(t0)
    lw x12, 12*4(t0)
    lw x13, 13*4(t0)
    lw x14, 14*4(t0)
    lw x15, 15*4(t0)
    lw x16, 16*4(t0)
    lw x17, 17*4(t0)
    lw x18, 18*4(t0)
    lw x19, 19*4(t0)
    lw x20, 20*4(t0)
    lw x21, 21*4(t0)
    lw x22, 22*4(t0)
    lw x23, 23*4(t0)
    lw x24, 24*4(t0)
    lw x25, 25*4(t0)
    lw x26, 26*4(t0)
    lw x27, 27*4(t0)
    lw x28, 28*4(t0)
    lw x29, 29*4(t0)
    lw x30, 30*4(t0)
    lw x31, 31*4(t0)
    csrw mscratch, t0
    lw sp, 2*4(t0)
    lw t0, 5*4(t0)
    mret
"#);

unsafe extern "C" {
    pub(crate) fn trap_entry();
    pub(crate) fn enter_first_task(frame: *const TrapFrame) -> !;
}

/// Reentrancy guard: set while printing a fault dump (timer is off then).
/// A nested fault (e.g. dump touching bad memory) halts instead of recursing.
static mut IN_TRAP_DUMP: bool = false;

fn mcause_str(mcause: u32) -> &'static str {
    match mcause {
        0x8000_0007 => "machine timer interrupt",
        0x8000_0003 => "machine software interrupt",
        0x8000_000B => "machine external interrupt",
        0xB => "ecall from M-mode",
        0x2 => "illegal instruction",
        0x0 => "instruction address misaligned",
        0x1 => "instruction access fault",
        0x4 => "load address misaligned",
        0x5 => "load access fault",
        0x6 => "store address misaligned",
        0x7 => "store access fault",
        0x3 => "breakpoint",
        _ => "unknown",
    }
}

#[no_mangle]
pub extern "C" fn rust_trap(frame: *mut TrapFrame) -> *mut TrapFrame {
    let mcause = r_mcause();
    let mtval = r_mtval();
    let f = unsafe { &mut *frame };

    // ---- interrupts ----
    if mcause & 0x8000_0000 != 0 {
        match mcause & 0x7FFF_FFFF {
            7 => {
                // timer: advance tick, reprogram, preempt
                unsafe {
                    TICKS = TICKS.wrapping_add(1);
                    NEXT_TIMECMP = NEXT_TIMECMP.wrapping_add(TICK_INTERVAL);
                    mtimecmp_set(NEXT_TIMECMP);
                }
                return schedule(frame);
            }
            3 => {
                // software interrupt: clear msip0, treat as yield
                unsafe {
                    core::ptr::write_volatile(CLINT_MSIP0 as *mut u32, 0);
                }
                return schedule(frame);
            }
            11 => {
                // external: UART is polled, nothing to do
                return frame;
            }
            _ => return frame,
        }
    }

    // ---- exceptions ----
    match mcause {
        0xB => {
            // ecall: advance mepc past ecall (2 or 4 bytes)
            let insn = unsafe { *(f.mepc as *const u16) };
            let adv = if insn & 0x3 == 0x3 { 4 } else { 2 };
            f.mepc = f.mepc.wrapping_add(adv);

            let nr = f.regs[17]; // a7
            match nr {
                SYS_YIELD => return schedule(frame),
                SYS_PUTC => {
                    putc_raw(f.regs[10] as u8);
                    return frame;
                }
                SYS_GETC => {
                    f.regs[10] = match getc_nb() {
                        Some(b) => b as u32,
                        None => 0xFFFF_FFFF,
                    };
                    return frame;
                }
                SYS_TICKS => {
                    let t = unsafe { TICKS };
                    f.regs[10] = t as u32;
                    f.regs[11] = (t >> 32) as u32;
                    return frame;
                }
                SYS_TASKID => {
                    f.regs[10] = unsafe { CURRENT } as u32;
                    return frame;
                }
                _ => return frame,
            }
        }
        _ => {
            // fault: dump and halt (or kill non-shell task).
            // Silence the timer first so the dump cannot be preempted,
            // and guard against a nested fault while dumping.
            w_mie(0);
            unsafe {
                if IN_TRAP_DUMP {
                    loop {
                        asm!("wfi");
                    }
                }
                IN_TRAP_DUMP = true;
            }
            unsafe {
                _print_raw(format_args!(
                    "\n[FATAL] {} (mcause={:#x} mepc={:#x} mtval={:#x}) on task {} \"{}\"\n",
                    mcause_str(mcause),
                    mcause,
                    f.mepc,
                    mtval,
                    CURRENT,
                    TASKS[CURRENT].name
                ));
                _print_raw(format_args!(
                    "  ra={:#x} sp={:#x} gp={:#x} tp={:#x} t0={:#x} t1={:#x} t2={:#x}\n",
                    f.regs[1], f.regs[2], f.regs[3], f.regs[4], f.regs[5], f.regs[6], f.regs[7]
                ));
                _print_raw(format_args!(
                    "  s0={:#x} s1={:#x} a0={:#x} a1={:#x} a2={:#x} a3={:#x} a4={:#x} a5={:#x}\n",
                    f.regs[8], f.regs[9], f.regs[10], f.regs[11], f.regs[12], f.regs[13],
                    f.regs[14], f.regs[15]
                ));
                _print_raw(format_args!(
                    "  a6={:#x} a7={:#x} s2={:#x} s3={:#x} s4={:#x} s5={:#x} s6={:#x} s7={:#x}\n",
                    f.regs[16], f.regs[17], f.regs[18], f.regs[19], f.regs[20], f.regs[21],
                    f.regs[22], f.regs[23]
                ));
                _print_raw(format_args!(
                    "  s8={:#x} s9={:#x} s10={:#x} s11={:#x} t3={:#x} t4={:#x} t5={:#x} t6={:#x}\n",
                    f.regs[24], f.regs[25], f.regs[26], f.regs[27], f.regs[28], f.regs[29],
                    f.regs[30], f.regs[31]
                ));
            }
            if unsafe { CURRENT } != 0 {
                unsafe {
                    _print_raw(format_args!("[trap] killing task {}\n", CURRENT));
                    TASKS[CURRENT].alive = false;
                    IN_TRAP_DUMP = false;
                }
                return schedule(frame);
            }
            loop {
                unsafe { asm!("wfi") };
            }
        }
    }
}
