#![no_std]

use core::arch::asm;

#[inline(always)]
fn ecall(id: usize, a0: usize, a1: usize, a2: usize) -> isize {
    let ret: isize;
    unsafe {
        asm!(
            "ecall",
            inlateout("a0") a0 => ret,
            in("a1") a1,
            in("a2") a2,
            in("a7") id,
        );
    }
    ret
}

pub fn fork() -> isize {
    ecall(1, 0, 0, 0)
}
pub fn exit(code: i32) -> ! {
    ecall(2, code as usize, 0, 0);
    loop {}
}
pub fn wait(code_out: *mut i32) -> isize {
    ecall(3, code_out as usize, 0, 0)
}
pub fn pipe(fds: *mut i32) -> isize {
    ecall(4, fds as usize, 0, 0)
}
pub fn read(fd: isize, buf: *mut u8, len: usize) -> isize {
    ecall(5, fd as usize, buf as usize, len)
}
pub fn write(fd: isize, buf: *const u8, len: usize) -> isize {
    ecall(6, fd as usize, buf as usize, len)
}
pub fn close(fd: isize) -> isize {
    ecall(7, fd as usize, 0, 0)
}
pub fn exec(path: *const u8, argv: usize) -> isize {
    ecall(9, path as usize, argv, 0)
}
pub fn open(path: *const u8, flags: i32) -> isize {
    ecall(10, path as usize, flags as usize, 0)
}
pub fn dup(fd: isize) -> isize {
    ecall(11, fd as usize, 0, 0)
}
pub fn getpid() -> isize {
    ecall(12, 0, 0, 0)
}
pub fn sbrk(inc: i32) -> isize {
    ecall(13, inc as usize, 0, 0)
}
pub fn sleep(n: usize) -> isize {
    ecall(14, n, 0, 0)
}
pub fn mkdir(path: *const u8) -> isize {
    ecall(17, path as usize, 0, 0)
}
pub fn unlink(path: *const u8) -> isize {
    ecall(19, path as usize, 0, 0)
}
pub fn yield_() -> isize {
    ecall(21, 0, 0, 0)
}

pub fn print(s: &str) {
    let _ = write(1, s.as_ptr(), s.len());
}

pub fn eprint(s: &str) {
    let _ = write(2, s.as_ptr(), s.len());
}

pub fn read_line(buf: &mut [u8]) -> usize {
    // read until newline
    let mut n = 0;
    while n < buf.len() {
        let r = read(0, unsafe { buf.as_mut_ptr().add(n) }, 1);
        if r <= 0 {
            if n == 0 {
                return 0;
            }
            break;
        }
        n += 1;
        if buf[n - 1] == b'\n' {
            break;
        }
    }
    n
}
