//! Interactive shell (task 0): line editor + built-in commands.

use core::arch::asm;
use core::ptr::addr_of;

use crate::clint::{CLINT_MTIMECMP0, TICK_HZ, TICK_INTERVAL, TIMEBASE_HZ, mtime_get};
use crate::csr::r_mhartid;
use crate::memory::{
    _boot_stack_bottom, _boot_stack_top, _ebss, _edata, _erodata, _etext, _sbss, _sdata,
    _srodata, _stext,
};
use crate::syscall::{sys_taskid, sys_ticks, sys_yield};
use crate::task::{CURRENT, MAX_TASKS, TASKS};
use crate::uart::{UART_BASE, getc_nb, intr_off, intr_restore, putc_raw};

fn exec_line(line: &str) {
    let cmd = line.trim();
    if cmd.is_empty() {
        return;
    }
    let mut parts = cmd.split_whitespace();
    let head = parts.next().unwrap_or("");
    match head {
        "help" => {
            crate::println!("commands:");
            crate::println!("  help            - this list");
            crate::println!("  info            - hart / build / memory map");
            crate::println!("  ps              - task list");
            crate::println!("  ticks           - timer ticks + uptime");
            crate::println!("  echo <text>     - print text");
            crate::println!("  yield           - voluntarily yield CPU");
            crate::println!("  whoami          - current task id");
            crate::println!("  timer           - raw mtime / mtimecmp");
            crate::println!("  fault           - trigger illegal-insn trap (halts)");
            crate::println!("  poweroff        - exit QEMU (SiFive test device)");
            crate::println!("  clear           - clear screen");
        }
        "info" => {
            let hart = r_mhartid();
            {
                crate::println!("embed-os 0.1.0 rv32imac, hart={}", hart);
                crate::println!(
                    "  .text   {:#x}..{:#x}",
                    addr_of!(_stext) as u32,
                    addr_of!(_etext) as u32
                );
                crate::println!(
                    "  .rodata {:#x}..{:#x}",
                    addr_of!(_srodata) as u32,
                    addr_of!(_erodata) as u32
                );
                crate::println!(
                    "  .data   {:#x}..{:#x}",
                    addr_of!(_sdata) as u32,
                    addr_of!(_edata) as u32
                );
                crate::println!(
                    "  .bss    {:#x}..{:#x}",
                    addr_of!(_sbss) as u32,
                    addr_of!(_ebss) as u32
                );
                crate::println!(
                    "  bootstk {:#x}..{:#x}",
                    addr_of!(_boot_stack_bottom) as u32,
                    addr_of!(_boot_stack_top) as u32
                );
            }
            crate::println!(
                "  timebase {} Hz, tick {} Hz ({} cycles)",
                TIMEBASE_HZ,
                TICK_HZ,
                TICK_INTERVAL
            );
            crate::println!("  console uart=0x{:x} (ns16550a, polling)", UART_BASE);
        }
        "ps" => {
            unsafe {
                crate::println!("id  name   alive  runs");
                for i in 0..MAX_TASKS {
                    let t = &TASKS[i];
                    let cur = if i == CURRENT { "*" } else { " " };
                    crate::println!(
                        "{}{}  {:<6} {}      {}",
                        cur,
                        i,
                        t.name,
                        t.alive as u8,
                        t.runs
                    );
                }
            }
        }
        "ticks" => {
            let t = sys_ticks();
            crate::println!("ticks={} uptime={}.{:02}s", t, t / 100, t % 100);
        }
        "echo" => {
            let rest = cmd.strip_prefix("echo").unwrap_or("").trim_start();
            crate::println!("{}", rest);
        }
        "yield" => {
            sys_yield();
            crate::println!("yielded (task {})", sys_taskid());
        }
        "whoami" => {
            crate::println!("task {}", sys_taskid());
        }
        "timer" => {
            let now = mtime_get();
            let cmp = unsafe {
                let b = CLINT_MTIMECMP0 as *const u32;
                ((core::ptr::read_volatile(b.add(1)) as u64) << 32)
                    | (core::ptr::read_volatile(b) as u64)
            };
            crate::println!("mtime={} mtimecmp={} ticks={}", now, cmp, sys_ticks());
        }
        "fault" => {
            crate::println!("triggering illegal instruction...");
            unsafe { asm!("unimp") };
            crate::println!("unreachable");
        }
        "poweroff" => {
            crate::println!("poweroff via SiFive test device...");
            unsafe {
                core::ptr::write_volatile(0x1000_00 as *mut u32, 0x5555);
            }
            loop {
                unsafe { asm!("wfi") };
            }
        }
        "clear" => {
            // don't use println (adds \n); raw escape
            let s = intr_off();
            for b in "\x1b[2J\x1b[H".bytes() {
                putc_raw(b);
            }
            intr_restore(s);
        }
        _ => {
            crate::println!("unknown cmd: \"{}\" (try help)", head);
        }
    }
}

pub(crate) extern "C" fn shell_task() -> ! {
    crate::println!("");
    crate::println!("type help + Enter. preemptive tasks: blink + fib running.");
    crate::print!("shell> ");
    let mut buf = [0u8; 128];
    let mut len: usize = 0;
    loop {
        match getc_nb() {
            Some(c) => {
                if c == b'\r' || c == b'\n' {
                    putc_raw(b'\n');
                    let line = core::str::from_utf8(&buf[..len]).unwrap_or("");
                    exec_line(line);
                    len = 0;
                    crate::print!("shell> ");
                } else if c == 0x08 || c == 0x7f {
                    if len > 0 {
                        len -= 1;
                        let s = intr_off();
                        putc_raw(0x08);
                        putc_raw(b' ');
                        putc_raw(0x08);
                        intr_restore(s);
                    }
                } else if c >= 0x20 && c < 0x7f {
                    if len < buf.len() {
                        buf[len] = c;
                        len += 1;
                        let s = intr_off();
                        putc_raw(c);
                        intr_restore(s);
                    }
                }
            }
            None => {
                sys_yield();
            }
        }
    }
}
