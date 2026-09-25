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

/// v0.11: execve(path, argv, envp). envp = ptr to array of "K=V" NUL-string
/// pointers, NULL-terminated.
pub fn execve(path: *const u8, argv: usize, envp: usize) -> isize {
    ecall(34, path as usize, argv, envp)
}

/// v1.0: current hart id (0..MAX_HART).
pub fn gethart() -> isize {
    ecall(35, 0, 0, 0)
}

/// v0.11: inherited environment, filled by _start from the kernel stack
/// layout (argc/argv/envc/envp). Read-only for the process.
/// no_mangle: referenced by name from _start asm; used: invisible to
/// --gc-sections otherwise.
#[no_mangle]
#[used]
pub static mut ENVIRON_P: usize = 0;
#[no_mangle]
#[used]
pub static mut ENVIRON_C: usize = 0;

pub fn env_count() -> usize {
    unsafe { ENVIRON_C }
}

/// i-th "K=V" entry (NUL-terminated, cap 128). Bounds/NULL safe.
pub unsafe fn env_str(i: usize) -> Option<&'static [u8]> {
    let n = ENVIRON_C;
    if i >= n || n > 16 {
        return None;
    }
    let arr = ENVIRON_P as *const *const u8;
    if arr.is_null() {
        return None;
    }
    let p = *arr.add(i);
    if p.is_null() {
        return None;
    }
    let mut len = 0;
    while len < 128 && *p.add(len) != 0 {
        len += 1;
    }
    Some(core::slice::from_raw_parts(p, len))
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
/// v1.8: monotonic ms since boot (SYS_TIME=44; virt has no RTC).
pub fn time() -> isize {
    ecall(44, 0, 0, 0)
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
/// v2.0: chroot jail (SYS_CHROOT=45). 0 ok, -1 if missing/not-a-dir.
pub fn chroot(path: *const u8) -> isize {
    ecall(45, path as usize, 0, 0)
}
/// v2.1: unshare (SYS_UNSHARE=46). Only CLONE_NEWPID (0x20000000) is
/// accepted: arms the next forked child to found a new pid namespace.
/// Returns 0 or -1.
pub fn unshare(flags: usize) -> isize {
    ecall(46, flags, 0, 0)
}
/// Linux CLONE_NEWPID value (accepted by unshare()).
pub const CLONE_NEWPID: usize = 0x20000000;
/// v2.2: cgroup-lite (SYS_CGCREATE=47, CGENTER=48, CGLIMIT=49).
/// cgcreate(limit_frames) makes a child cgroup with a cap, returns its id
/// (or -1 when full). cgenter(id) moves self. cglimit(id, lim) sets a cap
/// (0 = unlimited). No permission model: existence is the only check.
pub fn cgcreate(limit_frames: usize) -> isize {
    ecall(47, limit_frames, 0, 0)
}
pub fn cgenter(id: isize) -> isize {
    ecall(48, id as usize, 0, 0)
}
pub fn cglimit(id: isize, limit_frames: usize) -> isize {
    ecall(49, id as usize, limit_frames, 0)
}
/// v2.3: CPU cap (SYS_CGSETCPU=50). cgsetcpu(id, pct) caps the cgroup at
/// pct percent of a 100-tick window (0 = run only when nothing else wants
/// the hart). Returns 0 or -1 for bogus ids.
pub fn cgsetcpu(id: isize, pct: usize) -> isize {
    ecall(50, id as usize, pct, 0)
}
/// v2.4: pid liveness + identity (SYS_PIDINFO=51). Returns +(start+2) if
/// the pid slot holds a live task, -(start+2) for an unreaped zombie,
/// -1 if the slot is empty (reaped) or out of range. `start` is the
/// spawn/fork tick; (pid,start) is unique even across reboot-stale pids.
pub fn pidinfo(pid: isize) -> isize {
    ecall(51, pid as usize, 0, 0)
}
/// v2.5: exit code of a reaped pid (SYS_REAPSTAT=52). Returns
/// `code + 0x10000`, or -1 if no record (still alive/zombie, or evicted
/// from the 32-deep ring, or never existed).
pub fn reapstat(pid: isize) -> isize {
    ecall(52, pid as usize, 0, 0)
}
/// v2.6: kill all live tasks in cgroup `cg` (SYS_CGKILL=53). Returns the
/// number marked, or -1 for cg 0 / out-of-range ids. Empty group returns
/// 0 (idempotent; doubles as an emptiness probe).
pub fn cgkill(cg: isize) -> isize {
    ecall(53, cg as usize, 0, 0)
}
/// v2.7: CPU share weight (SYS_CGSETSHARE=54). cgsetshare(id, w) sets the
/// vruntime weight (1-1000; higher = more CPU under contention). Default
/// (never set) behaves as 1. Returns 0 or -1 for bogus ids/weights.
pub fn cgsetshare(id: isize, weight: usize) -> isize {
    ecall(54, id as usize, weight, 0)
}
/// v2.9: join the pid namespace of task `pid` (SYS_NSENTER=55). Keeps the
/// caller's global pid; the in-namespace identity becomes a fresh lpid.
/// Returns 0 or -1 (missing target, or the root ns -- joining ns 0 from
/// a container would be an escape hatch).
pub fn nsenter(pid: isize) -> isize {
    ecall(55, pid as usize, 0, 0)
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

/// v0.8: anonymous mmap/munmap (SYS_MMAP=30, SYS_MUNMAP=31).
/// prot bit0=R, bit1=W (0 => R|W). Returns base address or -1.
pub fn mmap(hint: usize, len: usize, prot: usize) -> isize {
    ecall(30, hint, len, prot)
}
pub fn munmap(addr: usize, len: usize) -> isize {
    ecall(31, addr, len, 0)
}

/// v1.2: memstat (SYS_MEMSTAT=36): returns free frame count (or -1).
pub fn memstat() -> isize {
    ecall(36, 0, 0, 0)
}

/// v1.8: uptime_ms (SYS_TIME=44): monotonic ms since boot. No wall clock
/// (virt has no RTC hardware); good for timeouts, not timestamps.
pub fn uptime_ms() -> isize {
    ecall(44, 0, 0, 0)
}

/// v1.3: UDP sockets (SYS_SOCKET=37 .. SYS_RECV=40). connect takes BE
/// IPv4 u32 + port; send/recv are datagrams on the connected peer.
/// send/recv return -2 (WouldBlock) when ARP is unresolved / no packet;
/// callers retry with their own deadline.
/// v1.5: socket takes kind (0 = UDP, 1 = TCP); bind/listen/accept for TCP.
pub fn socket(kind: usize) -> isize {
    ecall(37, kind, 0, 0)
}
pub fn connect(fd: isize, ip_be: u32, port: u16) -> isize {
    ecall(38, fd as usize, ip_be as usize, port as usize)
}
pub fn send(fd: isize, buf: *const u8, len: usize) -> isize {
    ecall(39, fd as usize, buf as usize, len)
}
pub fn recv(fd: isize, buf: *mut u8, len: usize) -> isize {
    ecall(40, fd as usize, buf as usize, len)
}
pub fn bind(fd: isize, port: u16) -> isize {
    ecall(41, fd as usize, port as usize, 0)
}
pub fn listen(fd: isize) -> isize {
    ecall(42, fd as usize, 0, 0)
}
pub fn accept(fd: isize) -> isize {
    ecall(43, fd as usize, 0, 0)
}

/// v1.7: dotted-decimal "a.b.c.d" -> BE u32. Strict (4 parts, 0-255).
pub fn parse_ip(s: &[u8]) -> Option<u32> {
    let mut parts = [0u32; 4];
    let mut pi = 0usize;
    let mut cur = 0u32;
    let mut digits = 0usize;
    let mut i = 0usize;
    // allow trailing NUL (argv slices are already trimmed, but be safe)
    let n = s.len();
    while i <= n {
        let c = if i < n { s[i] } else { b'.' };
        if c >= b'0' && c <= b'9' {
            cur = cur * 10 + (c - b'0') as u32;
            if cur > 255 {
                return None;
            }
            digits += 1;
            if digits > 3 {
                return None;
            }
        } else if (c == b'.' || c == 0) && digits > 0 {
            if pi >= 4 {
                return None;
            }
            parts[pi] = cur;
            pi += 1;
            cur = 0;
            digits = 0;
            if c == 0 {
                break;
            }
        } else {
            return None;
        }
        i += 1;
    }
    if pi != 4 {
        return None;
    }
    Some((parts[0] << 24) | (parts[1] << 16) | (parts[2] << 8) | parts[3])
}

/// v1.7: build "GET {path} HTTP/1.0\r\nHost: {host}\r\nConnection: close\r\n\r\n"
/// into buf. Returns request length (0 = doesn't fit).
pub fn http_req(buf: &mut [u8], host: &[u8], path: &[u8]) -> usize {
    let parts: [&[u8]; 6] = [b"GET ", path, b" HTTP/1.0\r\nHost: ", host, b"\r\nConnection: close\r\n\r\n", b""];
    let mut n = 0;
    for p in parts.iter() {
        if n + p.len() > buf.len() {
            return 0;
        }
        buf[n..n + p.len()].copy_from_slice(p);
        n += p.len();
    }
    n
}

/// v1.7: split an HTTP response at "\r\n\r\n". Returns (status_code, body_off).
/// status 0 = unparseable.
pub fn http_split(resp: &[u8]) -> (u16, usize) {
    let mut i = 0;
    while i + 4 <= resp.len() {
        if resp[i] == b'\r' && resp[i + 1] == b'\n' && resp[i + 2] == b'\r' && resp[i + 3] == b'\n' {
            let code = http_status(resp);
            return (code, i + 4);
        }
        i += 1;
    }
    (0, 0)
}

fn http_status(resp: &[u8]) -> u16 {
    // "HTTP/1.x NNN ..."
    let mut i = 0;
    while i < resp.len() && resp[i] != b' ' {
        i += 1;
    }
    if i + 4 > resp.len() || resp[i] != b' ' {
        return 0;
    }
    let (a, b, c) = (resp[i + 1], resp[i + 2], resp[i + 3]);
    if !a.is_ascii_digit() || !b.is_ascii_digit() || !c.is_ascii_digit() {
        return 0;
    }
    (a - b'0') as u16 * 100 + (b - b'0') as u16 * 10 + (c - b'0') as u16
}

/// v1.7: DNS A-query resolve via a UDP socket opened internally.
/// name: "example.com" labels (no trailing dot needed); server: BE ip + port.
/// Returns BE IPv4 on success. Retries 3x with ~2s deadlines. Follows one
/// CNAME hop implicitly by taking the first A record (standard single-
/// question responses carry the chain then the answer).
pub fn dns_resolve(name: &[u8], server_ip: u32, server_port: u16) -> Option<u32> {
    let fd = socket(0);
    if fd < 0 {
        return None;
    }
    let r = dns_resolve_on(fd, name, server_ip, server_port);
    close(fd);
    r
}

fn dns_resolve_on(fd: isize, name: &[u8], server_ip: u32, server_port: u16) -> Option<u32> {
    if connect(fd, server_ip, server_port) != 0 {
        return None;
    }
    // build query: id + flags(RD) + qd=1 + an/ns/ar=0, QNAME, QTYPE=A, QCLASS=IN
    let mut q = [0u8; 300];
    if name.len() + 16 > q.len() {
        return None;
    }
    q[0] = 0x12;
    q[1] = 0x34;
    q[2] = 0x01;
    q[3] = 0x00;
    q[4] = 0x00;
    q[5] = 0x01;
    // qdcount already 1; ancount/nscount/arcount zero
    let mut n = 12;
    let mut li = 0;
    // encode labels; tolerate one trailing dot
    let mut end = name.len();
    while end > 0 && name[end - 1] == b'.' {
        end -= 1;
    }
    let mut i = 0;
    while i < end {
        let mut j = i;
        while j < end && name[j] != b'.' {
            j += 1;
        }
        let lab = j - i;
        if lab == 0 || lab > 63 || n + 1 + lab + 5 > q.len() {
            return None;
        }
        q[n] = lab as u8;
        n += 1;
        q[n..n + lab].copy_from_slice(&name[i..j]);
        n += lab;
        li += 1;
        if li > 8 {
            return None;
        }
        i = if j < end { j + 1 } else { j };
    }
    q[n] = 0;
    n += 1;
    q[n] = 0;
    q[n + 1] = 1; // QTYPE A
    q[n + 2] = 0;
    q[n + 3] = 1; // QCLASS IN
    n += 4;
    let mut attempt = 0;
    while attempt < 3 {
        // (re)send; WouldBlock (ARP) just retries inside the deadline
        let mut s = 0;
        let mut sent = false;
        while s < 200 {
            let r = send(fd, q.as_ptr(), n);
            if r == n as isize {
                sent = true;
                break;
            }
            if r >= 0 || r != -2 {
                break;
            }
            sleep(1);
            s += 1;
        }
        if !sent {
            attempt += 1;
            continue;
        }
        // wait reply
        let mut rb = [0u8; 512];
        let mut w = 0;
        while w < 200 {
            let r = recv(fd, rb.as_mut_ptr(), rb.len());
            if r > 0 {
                if let Some(ip) = dns_parse_reply(&rb[..r as usize]) {
                    return Some(ip);
                }
                break; // got something unparseable: next attempt
            }
            if r != -2 {
                break;
            }
            sleep(1);
            w += 1;
        }
        attempt += 1;
    }
    None
}

/// Parse a DNS response; first A/IN rdata wins (CNAME chains resolve to
/// the A that follows them in single-question responses).
fn dns_parse_reply(p: &[u8]) -> Option<u32> {
    if p.len() < 12 {
        return None;
    }
    if p[2] & 0x80 == 0 {
        return None; // not a response
    }
    if p[3] & 0x0f != 0 {
        return None; // RCODE != 0
    }
    let qd = ((p[4] as usize) << 8) | p[5] as usize;
    let an = ((p[6] as usize) << 8) | p[7] as usize;
    if qd != 1 || an == 0 {
        return None;
    }
    // skip question section
    let mut o = 12;
    o = dns_skip_name(p, o)?;
    if o + 4 > p.len() {
        return None;
    }
    o += 4; // QTYPE + QCLASS
    // scan answers
    let mut ai = 0;
    while ai < an {
        o = dns_skip_name(p, o)?;
        if o + 10 > p.len() {
            return None;
        }
        let typ = ((p[o] as usize) << 8) | p[o + 1] as usize;
        let cls = ((p[o + 2] as usize) << 8) | p[o + 3] as usize;
        let rdlen = ((p[o + 8] as usize) << 8) | p[o + 9] as usize;
        o += 10;
        if o + rdlen > p.len() {
            return None;
        }
        if typ == 1 && cls == 1 && rdlen == 4 {
            return Some(
                ((p[o] as u32) << 24) | ((p[o + 1] as u32) << 16) | ((p[o + 2] as u32) << 8) | p[o + 3] as u32,
            );
        }
        o += rdlen;
        ai += 1;
        if ai > 16 {
            return None;
        }
    }
    None
}

/// Skip a (possibly compressed) domain name; returns offset past it.
fn dns_skip_name(p: &[u8], mut o: usize) -> Option<usize> {
    let mut jumps = 0;
    loop {
        if o >= p.len() {
            return None;
        }
        let b = p[o];
        if b & 0xc0 == 0xc0 {
            // pointer: 2 bytes, terminates this name
            if o + 1 >= p.len() {
                return None;
            }
            return Some(o + 2);
        }
        if b == 0 {
            return Some(o + 1);
        }
        if (b as usize) > 63 || o + 1 + (b as usize) > p.len() {
            return None;
        }
        o += 1 + (b as usize);
        jumps += 1;
        if jumps > 16 {
            return None;
        }
    }
}

/// v0.9: ps(buf, len) fills "pid ppid state brk cwd\n" lines; trace(pid, on).
pub fn ps(buf: *mut u8, len: usize) -> isize {
    ecall(32, buf as usize, len, 0)
}
pub fn trace(pid: isize, on: usize) -> isize {
    ecall(33, pid as usize, on, 0)
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
