use crate::trap::TrapFrame;

// Syscall numbers (Unix-v6 inspired, xv6-compatible subset)
pub const SYS_FORK: usize = 1;
pub const SYS_EXIT: usize = 2;
pub const SYS_WAIT: usize = 3;
pub const SYS_PIPE: usize = 4;
pub const SYS_READ: usize = 5;
pub const SYS_WRITE: usize = 6;
pub const SYS_CLOSE: usize = 7;
pub const SYS_KILL: usize = 8;
pub const SYS_EXEC: usize = 9;
pub const SYS_OPEN: usize = 10;
pub const SYS_DUP: usize = 11;
pub const SYS_GETPID: usize = 12;
pub const SYS_SBRK: usize = 13;
pub const SYS_SLEEP: usize = 14;
pub const SYS_MKNOD: usize = 15;
pub const SYS_CHDIR: usize = 16;
pub const SYS_MKDIR: usize = 17;
pub const SYS_LINK: usize = 18;
pub const SYS_UNLINK: usize = 19;
pub const SYS_FSTAT: usize = 20;
pub const SYS_YIELD: usize = 21;
pub const SYS_GETDENTS: usize = 22;
pub const SYS_SHUTDOWN: usize = 23;
pub const SYS_SETFG: usize = 24;
// v0.5
pub const SYS_GETCWD: usize = 25;
pub const SYS_LSEEK: usize = 26;
pub const SYS_DUP2: usize = 27;
pub const SYS_WAITPID: usize = 28;
pub const SYS_FSSTAT: usize = 29;
// v0.8
pub const SYS_MMAP: usize = 30;
pub const SYS_MUNMAP: usize = 31;
// v0.9
pub const SYS_PS: usize = 32;
pub const SYS_TRACE: usize = 33;
// v0.11
pub const SYS_EXECVE: usize = 34;
// v1.0
pub const SYS_GETHART: usize = 35;
// v1.2
pub const SYS_MEMSTAT: usize = 36;
// v1.3: UDP sockets (socket/connect/send/recv). Port args and IPv4
// addresses are plain usize values (ip BE u32, port u16 in low bits).
pub const SYS_SOCKET: usize = 37;
pub const SYS_CONNECT: usize = 38;
pub const SYS_SEND: usize = 39;
pub const SYS_RECV: usize = 40;
// v1.8: monotonic ms since boot (no wall clock: virt has no RTC hw).
pub const SYS_TIME: usize = 44;
// v2.0: chroot jail (per-proc fs root; tightening only, see sys_chroot).
pub const SYS_CHROOT: usize = 45;
// v2.1: unshare (pid namespace only for now; other flags -> -1).
pub const SYS_UNSHARE: usize = 46;
// v2.2: cgroup-lite (create-with-limit / enter / set-limit).
pub const SYS_CGCREATE: usize = 47;
pub const SYS_CGENTER: usize = 48;
pub const SYS_CGLIMIT: usize = 49;
// v2.3: CPU cap percent (0-100) for a cgroup.
pub const SYS_CGSETCPU: usize = 50;
// v2.4: cross-namespace pid liveness + start-tick identity (ctr ps/stop).
pub const SYS_PIDINFO: usize = 51;
// v2.5: exit code of a reaped pid (ctr ps code display).
pub const SYS_REAPSTAT: usize = 52;
// v2.6: kill all live tasks in a cgroup (ctr stop fate-sharing).
pub const SYS_CGKILL: usize = 53;
// v2.7: CPU share weight for a cgroup (vruntime fairness).
pub const SYS_CGSETSHARE: usize = 54;
// v2.9: join an existing (non-root) pid namespace (ctr exec).
pub const SYS_NSENTER: usize = 55;
// v3.5: bind a host dir into the caller's view (ctr volume -v).
pub const SYS_MOUNTVOL: usize = 56;
// v3.6: read back decoded cgroup quotas (ctr ps).
pub const SYS_CGSTAT: usize = 57;
// v1.5: TCP listen-side + bind.
pub const SYS_BIND: usize = 41;
pub const SYS_LISTEN: usize = 42;
pub const SYS_ACCEPT: usize = 43;

pub fn handle(id: usize, a0: usize, a1: usize, a2: usize, tf: *mut TrapFrame) -> isize {
    match id {
        SYS_FORK => sys_fork() as isize,
        SYS_EXIT => {
            do_exit(a0 as i32);
            0
        }
        SYS_WAIT => sys_wait(a0) as isize,
        SYS_READ => sys_read(a0 as i32, a1, a2) as isize,
        // v1.8: mutating syscalls run inside one journal TX (per-syscall
        // atomic durability): begin/commit bracket the whole op; syscalls
        // don't nest, and commit() with zero records writes nothing (so
        // console/pipe writes cost nothing extra).
        SYS_WRITE => {
            crate::fs::jnl::begin();
            let r = sys_write(a0 as i32, a1, a2) as isize;
            crate::fs::jnl::commit();
            r
        }
        SYS_OPEN => {
            crate::fs::jnl::begin();
            let r = sys_open(a0, a1 as i32) as isize;
            crate::fs::jnl::commit();
            r
        }
        SYS_CLOSE => sys_close(a0 as i32) as isize,
        SYS_DUP => sys_dup(a0 as i32) as isize,
        SYS_PIPE => sys_pipe(a0) as isize,
        SYS_EXEC => sys_exec(a0, a1) as isize,
        SYS_GETPID => crate::task::current_lpid() as isize,
        SYS_SBRK => sys_sbrk(a0 as i32) as isize,
        SYS_SLEEP => sys_sleep(a0) as isize,
        SYS_SETFG => {
            // v2.1: sh passes its lpid; translate to global HERE (syscall
            // context = sh's own ns) so the ISR-side kill_fg needs no ns.
            crate::task::set_fg_global(a0);
            0
        }
        SYS_KILL => sys_kill(a0) as isize,
        SYS_MKDIR => {
            crate::fs::jnl::begin();
            let r = sys_mkdir(a0) as isize;
            crate::fs::jnl::commit();
            r
        }
        SYS_CHDIR => sys_chdir(a0) as isize,
        SYS_MKNOD => 0,
        SYS_LINK => {
            crate::fs::jnl::begin();
            let r = sys_link(a0, a1) as isize;
            crate::fs::jnl::commit();
            r
        }
        SYS_UNLINK => {
            crate::fs::jnl::begin();
            let r = sys_unlink(a0) as isize;
            crate::fs::jnl::commit();
            r
        }
        SYS_FSTAT => sys_fstat(a0 as i32, a1) as isize,
        SYS_YIELD => {
            crate::task::yield_now();
            0
        }
        SYS_GETDENTS => sys_getdents(a0, a1, a2) as isize,
        SYS_GETCWD => sys_getcwd(a0, a1) as isize,
        SYS_LSEEK => sys_lseek(a0 as i32, a1 as isize, a2) as isize,
        SYS_DUP2 => sys_dup2(a0 as i32, a1 as i32) as isize,
        SYS_WAITPID => sys_waitpid(a0 as isize, a1, a2) as isize,
        SYS_FSSTAT => sys_fsstat(a0, a1) as isize,
        SYS_MMAP => sys_mmap(a0, a1, a2) as isize,
        SYS_MUNMAP => sys_munmap(a0, a1) as isize,
        SYS_PS => sys_ps(a0, a1) as isize,
        SYS_TRACE => sys_trace(a0, a1) as isize,
        SYS_EXECVE => sys_execve(a0, a1, a2) as isize,
        SYS_GETHART => (crate::task::hartid() % crate::MAX_HART) as isize,
        SYS_MEMSTAT => crate::mem::frame::free_frames() as isize,
        SYS_SOCKET => sys_socket(a0) as isize,
        SYS_CONNECT => sys_connect(a0 as i32, a1 as u32, a2 as u16) as isize,
        SYS_SEND => sys_send(a0 as i32, a1, a2) as isize,
        SYS_RECV => sys_recv(a0 as i32, a1, a2) as isize,
        // v1.8: monotonic ms since boot (timer::ticks is 10ms units).
        SYS_TIME => (crate::timer::ticks() * 10) as isize,
        SYS_CHROOT => sys_chroot(a0) as isize,
        SYS_UNSHARE => sys_unshare(a0) as isize,
        SYS_CGCREATE => sys_cgcreate(a0 as u64) as isize,
        SYS_CGENTER => sys_cgenter(a0) as isize,
        SYS_CGLIMIT => sys_cglimit(a0, a1 as u64) as isize,
        SYS_CGSETCPU => sys_cgsetcpu(a0, a1 as u64) as isize,
        SYS_PIDINFO => crate::task::pidinfo(a0) as isize,
        SYS_REAPSTAT => crate::task::reapstat(a0) as isize,
        SYS_CGKILL => crate::task::cg_kill(a0) as isize,
        SYS_CGSETSHARE => sys_cgsetshare(a0, a1 as u64) as isize,
        SYS_NSENTER => sys_nsenter(a0) as isize,
        SYS_MOUNTVOL => sys_mountvol(a0, a1) as isize,
        SYS_CGSTAT => sys_cgstat(a0, a1) as isize,
        SYS_BIND => sys_bind(a0 as i32, a1 as u16) as isize,
        SYS_LISTEN => sys_listen(a0 as i32) as isize,
        SYS_ACCEPT => sys_accept(a0 as i32) as isize,
        SYS_SHUTDOWN => {
            crate::println!("[SYS] shutdown");
            if crate::fs::use_disk() {
                crate::fs::blk::sync();
                // v1.6: void the journal generation BEFORE clearing dirty
                // (either order is safe: dirty + fresh gen replays nothing).
                crate::fs::jnl::reset();
                crate::fs::disk::set_dirty(false);
                crate::println!("[FS] marked clean");
            }
            // v1.1: scheduler balance stats (steals across harts).
            crate::task::print_stats();
            crate::sbi::shutdown();
        }
        _ => {
            crate::println!("[SYSCALL] unknown {}", id);
            -1
        }
    }
}

pub fn do_exit(code: i32) -> ! {
    let pid = crate::task::current_pid();
    crate::task::exit(pid, code);
    unreachable!()
}

fn sys_kill(pid: usize) -> isize {
    if crate::task::kill(pid) {
        0
    } else {
        -1
    }
}

fn sys_fork() -> usize {
    let pid = crate::task::current_pid();
    let child = crate::task::fork(pid);
    crate::println!("[PROC] fork pid={} -> child={}", pid, child);
    child
}

fn sys_wait(uaddr: usize) -> isize {
    // uaddr: *mut i32 for exit code, may be 0
    let pid = crate::task::current_pid();
    let (c, code) = crate::task::wait(pid);
    if c == -2 {
        return -2; // would block, user retries
    }
    if c < 0 {
        return -1;
    }
    if uaddr != 0 {
        unsafe {
            *(uaddr as *mut i32) = code as i32;
        }
    }
    c as isize
}

fn sys_read(fd: i32, buf: usize, len: usize) -> usize {
    if len == 0 {
        return 0;
    }
    // TEMP DBG v1.0-2: wild userlen writes corrupt kernel memory (S-mode
    // can touch anything); freeze hotel for inspection instead of mayhem.
    if len > 65536 {
        crate::println!(
            "[DBG] HUGE-READ fd={} buf={:#x} len={} pid={} hart={}",
            fd,
            buf,
            len,
            crate::task::current_pid(),
            crate::task::hartid() % crate::MAX_HART
        );
        loop {
            unsafe { core::arch::asm!("wfi") };
        }
    }
    let pid = crate::task::current_pid();
    let (kind, path_id, off) = crate::task::with_current(|p| {
        if fd < 0 || fd >= 16 {
            (0, 0, 0)
        } else {
            (p.fd_kind[fd as usize], p.fd_path[fd as usize], p.fd_off[fd as usize])
        }
    });
    unsafe {
        let dst = crate::fs::user_slice_mut(buf, len);
        match kind {
            1 => {
                // stdin: ISR-filled ring; block if empty (woken by UART ISR).
                // EOF flag (Ctrl-D) yields available bytes, then 0.
                let mut n = 0;
                while n < len {
                    match crate::uart::getchar() {
                        Some(c) => {
                            dst[n] = c;
                            n += 1;
                            if c == b'\n' {
                                break;
                            }
                        }
                        None => break,
                    }
                }
                if n == 0 {
                    if crate::uart::take_eof() {
                        return 0;
                    }
                    // truly block: descheduled until ISR wakes us; the
                    // pre-written 0 is delivered on resume, caller retries.
                    crate::task::block_current(crate::task::BLOCK_STDIN, 0);
                    crate::task::yield_now();
                }
                n
            }
            4 => {
                // file: need path from id
                let path = fd_path_to_string(path_id);
                let n = crate::fs::read_at(&path, off, dst);
                crate::task::with_current_mut(|p| {
                    p.fd_off[fd as usize] = off + n;
                });
                n
            }
            5 => {
                // pipe read end: id = path_id
                let n = crate::fs::pipe::read(path_id as usize, dst);
                n
            }
            _ => 0,
        }
    }
}

fn sys_write(fd: i32, buf: usize, len: usize) -> usize {
    if len == 0 {
        return 0;
    }
    // TEMP DBG v1.0-2: see sys_read note.
    if len > 65536 {
        crate::println!(
            "[DBG] HUGE-WRITE fd={} buf={:#x} len={} pid={} hart={}",
            fd,
            buf,
            len,
            crate::task::current_pid(),
            crate::task::hartid() % crate::MAX_HART
        );
        loop {
            unsafe { core::arch::asm!("wfi") };
        }
    }
    let pid = crate::task::current_pid();
    let (kind, path_id, off) = crate::task::with_current(|p| {
        if fd < 0 || fd >= 16 {
            (0, 0, 0)
        } else {
            (p.fd_kind[fd as usize], p.fd_path[fd as usize], p.fd_off[fd as usize])
        }
    });
    unsafe {
        let src = crate::fs::user_slice(buf, len);
        match kind {
            2 => {
                // stdout/stderr -> console (whole write under one lock;
                // see console::write_bytes -- byte-wise putchar here
                // would tear lines against other harts' prints)
                crate::console::write_bytes(src);
                len
            }
            4 => {
                let path = fd_path_to_string(path_id);
                let n = crate::fs::write_at(&path, off, src);
                crate::task::with_current_mut(|p| {
                    p.fd_off[fd as usize] = off + n;
                });
                n
            }
            6 => {
                let n = crate::fs::pipe::write(path_id as usize, src);
                n
            }
            _ => 0,
        }
    }
}

fn sys_open(path_ptr: usize, flags: i32) -> isize {
    unsafe {
        let raw = match crate::fs::user_str(path_ptr) {
            Some(s) => s,
            None => return -1,
        };
        let pid = crate::task::current_pid();
        let path = crate::task::resolve_for(pid, &raw);
        // flags: 0=R,1=W,2=RW, 0x40=CREATE, 0x200=TRUNC (match user-lib)
        // v0.5: 0x400=APPEND, 0x80000=CLOEXEC
        const O_CREATE: i32 = 0x40;
        const O_TRUNC: i32 = 0x200;
        const O_APPEND: i32 = 0x400;
        const O_CLOEXEC: i32 = 0x80000;
        // v2.3: existence must bypass the /bin embed fallback in
        // fs::read_file (it returns embedded ELFs by basename: creating
        // e.g. /ctr/testimg/bin/echo would "exist" as embedded `echo`,
        // skip creation, and then every write fails on the missing
        // disk file). exec keeps using the fallback; open must not.
        let exists = if crate::fs::use_disk() {
            crate::fs::disk::read_file(&path).is_some()
        } else {
            crate::fs::ramfs::read_file(&path).is_some()
        };
        if !exists {
            if flags & O_CREATE != 0 {
                crate::fs::write_file(&path, b"");
            } else {
                return -1;
            }
        } else if flags & O_TRUNC != 0 {
            crate::fs::truncate(&path);
        }
        // alloc fd
        let mut ret: isize = -1;
        crate::task::with_current_mut(|p| {
            for i in 3..16 {
                if p.fd_kind[i] == 0 {
                    p.fd_kind[i] = 4;
                    // APPEND starts at end (after possible TRUNC above)
                    p.fd_off[i] = if flags & O_APPEND != 0 {
                        crate::fs::file_len(&path).unwrap_or(0)
                    } else {
                        0
                    };
                    p.fd_path[i] = register_path(&path);
                    p.fds[i] = i as i32;
                    p.fd_cloexec[i] = flags & O_CLOEXEC != 0;
                    ret = i as isize;
                    break;
                }
            }
        });
        ret
    }
}

/// v1.3: UDP sockets. fd_kind 7, fds[] = socket index. Only datagram
/// semantics; no TCP, no bind (ephemeral ports), peer set by connect.
fn sys_close(fd: i32) -> isize {
    if fd < 0 || fd >= 16 {
        return -1;
    }
    // v1.3: sockets free their slot on close (fd_kind 7, fds[] = sock idx)
    let sock = crate::task::with_current(|p| {
        if p.fd_kind[fd as usize] == 7 {
            Some(p.fds[fd as usize])
        } else {
            None
        }
    });
    if let Some(s) = sock {
        if s >= 0 {
            crate::net::sock_close(s as usize);
        }
    }
    crate::task::with_current_mut(|p| {
        p.fd_kind[fd as usize] = 0;
        p.fds[fd as usize] = -1;
        p.fd_cloexec[fd as usize] = false;
    });
    0
}

fn sys_socket(kind: usize) -> isize {
    // v1.5: kind 0 = UDP, 1 = TCP. v1.6: kind 2 = ICMP (ping).
    if kind > 2 {
        return -1;
    }
    let idx = match crate::net::sock_open_kind(kind as u8) {
        Some(i) => i,
        None => return -1,
    };
    let mut ret: isize = -1;
    crate::task::with_current_mut(|p| {
        for i in 3..16 {
            if p.fd_kind[i] == 0 {
                p.fd_kind[i] = 7;
                p.fds[i] = idx as i32;
                p.fd_off[i] = 0;
                p.fd_path[i] = 0;
                p.fd_cloexec[i] = false;
                ret = i as isize;
                break;
            }
        }
    });
    if ret < 0 {
        crate::net::sock_close(idx);
    }
    ret
}

fn sock_idx_of(fd: i32) -> Option<usize> {
    crate::task::with_current(|p| {
        if fd < 0 || fd >= 16 || p.fd_kind[fd as usize] != 7 || p.fds[fd as usize] < 0 {
            None
        } else {
            Some(p.fds[fd as usize] as usize)
        }
    })
}

/// v1.5: bind/listen/accept (fd_kind 7 sockets; TCP listen-side).
fn sys_bind(fd: i32, port: u16) -> isize {
    match sock_idx_of(fd) {
        Some(s) => {
            if crate::net::sock_bind(s, port) {
                0
            } else {
                -1
            }
        }
        None => -1,
    }
}

fn sys_listen(fd: i32) -> isize {
    match sock_idx_of(fd) {
        Some(s) => {
            if crate::net::sock_listen(s) {
                0
            } else {
                -1
            }
        }
        None => -1,
    }
}

fn sys_accept(fd: i32) -> isize {
    let child = match sock_idx_of(fd) {
        Some(s) => match crate::net::sock_accept(s) {
            Some(c) => c,
            None => return -2, // WouldBlock: no completed handshake yet
        },
        None => return -1,
    };
    // allocate an fd for the child socket (fd_kind 7, like sys_socket)
    let mut ret: isize = -1;
    crate::task::with_current_mut(|p| {
        for i in 3..16 {
            if p.fd_kind[i] == 0 {
                p.fd_kind[i] = 7;
                p.fds[i] = child as i32;
                p.fd_off[i] = 0;
                p.fd_path[i] = 0;
                p.fd_cloexec[i] = false;
                ret = i as isize;
                break;
            }
        }
    });
    if ret >= 0 {
        crate::net::sock_accept_commit(child);
    }
    ret
}

fn sys_connect(fd: i32, ip_be: u32, port: u16) -> isize {
    match sock_idx_of(fd) {
        Some(s) => {
            // v1.7: TCP sockets take the active-open path (0/-2/-1);
            // UDP/ICMP keep the boolean path.
            if crate::net::sock_kind(s) == Some(1) {
                return crate::net::sock_connect_tcp(s, ip_be, port);
            }
            if crate::net::sock_connect(s, ip_be, port) {
                0
            } else {
                -1
            }
        }
        None => -1,
    }
}

fn sys_send(fd: i32, buf: usize, len: usize) -> isize {
    if len == 0 || len > 65536 {
        return -1;
    }
    match sock_idx_of(fd) {
        Some(s) => unsafe {
            let src = crate::fs::user_slice(buf, len);
            crate::net::sock_send(s, src)
        },
        None => -1,
    }
}

fn sys_recv(fd: i32, buf: usize, len: usize) -> isize {
    if len == 0 || len > 65536 {
        return -1;
    }
    match sock_idx_of(fd) {
        Some(s) => unsafe {
            let dst = crate::fs::user_slice_mut(buf, len);
            crate::net::sock_recv(s, dst)
        },
        None => -1,
    }
}

fn sys_dup(fd: i32) -> isize {
    if fd < 0 || fd >= 16 {
        return -1;
    }
    let mut ret: isize = -1;
    crate::task::with_current_mut(|p| {
        if p.fd_kind[fd as usize] == 0 {
            return;
        }
        for i in 0..16 {
            if p.fd_kind[i] == 0 {
                p.fd_kind[i] = p.fd_kind[fd as usize];
                p.fd_off[i] = p.fd_off[fd as usize];
                p.fd_path[i] = p.fd_path[fd as usize];
                p.fds[i] = i as i32;
                // dup() new fd starts with CLOEXEC cleared
                p.fd_cloexec[i] = false;
                ret = i as isize;
                break;
            }
        }
    });
    ret
}

fn sys_pipe(uaddr: usize) -> isize {
    // uaddr: *mut [i32;2]
    if uaddr == 0 {
        return -1;
    }
    let (id, _) = crate::fs::pipe::create();
    let mut rfd: isize = -1;
    let mut wfd: isize = -1;
    crate::task::with_current_mut(|p| {
        for i in 3..16 {
            if p.fd_kind[i] == 0 {
                p.fd_kind[i] = 5;
                p.fd_path[i] = id as u64;
                p.fds[i] = i as i32;
                rfd = i as isize;
                break;
            }
        }
        for i in 3..16 {
            if p.fd_kind[i] == 0 {
                p.fd_kind[i] = 6;
                p.fd_path[i] = id as u64;
                p.fds[i] = i as i32;
                wfd = i as isize;
                break;
            }
        }
    });
    if rfd < 0 || wfd < 0 {
        return -1;
    }
    unsafe {
        let out = uaddr as *mut i32;
        *out.add(0) = rfd as i32;
        *out.add(1) = wfd as i32;
    }
    0
}

fn sys_exec(path_ptr: usize, argv_ptr: usize) -> isize {
    unsafe {
        let raw = match crate::fs::user_str(path_ptr) {
            Some(s) => s,
            None => return -1,
        };
        // copy argv out of (soon replaced) user memory first
        let mut args: Vec<Vec<u8>> = Vec::new();
        if argv_ptr != 0 {
            for i in 0..16 {
                let p = *(argv_ptr as *const usize).add(i);
                if p == 0 {
                    break;
                }
                let mut v = Vec::new();
                for k in 0..128 {
                    let b = *((p as *const u8).add(k));
                    if b == 0 {
                        break;
                    }
                    v.push(b);
                }
                args.push(v);
            }
        }
        let pid = crate::task::current_pid();
        // resolve against caller cwd before address space is replaced
        let path = crate::task::resolve_for(pid, &raw);
        crate::println!("[PROC] exec pid={} -> {} (argc={})", pid, path, args.len());
        if crate::task::exec(pid, &path, &args, &[]) {
            0
        } else {
            -1
        }
    }
}

/// v0.11: execve(path, argv, envp). envp = user array of "K=V" NUL-string
/// pointers, NULL-terminated, max 16 entries x 127 bytes.
fn sys_execve(path_ptr: usize, argv_ptr: usize, envp_ptr: usize) -> isize {
    unsafe {
        let raw = match crate::fs::user_str(path_ptr) {
            Some(s) => s,
            None => return -1,
        };
        let args = copy_strvec(argv_ptr, 16);
        let env = copy_strvec(envp_ptr, 16);
        let pid = crate::task::current_pid();
        let path = crate::task::resolve_for(pid, &raw);
        crate::println!(
            "[PROC] execve pid={} -> {} (argc={} envc={})",
            pid,
            path,
            args.len(),
            env.len()
        );
        if crate::task::exec(pid, &path, &args, &env) {
            0
        } else {
            -1
        }
    }
}

/// Copy a NULL-terminated user array of NUL-terminated strings (cap count).
unsafe fn copy_strvec(arr_ptr: usize, max: usize) -> Vec<Vec<u8>> {
    let mut out: Vec<Vec<u8>> = Vec::new();
    if arr_ptr == 0 {
        return out;
    }
    for i in 0..max {
        let p = *(arr_ptr as *const usize).add(i);
        if p == 0 {
            break;
        }
        let mut v = Vec::new();
        for k in 0..128 {
            let b = *((p as *const u8).add(k));
            if b == 0 {
                break;
            }
            v.push(b);
        }
        out.push(v);
    }
    out
}

fn sys_sleep(ticks: usize) -> isize {
    // Block until target tick; return value is pre-written and delivered
    // on resume, so from user view sleep() waited the full duration.
    let target = crate::timer::ticks().saturating_add(ticks);
    crate::task::block_current(crate::task::BLOCK_SLEEP, target as u64);
    crate::task::yield_now();
    0
}

fn sys_sbrk(inc: i32) -> isize {
    let old = crate::task::with_current(|p| p.brk);
    if inc == 0 {
        return old as isize;
    }
    if inc > 0 {
        let new_brk = old + inc as usize;
        let pid = crate::task::current_pid();
        let root = crate::task::with_current(|p| p.root);
        // v2.2: fallible (cap-hit/OOM returns -1, never panics). Partial
        // mappings stay mapped (harmless: brk is only advanced on success).
        if !crate::mem::alloc_map_user(
            root,
            old,
            inc as usize,
            crate::mem::pagetable::PTE_R | crate::mem::pagetable::PTE_W,
            crate::task::current_cg(),
        ) {
            return -1;
        }
        crate::task::with_current_mut(|p| {
            p.brk = new_brk;
        });
        // need re-activate? same root, new mappings visible after sfence
        unsafe {
            core::arch::asm!("sfence.vma");
        }
        let _ = pid;
    } else {
        // v0.8: shrink: unmap [new, old) and recycle frames; never below brk_min
        let dec = (-(inc as isize)) as usize;
        if dec > old {
            return -1;
        }
        let new_brk = old - dec;
        let brk_min = crate::task::with_current(|p| p.brk_min);
        if new_brk < brk_min {
            return -1;
        }
        let root = crate::task::with_current(|p| p.root);
        crate::mem::unmap_free_user(root, new_brk, dec);
        crate::task::with_current_mut(|p| {
            p.brk = new_brk;
        });
    }
    old as isize
}

/// v0.8: anonymous mmap. hint ignored (always top-down); prot bit0=R,
/// bit1=W (0 => R|W); X never granted. Returns base or -1.
fn sys_mmap(_hint: usize, len: usize, prot: usize) -> isize {
    if len == 0 || len > 64 * 1024 * 1024 {
        return -1;
    }
    if prot & !0x3 != 0 {
        return -1;
    }
    let _flags = if prot == 0 {
        crate::mem::pagetable::PTE_R | crate::mem::pagetable::PTE_W
    } else {
        let mut f = 0;
        if prot & 0x1 != 0 {
            f |= crate::mem::pagetable::PTE_R;
        }
        if prot & 0x2 != 0 {
            f |= crate::mem::pagetable::PTE_W;
        }
        if f == 0 {
            return -1;
        }
        f
    };
    let pages = (len + 0xfff) & !0xfff;
    let pid = crate::task::current_pid();
    let (root, base) = crate::task::with_current(|p| (p.root, p.mmap_base));
    if pages > base {
        return -1;
    }
    // guard against colliding with the heap top (brk): mmap region must
    // stay strictly above brk. Estimate brk via fresh read (same lock order
    // as elsewhere: with_current twice is fine, no nesting).
    let brk = crate::task::with_current(|p| p.brk);
    let new_base = base - pages;
    if new_base < brk {
        return -1;
    }
    // v2.2: fallible (cap-hit/OOM returns -1; frontier untouched on failure).
    if !crate::mem::alloc_map_user(root, new_base, pages, _flags, crate::task::current_cg()) {
        return -1;
    }
    // fresh frames are zeroed by the frame allocator; ensure visibility
    unsafe {
        core::arch::asm!("sfence.vma");
    }
    crate::task::with_current_mut(|p| {
        if p.mmap_base == base {
            p.mmap_base = new_base;
        }
    });
    let _ = pid;
    new_base as isize
}

/// v0.8: munmap(addr, len). Unmaps user pages, recycles frames. Always 0
/// for in-range calls (unmapped/non-U pages skipped); -1 only if the range
/// is absurd (>256MB) to catch wild pointers.
fn sys_munmap(addr: usize, len: usize) -> isize {
    if len == 0 {
        return 0;
    }
    if len > 256 * 1024 * 1024 {
        return -1;
    }
    let root = crate::task::with_current(|p| p.root);
    crate::mem::unmap_free_user(root, addr, len);
    0
}

/// v0.9: ps(buf, len). Writes "pid ppid state brk cwd\n" lines; truncates
/// at line boundary if short; returns bytes written.
/// v2.1: pids are the caller's namespace view (see ps_snapshot).
fn sys_ps(buf: usize, len: usize) -> isize {
    if buf == 0 || len == 0 {
        return -1;
    }
    let snap = crate::task::ps_snapshot(crate::task::current_pid());
    let b = snap.as_bytes();
    // truncate to last full line that fits
    let mut n = b.len().min(len);
    if n < b.len() {
        let mut cut = 0;
        for (i, &c) in b.iter().enumerate() {
            if i >= len {
                break;
            }
            if c == b'\n' {
                cut = i + 1;
            }
        }
        n = cut;
    }
    if n == 0 && !b.is_empty() {
        return -1;
    }
    unsafe {
        let dst = crate::fs::user_slice_mut(buf, len);
        dst[..n].copy_from_slice(&b[..n]);
    }
    n as isize
}

/// v0.9: trace(pid, on). Sets the strace-lite flag. Returns 0/-1.
fn sys_trace(pid: usize, on: usize) -> isize {
    if crate::task::set_traced(pid, on != 0) {
        0
    } else {
        -1
    }
}

fn sys_mkdir(path_ptr: usize) -> isize {
    unsafe {
        match crate::fs::user_str(path_ptr) {
            Some(raw) => {
                let pid = crate::task::current_pid();
                let p = crate::task::resolve_for(pid, &raw);
                if crate::fs::mkdir(&p) {
                    0
                } else {
                    -1
                }
            }
            None => -1,
        }
    }
}

/// v2.0: chroot jail. Resolves `path` under the CURRENT root (so the
/// result is a descendant by construction -- tightening only, no
/// prefix-string games), requires an existing directory, then jails.
/// Already-open fds keep pointing outside (classic chroot limitation,
/// documented in _doc/v2.0.md).
fn sys_chroot(path_ptr: usize) -> isize {
    unsafe {
        match crate::fs::user_str(path_ptr) {
            Some(raw) => {
                let pid = crate::task::current_pid();
                let p = crate::task::resolve_for(pid, &raw);
                if !crate::fs::disk::is_dir(&p) {
                    return -1;
                }
                if crate::task::set_root(pid, &p) {
                    0
                } else {
                    -1
                }
            }
            None => -1,
        }
    }
}

/// v2.1: unshare (pid namespace only). Arms the next forked child to found
/// a new ns; other flags rejected.
fn sys_unshare(flags: usize) -> isize {
    if crate::task::unshare(flags) {
        0
    } else {
        -1
    }
}

/// v2.9: join the pid namespace of a live task (ctr exec). Refuses
/// missing targets and the root ns (joining ns 0 from a container
/// would be an escape hatch; the only in-tree user joins container
/// namespaces from the root ns).
fn sys_nsenter(pid: usize) -> isize {
    if crate::task::nsenter(pid) {
        0
    } else {
        -1
    }
}

/// v3.5: bind host dir `tgt` at `mp` for the caller (both user
/// pointers; resolved in the caller's CURRENT view first, so ctr
/// passes pre-chroot absolute paths). 0 ok, -1 on any failure
/// (missing/non-dir target, bad mountpoint, table full).
fn sys_mountvol(mp_ptr: usize, tgt_ptr: usize) -> isize {
    unsafe {
        let mp_raw = match crate::fs::user_str(mp_ptr) {
            Some(s) => s,
            None => return -1,
        };
        let tgt_raw = match crate::fs::user_str(tgt_ptr) {
            Some(s) => s,
            None => return -1,
        };
        let pid = crate::task::current_pid();
        let mp = crate::task::resolve_for(pid, &mp_raw);
        let tgt = crate::task::resolve_for(pid, &tgt_raw);
        if crate::task::mount_vol(&mp, &tgt) {
            0
        } else {
            -1
        }
    }
}

/// v3.6: read decoded cgroup quotas into 3 user u64s
/// [limit_frames, cpu_pct, weight]. -1 on bogus id or bad pointer
/// (callers print detached-style defaults instead).
fn sys_cgstat(id: usize, out_ptr: usize) -> isize {
    if out_ptr == 0 {
        return -1;
    }
    let mut out = [0u64; 3];
    if !crate::task::cgstat(id, &mut out) {
        return -1;
    }
    unsafe {
        let dst = crate::fs::user_slice_mut(out_ptr, 24);
        if dst.len() < 24 {
            return -1;
        }
        let mut i = 0;
        while i < 3 {
            let b = out[i].to_le_bytes();
            let mut k = 0;
            while k < 8 {
                dst[i * 8 + k] = b[k];
                k += 1;
            }
            i += 1;
        }
    }
    0
}

/// v2.2: create a child cgroup with a frame limit; returns id or -1.
/// (usize::MAX from the task layer maps to -1 here.)
fn sys_cgcreate(limit: u64) -> isize {
    let id = crate::task::cgcreate(limit);
    if id == usize::MAX {
        -1
    } else {
        id as isize
    }
}

/// v2.2: move self into cgroup id.
fn sys_cgenter(id: usize) -> isize {
    if crate::task::cgenter(id) {
        0
    } else {
        -1
    }
}

/// v2.2: set the frame limit of cgroup id (0 = unlimited).
fn sys_cglimit(id: usize, limit: u64) -> isize {
    if crate::task::cgsetlimit(id, limit) {
        0
    } else {
        -1
    }
}

/// v2.3: set the CPU cap percent of cgroup id (0-100).
fn sys_cgsetcpu(id: usize, pct: u64) -> isize {
    if crate::task::cg_set_cpu(id, pct) {
        0
    } else {
        -1
    }
}

/// v2.7: set the CPU share weight of cgroup id (1-1000).
fn sys_cgsetshare(id: usize, weight: u64) -> isize {
    if crate::task::cg_set_share(id, weight) {
        0
    } else {
        -1
    }
}

fn sys_chdir(path_ptr: usize) -> isize {
    unsafe {
        match crate::fs::user_str(path_ptr) {
            Some(raw) => {
                let pid = crate::task::current_pid();
                let p = crate::task::resolve_for(pid, &raw);
                if !crate::fs::exists(&p) {
                    return -1;
                }
                // must be a directory (stat kind: 2=dir; ramfs fallback (2,0,1))
                let (k, _, _) = crate::fs::stat(&p);
                if k != 2 {
                    return -1;
                }
                // v2.0: store the CONTAINER view (strip the jail prefix),
                // so getcwd never leaks the real prefix and relative
                // resolution never double-applies the root.
                let root = crate::task::get_root(pid);
                let tail = p.strip_prefix(&root).unwrap_or("/");
                let view = if tail.is_empty() || !tail.starts_with('/') {
                    alloc::format!("/{}", tail)
                } else {
                    alloc::string::String::from(tail)
                };
                if crate::task::set_cwd(pid, &view) {
                    0
                } else {
                    -1
                }
            }
            None => -1,
        }
    }
}

fn sys_getcwd(buf: usize, len: usize) -> isize {
    if buf == 0 || len == 0 {
        return -1;
    }
    let pid = crate::task::current_pid();
    let cwd = crate::task::get_cwd(pid);
    let b = cwd.as_bytes();
    // need bytes incl. NUL
    if b.len() + 1 > len {
        return -1;
    }
    unsafe {
        let dst = crate::fs::user_slice_mut(buf, len);
        dst[..b.len()].copy_from_slice(b);
        dst[b.len()] = 0;
    }
    (b.len() + 1) as isize
}

fn sys_lseek(fd: i32, off: isize, whence: usize) -> isize {
    if fd < 0 || fd >= 16 {
        return -1;
    }
    // SEEK_SET=0, SEEK_CUR=1, SEEK_END=2
    if whence > 2 {
        return -1;
    }
    let (kind, path_id, cur) = crate::task::with_current(|p| {
        (p.fd_kind[fd as usize], p.fd_path[fd as usize], p.fd_off[fd as usize])
    });
    if kind != 4 {
        return -1;
    }
    let path = fd_path_to_string(path_id);
    let size = crate::fs::file_len(&path).unwrap_or(0) as isize;
    let new_off: isize = match whence {
        0 => off,
        1 => cur as isize + off,
        2 => size + off,
        _ => return -1,
    };
    if new_off < 0 {
        return -1;
    }
    crate::task::with_current_mut(|p| {
        p.fd_off[fd as usize] = new_off as usize;
    });
    new_off
}

fn sys_dup2(old: i32, new: i32) -> isize {
    if old < 0 || old >= 16 || new < 0 || new >= 16 {
        return -1;
    }
    let (kind, off, path) = crate::task::with_current(|p| {
        (p.fd_kind[old as usize], p.fd_off[old as usize], p.fd_path[old as usize])
    });
    if kind == 0 {
        return -1;
    }
    if old == new {
        return new as isize;
    }
    crate::task::with_current_mut(|p| {
        p.fd_kind[new as usize] = kind;
        p.fd_off[new as usize] = off;
        p.fd_path[new as usize] = path;
        p.fds[new as usize] = new;
        // POSIX: dup2 clears CLOEXEC on the new fd
        p.fd_cloexec[new as usize] = false;
    });
    new as isize
}

fn sys_waitpid(target: isize, status_ptr: usize, options: usize) -> isize {
    let pid = crate::task::current_pid();
    let (c, code) = crate::task::waitpid(pid, target, options);
    if c == -2 {
        return -2; // would block, user retries
    }
    if c == 0 {
        return 0; // WNOHANG: no zombie yet
    }
    if c < 0 {
        return -1;
    }
    if status_ptr != 0 {
        unsafe {
            *(status_ptr as *mut i32) = code as i32;
        }
    }
    c as isize
}

fn sys_fsstat(buf: usize, len: usize) -> isize {
    if buf == 0 || len == 0 {
        return -1;
    }
    let (total, free) = crate::fs::blocks_stat();
    let s = alloc::format!("total={} free={}\n", total, free);
    let b = s.as_bytes();
    if b.len() > len {
        return -1;
    }
    unsafe {
        let dst = crate::fs::user_slice_mut(buf, len);
        dst[..b.len()].copy_from_slice(b);
    }
    b.len() as isize
}

fn sys_unlink(path_ptr: usize) -> isize {
    unsafe {
        match crate::fs::user_str(path_ptr) {
            Some(raw) => {
                let pid = crate::task::current_pid();
                let p = crate::task::resolve_for(pid, &raw);
                if crate::fs::unlink(&p) {
                    0
                } else {
                    -1
                }
            }
            None => -1,
        }
    }
}

/// fstat(fd, out: *mut u32[3]) -> 0 ok / -1 err. out = [kind, size, nlink].
/// kind: 1 stdin, 2 stdout/stderr/dir, 4 file, 5/6 pipe.
fn sys_fstat(fd: i32, out: usize) -> isize {
    if out == 0 || fd < 0 || fd >= 16 {
        return -1;
    }
    let (kind, ipath, _) = crate::task::with_current(|p| {
        (
            p.fd_kind[fd as usize],
            p.fd_path[fd as usize],
            p.fd_off[fd as usize],
        )
    });
    let (k, sz, nl) = match kind {
        1 => (1u32, 0u32, 1u32),
        2 => (2u32, 0u32, 1u32),
        4 => {
            let path = fd_path_to_string(ipath);
            let (dk, ds, dn) = crate::fs::stat(&path);
            (if dk == 2 { 2 } else { 4 }, ds, dn)
        }
        5 | 6 => (kind as u32, 0u32, 1u32),
        _ => return -1,
    };
    unsafe {
        let o = out as *mut u32;
        *o.add(0) = k;
        *o.add(1) = sz;
        *o.add(2) = nl;
    }
    0
}

fn sys_link(old_ptr: usize, new_ptr: usize) -> isize {
    unsafe {
        let old_raw = match crate::fs::user_str(old_ptr) {
            Some(s) => s,
            None => return -1,
        };
        let new_raw = match crate::fs::user_str(new_ptr) {
            Some(s) => s,
            None => return -1,
        };
        let pid = crate::task::current_pid();
        let old = crate::task::resolve_for(pid, &old_raw);
        let new = crate::task::resolve_for(pid, &new_raw);
        if crate::fs::link(&old, &new) {
            0
        } else {
            -1
        }
    }
}

/// getdents(path_ptr, buf, len): write NUL-separated names, return count.
fn sys_getdents(path_ptr: usize, buf: usize, len: usize) -> isize {
    if buf == 0 {
        return -1;
    }
    unsafe {
        let raw = match crate::fs::user_str(path_ptr) {
            Some(s) => s,
            None => return -1,
        };
        let pid = crate::task::current_pid();
        let path = crate::task::resolve_for(pid, &raw);
        let names = crate::fs::list_dir(&path);
        let dst = crate::fs::user_slice_mut(buf, len);
        let mut o = 0usize;
        let mut cnt = 0isize;
        for n in names.iter() {
            let b = n.as_bytes();
            if o + b.len() + 1 > len {
                break;
            }
            dst[o..o + b.len()].copy_from_slice(b);
            dst[o + b.len()] = 0;
            o += b.len() + 1;
            cnt += 1;
        }
        cnt
    }
}

// ---- fd path registry (id -> path string) ----
use crate::sync::SpinMutex;
use alloc::string::String;
use alloc::vec::Vec;

static mut REG: Option<SpinMutex<Vec<String>>> = None;

fn reg() -> &'static SpinMutex<Vec<String>> {
    unsafe {
        if REG.is_none() {
            REG = Some(SpinMutex::new(Vec::new()));
        }
        REG.as_ref().unwrap()
    }
}

fn register_path(path: &str) -> u64 {
    let mut r = reg().lock();
    for (i, s) in r.iter().enumerate() {
        if s == path {
            return i as u64;
        }
    }
    r.push(String::from(path));
    (r.len() - 1) as u64
}

fn fd_path_to_string(id: u64) -> String {
    let r = reg().lock();
    r.get(id as usize).cloned().unwrap_or(String::from("/"))
}
