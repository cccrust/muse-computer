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
pub fn kill(pid: isize) -> isize {
    ecall(8, pid as usize, 0, 0)
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
pub fn getdents(path: *const u8, buf: *mut u8, len: usize) -> isize {
    ecall(22, path as usize, buf as usize, len)
}
pub fn shutdown() -> ! {
    ecall(23, 0, 0, 0);
    loop {}
}
/// set foreground pid for Ctrl-C delivery
pub fn setfg(pid: isize) -> isize {
    ecall(24, pid as usize, 0, 0)
}
/// fstat(fd, out: *mut u32[3]) -> out = [kind, size, nlink]
pub fn fstat(fd: isize, out: *mut u32) -> isize {
    ecall(20, fd as usize, out as usize, 0)
}
pub fn link(old: *const u8, new: *const u8) -> isize {
    ecall(18, old as usize, new as usize, 0)
}

/// v0.5: chdir / getcwd (SYS_CHDIR=16, SYS_GETCWD=25)
pub fn chdir(path: *const u8) -> isize {
    ecall(16, path as usize, 0, 0)
}
pub fn getcwd(buf: *mut u8, len: usize) -> isize {
    ecall(25, buf as usize, len, 0)
}

/// v0.5: lseek (SYS_LSEEK=26). whence: 0=SET, 1=CUR, 2=END.
pub fn lseek(fd: isize, off: isize, whence: usize) -> isize {
    ecall(26, fd as usize, off as usize, whence)
}

/// v0.5: dup2 (SYS_DUP2=27)
pub fn dup2(old: isize, new: isize) -> isize {
    ecall(27, old as usize, new as usize, 0)
}

/// v0.5: waitpid (SYS_WAITPID=28). pid>0 specific, -1 any; options&1=WNOHANG.
pub fn waitpid(pid: isize, code_out: *mut i32, options: usize) -> isize {
    ecall(28, pid as usize, code_out as usize, options)
}

/// v0.5: fsstat (SYS_FSSTAT=29): writes "total=N free=M\n" into buf.
pub fn fsstat(buf: *mut u8, len: usize) -> isize {
    ecall(29, buf as usize, len, 0)
}

/// open flags (must match kernel sys_open)
pub const O_CREATE: i32 = 0x40;
pub const O_TRUNC: i32 = 0x200;
pub const O_APPEND: i32 = 0x400;
pub const O_CLOEXEC: i32 = 0x80000;

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

/// argv[i] as byte slice (NUL-terminated, cap 256). Bounds/NULL safe.
pub unsafe fn argv_str(
    argv: *const *const u8,
    i: usize,
    argc: usize,
) -> Option<&'static [u8]> {
    if i >= argc || argv.is_null() {
        return None;
    }
    let p = *argv.add(i);
    if p.is_null() {
        return None;
    }
    let mut n = 0;
    while n < 256 && *p.add(n) != 0 {
        n += 1;
    }
    Some(core::slice::from_raw_parts(p, n))
}
