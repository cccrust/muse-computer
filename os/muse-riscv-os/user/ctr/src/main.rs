#![no_std]
#![no_main]
use core::arch::global_asm;
// NOTE: keep in sync with the other user apps (v0.11 env capture).
global_asm!(r#".section .text.entry
.globl _start
_start:
    ld a0, 0(sp)
    addi a1, sp, 8
    slli t0, a0, 3
    addi t0, t0, 16
    add t1, a1, t0
    la t2, ENVIRON_P
    sd t1, 0(t2)
    ld t0, -8(t1)
    la t2, ENVIRON_C
    sd t0, 0(t2)
    call main
    li a0, 0
    li a7, 2
    ecall
"#);
#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    user_lib::exit(-1);
    loop {}
}

// v2.0 container rootfs assembler: `ctr <name>` creates /ctr/<name>/bin
// and hardlinks every /bin entry into it (zero-copy: links share inodes).
// Idempotent (existing dirs/files are skipped, never fails on rerun).
// v2.3: + `ctr run <name> <prog> [args...]` (one-shot container run) and
// + `ctr pull <host> <port> <image>` (manifest + ustar layers from a
// registry stub, unpacked into /ctr/<image>). See _doc/v2.3.md.
// v2.4: + `ctr run -d` (detached: state file + `detached <pid>`) and
// + `ctr ps/stop/rm` (lifecycle over /ctr/*/.pid state files +
// SYS_PIDINFO liveness). See _doc/v2.4.md.
fn mkdir_p(path: &[u8]) {
    // path is a NUL-terminated stack buffer built by caller
    if user_lib::mkdir(path.as_ptr()) != 0 {
        // exists already is fine; other errors surface at link time
    }
}

/// copy src into dst with NUL terminator; returns bytes copied (excl. NUL).
fn nul_copy(dst: &mut [u8], src: &[u8]) -> usize {
    let m = src.len().min(dst.len() - 1);
    dst[..m].copy_from_slice(&src[..m]);
    dst[m] = 0;
    m
}

/// image/container name rules (shared by all subcommands): <24 chars,
/// no `/`, no `..` (no path tricks out of /ctr).
fn valid_name(name: &[u8]) -> bool {
    if name.is_empty() || name.len() >= 24 {
        return false;
    }
    for &c in name.iter() {
        if c == b'/' || c == 0 {
            return false;
        }
    }
    let mut i = 0;
    while i + 1 < name.len() {
        if name[i] == b'.' && name[i + 1] == b'.' {
            return false;
        }
        i += 1;
    }
    true
}

/// wait for a specific child; returns its exit code, or -1 on reap error.
fn wait_for(pid: isize) -> i32 {
    let mut code: i32 = -1;
    loop {
        let w = user_lib::waitpid(pid, &mut code as *mut i32, 0);
        if w == -2 {
            user_lib::yield_();
            continue;
        }
        if w != pid {
            return -1;
        }
        return code;
    }
}

// ---- v2.0 legacy: `ctr <name>` ----
fn cmd_assemble(name: &[u8]) {
    // /ctr/<name>/bin
    let mut base = [0u8; 64];
    base[..5].copy_from_slice(b"/ctr/");
    base[5..5 + name.len()].copy_from_slice(name);
    let basen = 5 + name.len();
    let mut root = [0u8; 64];
    root[..basen].copy_from_slice(&base[..basen]);
    mkdir_p(b"/ctr\0");
    mkdir_p(&root);
    let mut bindir = [0u8; 64];
    bindir[..basen].copy_from_slice(&base[..basen]);
    bindir[basen..basen + 4].copy_from_slice(b"/bin");
    mkdir_p(&bindir);
    // link every /bin/<f> -> <root>/bin/<f>
    let mut nb = [0u8; 512];
    let r = user_lib::getdents(b"/bin\0".as_ptr(), nb.as_mut_ptr(), 512);
    if r <= 0 {
        user_lib::print("ctr: empty /bin\n");
        user_lib::exit(1);
    }
    let mut off = 0usize;
    let mut n = 0;
    // v2.4: `r` is an ENTRY COUNT, not a byte length (pre-v2.4 code
    // compared the byte offset `off` against it, silently linking only
    // the first few entries -- more than a day of debugging: sleeper
    // is 22nd, echo 5th just squeaked through, which is why only the
    // new binary ever failed). Consume exactly r entries.
    let mut seen = 0isize;
    while seen < r && off < 511 {
        // NUL-terminated name at nb[off..]
        let mut len = 0;
        while off + len < 511 && nb[off + len] != 0 {
            len += 1;
        }
        if len == 0 || len > 28 {
            break;
        }
        let mut src = [0u8; 64];
        src[..5].copy_from_slice(b"/bin/");
        src[5..5 + len].copy_from_slice(&nb[off..off + len]);
        let mut dst = [0u8; 64];
        dst[..basen + 4].copy_from_slice(&bindir[..basen + 4]);
        dst[basen + 4] = b'/';
        dst[basen + 5..basen + 5 + len].copy_from_slice(&nb[off..off + len]);
        // exists already -> skip (idempotent reruns)
        user_lib::link(src.as_ptr(), dst.as_ptr());
        n += 1;
        off += len + 1;
        seen += 1;
        if n >= 32 {
            break;
        }
    }
    user_lib::print("ready ");
    let _ = user_lib::write(1, base.as_ptr(), basen);
    user_lib::print("\n");
    user_lib::exit(0);
}

// ---- v2.3: `ctr run <name> <prog> [args...]` ----
// v2.4: `detached` (from `run -d`) records /ctr/<name>/.pid and returns
// immediately; the container is reparented to init on our exit.
/// v2.9: resolve prog + build exec argv (argv[0] = prog as typed).
/// Shared by run and exec. Cap 16 matches the kernel argv limit
/// (see the v2.8 argv lesson -- never truncate silently below it).
/// prog resolution mirrors the shell: contains `/` -> as-is (jailed by
/// chroot after), bare name -> /bin/<prog>.
fn build_argv(
    prog: &[u8],
    args: &[&[u8]],
    progpath: &mut [u8; 64],
    toks: &mut [[u8; 64]; 16],
    av: &mut [*const u8; 17],
) -> usize {
    let mut slash = false;
    for &c in prog.iter() {
        if c == b'/' {
            slash = true;
            break;
        }
    }
    if slash {
        if prog.len() > 62 {
            user_lib::print("ctr: prog too long\n");
            user_lib::exit(1);
        }
        nul_copy(progpath, prog);
    } else {
        if prog.len() > 57 {
            user_lib::print("ctr: prog too long\n");
            user_lib::exit(1);
        }
        progpath[..5].copy_from_slice(b"/bin/");
        progpath[5..5 + prog.len()].copy_from_slice(prog);
        progpath[5 + prog.len()] = 0;
    }
    nul_copy(&mut toks[0], prog);
    let mut n = 1usize;
    for &a in args {
        if n >= 16 {
            break;
        }
        nul_copy(&mut toks[n], &a[..a.len().min(63)]);
        n += 1;
    }
    let mut i = 0;
    while i < n {
        av[i] = toks[i].as_ptr();
        i += 1;
    }
    av[n] = core::ptr::null();
    n
}

/// v2.9: build `/ctr/<name>/log` (NUL-terminated) into `out`.
/// (Caller guarantees a valid name: 5 + len + 4 + 1 fits easily.)
fn log_path(name: &[u8], out: &mut [u8; 64]) {
    out[..5].copy_from_slice(b"/ctr/");
    out[5..5 + name.len()].copy_from_slice(name);
    let mut n = 5 + name.len();
    out[n..n + 4].copy_from_slice(b"/log");
    n += 4;
    out[n] = 0;
}
/// v2.8: quota flags for `run` (all optional, all default-off).
pub struct Quota {
    pub mem_frames: usize, // 0 = unlimited (skip cglimit)
    pub mem_set: bool,
    pub cpu_pct: usize, // default 100 (skip cgsetcpu unless set)
    pub cpu_set: bool,
    pub weight: usize, // default 1 (skip cgsetshare unless set)
    pub weight_set: bool,
}

fn cmd_run(name: &[u8], prog: &[u8], args: &[&[u8]], detached: bool, q: &Quota, vols: &[VolBind], nvols: usize) {
    // root must exist (pull first; no implicit magic)
    let mut root = [0u8; 64];
    root[..5].copy_from_slice(b"/ctr/");
    root[5..5 + name.len()].copy_from_slice(name);
    root[5 + name.len()] = 0;
    let mut probe = [0u8; 64];
    if user_lib::getdents(root.as_ptr(), probe.as_mut_ptr(), 64) < 0 {
        user_lib::print("ctr: no such image (pull first)\n");
        user_lib::exit(1);
    }
    // prog resolution + exec argv (shared with exec, v2.9).
    let mut progpath = [0u8; 64];
    let mut toks = [[0u8; 64]; 16];
    let mut av: [*const u8; 17] = [core::ptr::null(); 17];
    build_argv(prog, args, &mut progpath, &mut toks, &mut av);
    // v2.9: detached output lands in /ctr/<name>/log (opened here, on
    // the host side before chroot; fork-inherited, dup2ed after the
    // jail is up). Open precedes cgcreate so a failure exits clean
    // with no residue. Foreground runs open nothing (stdio inherited).
    let mut logfd = -1isize;
    if detached {
        let mut lp = [0u8; 64];
        log_path(name, &mut lp);
        logfd = user_lib::open(lp.as_ptr(), user_lib::O_CREATE | user_lib::O_TRUNC);
        if logfd < 0 {
            user_lib::print("ctr: log open failed\n");
            user_lib::exit(1);
        }
    }
    // v2.6: private cgroup for fate-sharing (see _doc/v2.6.md §1).
    // Created before fork so the child enters it as its first act.
    // One slot per run (256/boot bound, documented); failure is loud.
    let ccg = user_lib::cgcreate(0);
    if ccg < 0 {
        user_lib::print("ctr: cgcreate failed\n");
        user_lib::exit(1);
    }
    let ccg = ccg as usize;
    // v2.8: apply requested quotas (each checked; any failure aborts
    // the run -- requested==effective is what the `detached` line
    // prints, see _doc/v2.8.md §1).
    if q.mem_set && user_lib::cglimit(ccg as isize, q.mem_frames) != 0 {
        user_lib::print("ctr: cglimit failed\n");
        user_lib::exit(1);
    }
    if q.cpu_set && user_lib::cgsetcpu(ccg as isize, q.cpu_pct) != 0 {
        user_lib::print("ctr: cgsetcpu failed\n");
        user_lib::exit(1);
    }
    if q.weight_set && user_lib::cgsetshare(ccg as isize, q.weight) != 0 {
        user_lib::print("ctr: cgsetshare failed\n");
        user_lib::exit(1);
    }
    // new pid namespace (child becomes pid 1 there, v2.1 semantics),
    // then fork: child jails itself, parent reaps + forwards the code.
    if user_lib::unshare(user_lib::CLONE_NEWPID) != 0 {
        user_lib::print("ctr: unshare failed\n");
        user_lib::exit(1);
    }
    let pid = user_lib::fork();
    if pid == 0 {
        if user_lib::cgenter(ccg as isize) != 0 {
            user_lib::print("ctr: cgenter failed\n");
            user_lib::exit(127);
        }
        // v3.5: volume binds (host view: <root><cpath> <- /vol/<v>),
        // before chroot (the jail step needs them in place).
        let mut vi = 0usize;
        while vi < nvols {
            // mountpoint = root + cpath (both NUL-terminated parts).
            let mut rlen = 0;
            while rlen < 64 && root[rlen] != 0 {
                rlen += 1;
            }
            let mut clen = 0;
            while clen < 64 && vols[vi].cpath[clen] != 0 {
                clen += 1;
            }
            let mut vlen = 0;
            while vlen < 64 && vols[vi].vol[vlen] != 0 {
                vlen += 1;
            }
            if rlen + clen >= 63 || vlen == 0 {
                user_lib::print("ctr: vol mount failed\n");
                user_lib::exit(127);
            }
            let mut mp = [0u8; 64];
            mp[..rlen].copy_from_slice(&root[..rlen]);
            mp[rlen..rlen + clen].copy_from_slice(&vols[vi].cpath[..clen]);
            let mut vp = [0u8; 64];
            vp[..5].copy_from_slice(b"/vol/");
            vp[5..5 + vlen].copy_from_slice(&vols[vi].vol[..vlen]);
            if user_lib::mount_vol(mp.as_ptr(), vp.as_ptr()) != 0 {
                user_lib::print("ctr: vol mount failed\n");
                user_lib::exit(127);
            }
            vi += 1;
        }
        if user_lib::chroot(root.as_ptr()) != 0 {
            user_lib::print("ctr: chroot failed\n");
            user_lib::exit(127);
        }
        if user_lib::chdir(b"/\0".as_ptr()) != 0 {
            user_lib::print("ctr: chdir failed\n");
            user_lib::exit(127);
        }
        // v2.9: detached stdio goes to the log file (post-jail dup2;
        // the fd was opened pre-chroot and inherited across fork).
        // From here on even our own errors land in the log (docker-like).
        if logfd >= 0 {
            // (dup2 returns the new fd on success, -1 on failure.)
            if user_lib::dup2(logfd, 1) < 0 || user_lib::dup2(logfd, 2) < 0 {
                user_lib::print("ctr: log redirect failed\n");
                user_lib::exit(127);
            }
            user_lib::close(logfd);
        }
        let _ = user_lib::exec(progpath.as_ptr(), av.as_ptr() as usize);
        user_lib::print("ctr: exec failed\n");
        user_lib::exit(127);
    } else if pid > 0 {
        // v2.9: the parent's copy of the log fd is done (the child
        // holds its own across fork).
        if logfd >= 0 {
            user_lib::close(logfd);
        }
        if detached {
            // v2.4: learn the child's start tick for the state file
            // (SYS_PIDINFO, +2 biased; must be > 0 here).
            let info = user_lib::pidinfo(pid);
            if info <= 0 {
                user_lib::kill(pid);
                user_lib::print("ctr: detached start failed\n");
                user_lib::exit(1);
            }
            let start = (info - 2) as usize;
            if !write_state(name, pid as usize, start, ccg) {
                // hygiene: never leave an orphan we cannot track.
                user_lib::kill(pid);
                user_lib::print("ctr: detached state failed\n");
                user_lib::exit(1);
            }
            // v2.8: echo applied quotas (defaults for unset: mem=0
            // unlimited, cpu=100, weight=1). Requested==effective:
            // any failed set above already aborted the run.
            // v3.8: single write (no interleave): the forked child
            // exec-prints concurrently on another hart, and the console
            // lock only covers one write() at a time -- per-byte dbg_num
            // splits mid-line (`cpu=` + `[PROC]` + `50 weight=8` across
            // two lines), breaking the suite's whole-line greps.
            let mut db = [0u8; 96];
            let mut dn = 0usize;
            dn = buf_put(&mut db, dn, b"detached ");
            dn = push_dec(&mut db, dn, pid as usize);
            dn = buf_put(&mut db, dn, b" mem=");
            dn = push_dec(&mut db, dn, if q.mem_set { q.mem_frames } else { 0 });
            dn = buf_put(&mut db, dn, b" cpu=");
            dn = push_dec(&mut db, dn, if q.cpu_set { q.cpu_pct } else { 100 });
            dn = buf_put(&mut db, dn, b" weight=");
            dn = push_dec(&mut db, dn, if q.weight_set { q.weight } else { 1 });
            dn = buf_put(&mut db, dn, b"\n");
            let _ = user_lib::write(1, db.as_ptr(), dn);
            user_lib::exit(0);
        }
        // stdio inherited; no setfg (fg job control stays shell-side).
        let code = wait_for(pid);
        if code < 0 {
            user_lib::exit(1);
        }
        user_lib::exit(code);
    } else {
        user_lib::print("ctr: fork failed\n");
        user_lib::exit(1);
    }
}

// ---- v2.3: `ctr pull <host> <port> <image>` ----

/// v3.4: read /pkg/token (first line, CR/LF-trimmed) into `out`.
/// Returns length (0 = absent). Written by `ctr login`.
fn read_token(out: &mut [u8; 64]) -> usize {
    let f = user_lib::open(b"/pkg/token\0".as_ptr(), 0);
    if f < 0 {
        return 0;
    }
    let mut n = 0usize;
    loop {
        if n >= out.len() {
            break;
        }
        let r = user_lib::read(f, unsafe { out.as_mut_ptr().add(n) }, out.len() - n);
        if r <= 0 {
            break;
        }
        n += r as usize;
    }
    user_lib::close(f);
    let mut e = 0;
    while e < n && out[e] != b'\n' && out[e] != b'\r' {
        e += 1;
    }
    e
}

/// run `/bin/wget <host> <port> <path> <outfile>`; true iff exit code 0.
/// v3.4: attaches /pkg/token as a bearer header when present
/// (registry private routes; public routes ignore it -- pull and all
/// pre-login fetches behave bit-for-bit as before).
fn run_wget(host: &[u8], port: &[u8], path: &[u8], out: &[u8]) -> bool {
    let mut hb = [0u8; 64];
    nul_copy(&mut hb, &host[..host.len().min(63)]);
    let mut pb = [0u8; 16];
    nul_copy(&mut pb, &port[..port.len().min(15)]);
    let mut qb = [0u8; 128];
    nul_copy(&mut qb, &path[..path.len().min(127)]);
    let mut ob = [0u8; 32];
    nul_copy(&mut ob, &out[..out.len().min(31)]);
    let mut w0 = [0u8; 8];
    w0[..4].copy_from_slice(b"wget");
    // token: first line of /pkg/token (login-validated charset).
    // Absent token leaves av[5] NULL (argc 5, pre-v3.4 behavior).
    let mut tb = [0u8; 64];
    let tn = read_token(&mut tb);
    // NUL-terminate for argv (wget reads a C string; length capped).
    tb[tn.min(63)] = 0;
    let mut av: [*const u8; 7] = [
        w0.as_ptr(),
        hb.as_ptr(),
        pb.as_ptr(),
        qb.as_ptr(),
        ob.as_ptr(),
        tb.as_ptr(),
        core::ptr::null(),
    ];
    if tn == 0 {
        av[5] = core::ptr::null();
    }
    let mut wget = [0u8; 16];
    wget[..9].copy_from_slice(b"/bin/wget");
    let pid = user_lib::fork();
    if pid == 0 {
        let _ = user_lib::exec(wget.as_ptr(), av.as_ptr() as usize);
        user_lib::exit(-1);
    } else if pid > 0 {
        return wait_for(pid) == 0;
    }
    false
}

fn is_zero_block(blk: &[u8; 512]) -> bool {
    let mut i = 0;
    while i < 512 {
        if blk[i] != 0 {
            return false;
        }
        i += 1;
    }
    true
}

/// octal size field (NUL/space padded); None on garbage.
fn parse_octal(b: &[u8]) -> Option<usize> {
    let mut i = 0;
    while i < b.len() && (b[i] == 0 || b[i] == b' ') {
        i += 1;
    }
    let mut v = 0usize;
    let mut nd = 0;
    while i < b.len() && b[i] >= b'0' && b[i] <= b'7' {
        v = v.checked_mul(8)?.checked_add((b[i] - b'0') as usize)?;
        i += 1;
        nd += 1;
    }
    if nd == 0 {
        return None;
    }
    Some(v)
}

fn read_block(fd: isize, blk: &mut [u8; 512]) -> bool {
    let mut o = 0;
    while o < 512 {
        let r = user_lib::read(fd, unsafe { blk.as_mut_ptr().add(o) }, 512 - o);
        if r <= 0 {
            return false;
        }
        o += r as usize;
    }
    true
}

/// mkdir -p <root>/<rel> (rel may be a file path: only parents are made).
/// Returns false if the final path would escape or overflow.
fn mkdir_parents(root: &[u8], rootn: usize, rel: &[u8], is_dir: bool) -> bool {
    // rel safety: relative only, no `..` components.
    if rel.is_empty() || rel[0] == b'/' {
        return false;
    }
    let mut i = 0;
    while i < rel.len() {
        let mut j = i;
        while j < rel.len() && rel[j] != b'/' {
            j += 1;
        }
        if &rel[i..j] == b".." {
            return false;
        }
        let last = j >= rel.len();
        if !last || is_dir {
            // mkdir root/rel[..j]
            let mut p = [0u8; 128];
            if rootn + 1 + j > 127 {
                return false;
            }
            p[..rootn].copy_from_slice(&root[..rootn]);
            p[rootn] = b'/';
            p[rootn + 1..rootn + 1 + j].copy_from_slice(&rel[..j]);
            user_lib::mkdir(p.as_ptr());
        }
        i = j + 1;
    }
    true
}

fn skip_data(fd: isize, size: usize) -> bool {
    let mut left = (size + 511) / 512;
    let mut blk = [0u8; 512];
    while left > 0 {
        if !read_block(fd, &mut blk) {
            return false;
        }
        left -= 1;
    }
    true
}

/// decimal print for untar diagnostics (no_std, no formatting).
fn dbg_num(v: usize) {
    let mut tmp = [0u8; 20];
    let mut n = 0usize;
    let mut x = v;
    if x == 0 {
        tmp[0] = b'0';
        n = 1;
    } else {
        while x > 0 && n < 20 {
            tmp[n] = b'0' + (x % 10) as u8;
            x /= 10;
            n += 1;
        }
    }
    let mut i = 0;
    while i < n {
        let _ = user_lib::write(1, unsafe { tmp.as_ptr().add(n - 1 - i) }, 1);
        i += 1;
    }
}

/// unpack a ustar stream into root; returns #regular files, or -1 on error.
/// Only regular files (`0`/`\0`) and dirs (`5`) are materialized; other
/// typeflags are skipped by size. Later layers overwrite (plain write).
fn untar(fd: isize, root: &[u8], rootn: usize) -> isize {
    let mut blk = [0u8; 512];
    let mut files = 0isize;
    let mut nblk = 0usize; // diagnostic: tar-stream block index
    loop {
        if !read_block(fd, &mut blk) {
            break; // EOF at a header boundary: clean end
        }
        if is_zero_block(&blk) {
            break; // end-of-archive marker
        }
        // ustar magic (tarfile USTAR_FORMAT); garbage fails the pull.
        if !(blk[257] == b'u'
            && blk[258] == b's'
            && blk[259] == b't'
            && blk[260] == b'a'
            && blk[261] == b'r')
        {
            user_lib::print("[TEST] img FAIL (untar:magic blk=");
            dbg_num(nblk);
            user_lib::print(" tf=");
            dbg_num(blk[156] as usize);
            user_lib::print(")\n");
            return -1;
        }
        let mut nlen = 0;
        while nlen < 100 && blk[nlen] != 0 {
            nlen += 1;
        }
        if nlen == 0 || nlen > 90 {
            user_lib::print("[TEST] img FAIL (untar:name blk=");
            dbg_num(nblk);
            user_lib::print(")\n");
            return -1;
        }
        let size = match parse_octal(&blk[124..136]) {
            Some(v) => v,
            None => {
                user_lib::print("[TEST] img FAIL (untar:octal blk=");
                dbg_num(nblk);
                user_lib::print(")\n");
                return -1;
            }
        };
        if size > (1 << 20) {
            user_lib::print("[TEST] img FAIL (untar:big blk=");
            dbg_num(nblk);
            user_lib::print(" size=");
            dbg_num(size);
            user_lib::print(")\n");
            return -1; // corrupt-size guard (guest ELFs are tens of KB)
        }
        let name = &blk[..nlen];
        let tf = blk[156];
        if tf == b'5' {
            if !mkdir_parents(root, rootn, name, true) {
                user_lib::print("[TEST] img FAIL (untar:mkdir blk=");
                dbg_num(nblk);
                user_lib::print(")\n");
                return -1;
            }
            if !skip_data(fd, size) {
                user_lib::print("[TEST] img FAIL (untar:skipdir blk=");
                dbg_num(nblk);
                user_lib::print(")\n");
                return -1;
            }
            nblk += 1 + (size + 511) / 512;
        } else if tf == b'0' || tf == 0 {
            if !mkdir_parents(root, rootn, name, false) {
                // unsafe path: skip its data, keep the pull alive
                if !skip_data(fd, size) {
                    user_lib::print("[TEST] img FAIL (untar:skipunsafe blk=");
                    dbg_num(nblk);
                    user_lib::print(")\n");
                    return -1;
                }
                nblk += 1 + (size + 511) / 512;
                continue;
            }
            let mut p = [0u8; 128];
            p[..rootn].copy_from_slice(&root[..rootn]);
            p[rootn] = b'/';
            p[rootn + 1..rootn + 1 + nlen].copy_from_slice(name);
            let f = user_lib::open(p.as_ptr(), user_lib::O_CREATE | user_lib::O_TRUNC);
            if f < 0 {
                user_lib::print("[TEST] img FAIL (untar:open blk=");
                dbg_num(nblk);
                user_lib::print(")\n");
                return -1;
            }
            let mut left = size;
            let mut ok = true;
            while left > 0 {
                if !read_block(fd, &mut blk) {
                    user_lib::print("[TEST] img FAIL (untar:data blk=");
                    dbg_num(nblk);
                    user_lib::print(" left=");
                    dbg_num(left);
                    user_lib::print(")\n");
                    ok = false;
                    break;
                }
                nblk += 1;
                let take = left.min(512);
                let mut w = 0;
                while w < take {
                    let r = user_lib::write(f, unsafe { blk.as_ptr().add(w) }, take - w);
                    if r <= 0 {
                        user_lib::print("[TEST] img FAIL (untar:write blk=");
                        dbg_num(nblk);
                        user_lib::print(")\n");
                        ok = false;
                        break;
                    }
                    w += r as usize;
                }
                if !ok {
                    break;
                }
                left -= take;
            }
            user_lib::close(f);
            if !ok {
                return -1;
            }
            files += 1;
            nblk += 1; // header block (data blocks counted above)
        } else {
            // unknown typeflag (x/g/L/K...): skip by size, stay in sync.
            if !skip_data(fd, size) {
                user_lib::print("[TEST] img FAIL (untar:skiptf blk=");
                dbg_num(nblk);
                user_lib::print(")\n");
                return -1;
            }
            nblk += 1 + (size + 511) / 512;
        }
    }
    files
}

fn cmd_pull(host: &[u8], port: &[u8], image: &[u8]) {
    mkdir_p(b"/tmp\0");
    mkdir_p(b"/ctr\0");
    let mut root = [0u8; 64];
    root[..5].copy_from_slice(b"/ctr/");
    root[5..5 + image.len()].copy_from_slice(image);
    let rootn = 5 + image.len();
    mkdir_p(&root);
    // 1. manifest -> /tmp/manifest
    let mut mpath = [0u8; 128];
    mpath[0] = b'/';
    mpath[1..1 + image.len()].copy_from_slice(image);
    let mpn = 1 + image.len();
    mpath[mpn..mpn + 9].copy_from_slice(b"/manifest");
    if !run_wget(host, port, &mpath[..mpn + 9], b"/tmp/manifest") {
        user_lib::print("[TEST] img FAIL (manifest)\n");
        user_lib::exit(1);
    }
    // 2. parse manifest: one layer filename per line, `#` comments.
    let mf = user_lib::open(b"/tmp/manifest\0".as_ptr(), 0);
    if mf < 0 {
        user_lib::print("[TEST] img FAIL (manifest-open)\n");
        user_lib::exit(1);
    }
    let mut mb = [0u8; 2048];
    let mut mn = 0usize;
    loop {
        if mn >= mb.len() {
            break;
        }
        let r = user_lib::read(mf, unsafe { mb.as_mut_ptr().add(mn) }, mb.len() - mn);
        if r <= 0 {
            break;
        }
        mn += r as usize;
    }
    user_lib::close(mf);
    let mut layers = [[0u8; 64]; 8];
    let mut layern = [0usize; 8];
    let mut nl = 0usize;
    let mut i = 0usize;
    while i < mn {
        let mut j = i;
        while j < mn && mb[j] != b'\n' {
            j += 1;
        }
        let mut e = j;
        if e > i && mb[e - 1] == b'\r' {
            e -= 1;
        }
        let line = &mb[i..e];
        if !line.is_empty() && line[0] != b'#' && nl < 8 {
            let m = line.len().min(63);
            layers[nl][..m].copy_from_slice(&line[..m]);
            layern[nl] = m;
            nl += 1;
        }
        i = j + 1;
    }
    if nl == 0 {
        user_lib::print("[TEST] img FAIL (no-layers)\n");
        user_lib::exit(1);
    }
    // 3. fetch + unpack each layer in order (later layers overwrite).
    let mut files = 0isize;
    let mut li = 0usize;
    while li < nl {
        let layer = &layers[li][..layern[li]];
        let mut rpath = [0u8; 128];
        rpath[0] = b'/';
        rpath[1..1 + image.len()].copy_from_slice(image);
        let rpn = 1 + image.len();
        rpath[rpn] = b'/';
        rpath[rpn + 1..rpn + 1 + layer.len()].copy_from_slice(layer);
        let mut ob = [0u8; 32];
        ob[..7].copy_from_slice(b"/tmp/l0");
        ob[6] = b'0' + li as u8;
        ob[7..11].copy_from_slice(b".tar");
        if !run_wget(host, port, &rpath[..rpn + 1 + layer.len()], &ob[..11]) {
            user_lib::print("[TEST] img FAIL (layer)\n");
            user_lib::exit(1);
        }
        let tf = user_lib::open(ob.as_ptr(), 0);
        if tf < 0 {
            user_lib::print("[TEST] img FAIL (layer-open)\n");
            user_lib::exit(1);
        }
        let f = untar(tf, &root[..rootn], rootn);
        user_lib::close(tf);
        user_lib::unlink(ob.as_ptr());
        if f < 0 {
            // reason already printed by untar (untar:<tag>)
            user_lib::exit(1);
        }
        files += f;
        li += 1;
    }
    user_lib::unlink(b"/tmp/manifest\0".as_ptr());
    if files <= 0 {
        user_lib::print("[TEST] img FAIL (empty)\n");
        user_lib::exit(1);
    }
    user_lib::print("[TEST] img PASS\n");
    user_lib::exit(0);
}

// ---- v2.4: lifecycle (`run -d`, `ps`, `stop`, `rm`) ----

/// v2.8 flag helpers (for `run`; see _doc/v2.8.md §1).
fn bad_flag() -> ! {
    user_lib::print("usage: ctr run [-d] [--memory N] [--cpu P] [--weight W] [-v V:/cpath] <name> <prog> [args...]\n");
    user_lib::exit(1);
}

fn starts_with(s: &[u8], pre: &[u8]) -> bool {
    s.len() >= pre.len() && &s[..pre.len()] == pre
}

/// strip a known `--flag=` prefix; empty if absent (caller falls back
/// to the next argv -- `--flag=` with nothing after behaves like a
/// separate token, deterministic either way).
fn strip_prefix_eq<'a>(s: &'a [u8], pre: &[u8]) -> &'a [u8] {
    if starts_with(s, pre) {
        &s[pre.len()..]
    } else {
        b""
    }
}

/// v2.8: memory size to frames: decimal with optional K/M/G suffix
/// (bytes, rounded UP to 4KB frames). Bare number = frames. None on
/// garbage. Overflow fails loud (checked ops), never wraps.
fn parse_mem(b: &[u8]) -> Option<usize> {
    let mut i = 0;
    while i < b.len() && b[i] >= b'0' && b[i] <= b'9' {
        i += 1;
    }
    let v = parse_dec(&b[..i])?;
    if i == b.len() {
        return Some(v); // frames
    }
    let per = match b[i] {
        b'K' | b'k' => 1024usize,
        b'M' | b'm' => 1024 * 1024,
        b'G' | b'g' => 1024 * 1024 * 1024,
        _ => return None,
    };
    // single-letter suffix only (no "MB"/"KB" aliases, see §3).
    if i + 1 != b.len() {
        return None;
    }
    let bytes = v.checked_mul(per)?;
    Some(bytes.checked_add(4095)? / 4096)
}

/// decimal parse (leading digits only); None if no digits.
fn parse_dec(b: &[u8]) -> Option<usize> {
    let mut v = 0usize;
    let mut nd = 0;
    for &c in b {
        if c < b'0' || c > b'9' {
            break;
        }
        v = v.checked_mul(10)?.checked_add((c - b'0') as usize)?;
        nd += 1;
    }
    if nd == 0 {
        return None;
    }
    Some(v)
}

/// bytes append into a stack buffer; returns new length (saturating,
/// never NUL-terminates -- for composing single-write log lines).
fn buf_put(buf: &mut [u8], mut n: usize, s: &[u8]) -> usize {
    let m = s.len().min(buf.len().saturating_sub(n));
    buf[n..n + m].copy_from_slice(&s[..m]);
    n + m
}

/// decimal append into a stack buffer; returns new length (saturating).
fn push_dec(buf: &mut [u8], mut n: usize, mut v: usize) -> usize {
    let mut tmp = [0u8; 20];
    let mut m = 0usize;
    if v == 0 {
        tmp[0] = b'0';
        m = 1;
    } else {
        while v > 0 && m < 20 {
            tmp[m] = b'0' + (v % 10) as u8;
            v /= 10;
            m += 1;
        }
    }
    let mut i = m;
    while i > 0 && n < buf.len() {
        i -= 1;
        buf[n] = tmp[i];
        n += 1;
    }
    n
}

/// build `/ctr/<name>/.pid` (NUL-terminated) into `out`; returns length.
fn state_path(name: &[u8], out: &mut [u8; 64]) -> usize {
    out[..5].copy_from_slice(b"/ctr/");
    out[5..5 + name.len()].copy_from_slice(name);
    let mut n = 5 + name.len();
    out[n..n + 5].copy_from_slice(b"/.pid");
    n += 5;
    out[n] = 0;
    n
}

/// write `<pid> <start> <cg> <name>\n` state; false on any I/O error.
fn write_state(name: &[u8], pid: usize, start: usize, cg: usize) -> bool {
    let mut sp = [0u8; 64];
    state_path(name, &mut sp);
    let f = user_lib::open(sp.as_ptr(), user_lib::O_CREATE | user_lib::O_TRUNC);
    if f < 0 {
        return false;
    }
    let mut b = [0u8; 64];
    let mut n = push_dec(&mut b, 0, pid);
    // (ids are small in practice, but push_dec saturates: never index
    // past the end on absurd values -- fail the state write instead,
    // and the caller kills the untrackable child.)
    if n + 40 > b.len() {
        user_lib::close(f);
        return false;
    }
    b[n] = b' ';
    n += 1;
    n = push_dec(&mut b, n, start);
    if n + 36 > b.len() {
        user_lib::close(f);
        return false;
    }
    b[n] = b' ';
    n += 1;
    n = push_dec(&mut b, n, cg);
    if n + 26 > b.len() {
        user_lib::close(f);
        return false;
    }
    b[n] = b' ';
    n += 1;
    let m = name.len().min(23);
    b[n..n + m].copy_from_slice(&name[..m]);
    n += m;
    b[n] = b'\n';
    n += 1;
    let mut w = 0;
    while w < n {
        let r = user_lib::write(f, unsafe { b.as_ptr().add(w) }, n - w);
        if r <= 0 {
            user_lib::close(f);
            return false;
        }
        w += r as usize;
    }
    user_lib::close(f);
    true
}

/// read state into (pid, start, cg); None if missing/unparseable.
/// Tolerates the v2.4 two-field format (`<pid> <start> ...`, cg = 0).
fn read_state(name: &[u8]) -> Option<(usize, usize, usize)> {
    let mut sp = [0u8; 64];
    state_path(name, &mut sp);
    let f = user_lib::open(sp.as_ptr(), 0);
    if f < 0 {
        return None;
    }
    let mut b = [0u8; 64];
    let mut n = 0usize;
    loop {
        if n >= b.len() {
            break;
        }
        let r = user_lib::read(f, unsafe { b.as_mut_ptr().add(n) }, b.len() - n);
        if r <= 0 {
            break;
        }
        n += r as usize;
    }
    user_lib::close(f);
    // fields: <pid> <start> [<cg>] <name...>. The first two are required;
    // the third is numeric only in the v2.6+ format (v2.4 wrote the name
    // third -- a non-numeric third field means legacy, cg = 0).
    let mut i = 0usize;
    while i < n && b[i] == b' ' {
        i += 1;
    }
    let mut j = i;
    while j < n && b[j] != b' ' && b[j] != b'\n' {
        j += 1;
    }
    let pid = parse_dec(&b[i..j])?;
    let mut k = if j < n { j + 1 } else { n };
    while k < n && b[k] == b' ' {
        k += 1;
    }
    let mut l = k;
    while l < n && b[l] != b' ' && b[l] != b'\n' {
        l += 1;
    }
    let start = parse_dec(&b[k..l])?;
    let mut cg = 0usize;
    let mut m = if l < n { l + 1 } else { n };
    while m < n && b[m] == b' ' {
        m += 1;
    }
    if m < n && b[m] != b'\n' {
        let mut e = m;
        while e < n && b[e] != b' ' && b[e] != b'\n' {
            e += 1;
        }
        // v2.6 format has FOUR fields (pid start cg name); v2.4 has three
        // (pid start name). A numeric third field counts as cg only with
        // a fourth field behind it -- otherwise a digit-leading v2.4 name
        // ("123") would misroute stop at a wrong group.
        if let Some(v) = parse_dec(&b[m..e]) {
            let mut f2 = if e < n { e + 1 } else { n };
            while f2 < n && b[f2] == b' ' {
                f2 += 1;
            }
            if f2 < n && b[f2] != b'\n' {
                cg = v;
            }
        }
    }
    Some((pid, start, cg))
}

/// pidinfo decode: (alive, start-matches-state). Zombie counts as NOT
/// alive here (init reaps it within ticks); gone (-1) is not alive.
/// A state file whose start disagrees is stale (reboot) -> not alive.
fn state_alive(pid: usize, want_start: usize) -> bool {
    let v = user_lib::pidinfo(pid as isize);
    if v <= 0 {
        return false;
    }
    (v - 2) as usize == want_start
}

fn cmd_ps() {
    let mut nb = [0u8; 512];
    user_lib::print("CTRPS NAME PID STATUS\n");
    let r = user_lib::getdents(b"/ctr\0".as_ptr(), nb.as_mut_ptr(), 512);
    if r <= 0 {
        return;
    }
    let mut off = 0usize;
    // v2.4: same count-vs-bytes fix as cmd_assemble (r = entries).
    let mut seen = 0isize;
    while seen < r && off < 511 {
        let mut len = 0;
        while off + len < 511 && nb[off + len] != 0 {
            len += 1;
        }
        if len == 0 || len > 28 {
            break;
        }
        let entry = &nb[off..off + len];
        // getdents marks dirs with a trailing '/'; display + state lookup
        // use the bare name ("/ctr/life//.pid" would still resolve, but
        // the ps table should read clean). NOTE: `off` still steps by the
        // raw `len` below.
        let mut dlen = len;
        if dlen > 0 && entry[dlen - 1] == b'/' {
            dlen -= 1;
        }
        if dlen == 0 {
            off += len + 1;
            seen += 1;
            continue;
        }
        let entry = &entry[..dlen];
        // state probe: non-containers (plain files) fail the open.
        // (`rlen` = raw step length: `off` must skip the '/' too.)
        let (entry, len, rlen) = (entry, dlen, len);
        let mut sp = [0u8; 64];
        if 5 + len + 5 < 63 {
            sp[..5].copy_from_slice(b"/ctr/");
            sp[5..5 + len].copy_from_slice(entry);
            sp[5 + len..5 + len + 5].copy_from_slice(b"/.pid");
            let f = user_lib::open(sp.as_ptr(), 0);
            if f >= 0 {
                user_lib::close(f);
                if let Some((pid, start, _cg)) = read_state(entry) {
                    user_lib::print("CTRPS ");
                    let _ = user_lib::write(1, entry.as_ptr(), len);
                    user_lib::print(" ");
                    dbg_num(pid);
                    if state_alive(pid, start) {
                        // Up seconds are for humans (time() is ms,
                        // start is ticks); never asserted, only the
                        // `Up` word is (see _doc/v2.4.md §5).
                        let now = user_lib::time().max(0) as usize;
                        let up = now / 1000;
                        user_lib::print(" Up ");
                        dbg_num(up);
                        user_lib::print("s");
                    } else {
                        // v2.5: dead with a retained code?
                        // reapstat returns code+0x10000, or -1 (alive-but-
                        // unreaped zombie, evicted, or never existed).
                        let rs = user_lib::reapstat(pid as isize);
                        if rs != -1 {
                            user_lib::print(" Exited (code ");
                            let code = rs - 0x10000;
                            if code < 0 {
                                user_lib::print("-");
                                dbg_num((0 - code) as usize);
                            } else {
                                dbg_num(code as usize);
                            }
                            user_lib::print(")");
                        } else {
                            user_lib::print(" Exited");
                        }
                    }
                    user_lib::print("\n");
                }
            }
        }
        off += rlen + 1;
        seen += 1;
    }
}

fn cmd_stop(name: &[u8]) {
    let (pid, start, cg) = match read_state(name) {
        Some(t) => t,
        None => {
            user_lib::print("ctr: no such container\n");
            user_lib::exit(1);
        }
    };
    // v2.6: kill by cgroup when recorded (fate-sharing: children die
    // with pid1 instead of escaping to init). Legacy cg==0 (v2.4 state
    // files) keeps the old kill-pid1 path.
    let mut killed: isize;
    let grouped = cg > 0;
    if grouped {
        if !state_alive(pid, start) {
            stopped_msg(name);
        }
        killed = user_lib::cgkill(cg as isize);
        if killed < 0 {
            // stale group id (reboot): the liveness check above already
            // passed on (pid,start)... unreachable same-boot (groups are
            // never deleted), but never kill blind -- fall back to pid1.
            if user_lib::kill(pid as isize) != 0 {
                stopped_msg(name);
            }
            killed = 1;
        }
    } else {
        if !state_alive(pid, start) {
            stopped_msg(name);
        }
        if user_lib::kill(pid as isize) != 0 {
            // lost a race with exit/reap: same terminal state, same code.
            stopped_msg(name);
        }
        killed = 1;
    }
    // poll until pid1 is GONE (reaped) and -- grouped only -- the group
    // is empty (cgkill doubles as probe, returns 0). Gone guarantees the
    // exit code reached the ring for the suite's ps assertion.
    let mut i = 0;
    while i < 10 {
        let gone = user_lib::pidinfo(pid as isize) == -1;
        let empty = !grouped || user_lib::cgkill(cg as isize) == 0;
        if gone && empty {
            user_lib::print("stopped ");
            let _ = user_lib::write(1, name.as_ptr(), name.len());
            user_lib::print(" (killed ");
            dbg_num(killed as usize);
            user_lib::print(")\n");
            user_lib::exit(0);
        }
        user_lib::sleep(100); // 1s (sleep takes 10ms ticks)
        i += 1;
    }
    user_lib::print("ctr: stop timeout\n");
    user_lib::exit(1);
}

/// shared `(already exited)` terminal print (idempotent reruns).
fn stopped_msg(name: &[u8]) -> ! {
    user_lib::print("stopped ");
    let _ = user_lib::write(1, name.as_ptr(), name.len());
    user_lib::print(" (already exited)\n");
    user_lib::exit(0);
}

/// delete path and everything under it. Files and dirs share one path:
/// getdents on a file yields no children, so recurse-then-unlink is
/// correct for both without a stat/is_dir probe (no symlinks exist).
/// Repeats getdents until empty (512B buffer may truncate big dirs).
fn rm_all(path: &[u8; 64]) -> bool {
    loop {
        let mut nb = [0u8; 512];
        let r = user_lib::getdents(path.as_ptr(), nb.as_mut_ptr(), 512);
        if r < 0 {
            return false;
        }
        if r == 0 {
            break;
        }
        let mut off = 0usize;
        let mut n = 0;
        // (same count-vs-bytes rule; the outer loop repeats until empty,
        // so a short pass here only costs an extra pass, never correctness)
        let mut seen = 0isize;
        while seen < r && off < 511 {
            let mut len = 0;
            while off + len < 511 && nb[off + len] != 0 {
                len += 1;
            }
            if len == 0 || len > 90 {
                return false;
            }
            // path + "/" + child (NUL); overflow fails loud, never silent.
            let mut plen = 0;
            while plen < 64 && path[plen] != 0 {
                plen += 1;
            }
            if plen + 1 + len >= 63 {
                return false;
            }
            let mut child = [0u8; 64];
            child[..plen].copy_from_slice(&path[..plen]);
            child[plen] = b'/';
            child[plen + 1..plen + 1 + len].copy_from_slice(&nb[off..off + len]);
            if !rm_all(&child) {
                return false;
            }
            n += 1;
            seen += 1;
            off += len + 1;
        }
        if n == 0 {
            break;
        }
    }
    user_lib::unlink(path.as_ptr()) == 0
}

fn cmd_rm(name: &[u8]) {
    // refuse to delete a live container (stop first, docker-style).
    // A stale state (reboot, start mismatch) is NOT live: falls through.
    if let Some((pid, start, _cg)) = read_state(name) {
        if state_alive(pid, start) {
            user_lib::print("ctr: running (stop first)\n");
            user_lib::exit(1);
        }
    }
    let mut root = [0u8; 64];
    root[..5].copy_from_slice(b"/ctr/");
    root[5..5 + name.len()].copy_from_slice(name);
    root[5 + name.len()] = 0;
    // existence probe (getdents < 0): unknown names fail, never "remove".
    let mut probe = [0u8; 64];
    if user_lib::getdents(root.as_ptr(), probe.as_mut_ptr(), 64) < 0 {
        user_lib::print("ctr: no such image\n");
        user_lib::exit(1);
    }
    if !rm_all(&root) {
        user_lib::print("ctr: rm failed\n");
        user_lib::exit(1);
    }
    user_lib::print("removed ");
    let _ = user_lib::write(1, name.as_ptr(), name.len());
    user_lib::print("\n");
    user_lib::exit(0);
}

// ---- v3.0: packages (`install/remove/list`, apt-style) ----
// v3.1: `depends:` + recursive install + remove protection.
// v3.3: versions (`x.y.z`), `>=` constraints, index resolution,
// `upgrade`, `autoremove`.

/// v3.3: version constraint operators.
const VEXACT: u8 = 0;
const VATLEAST: u8 = 1;

/// v3.3: is `v` a well-formed version (`x.y.z`, all-numeric non-empty
/// components)? Guards route building (no traversal) and comparisons.
fn ver_valid(v: &[u8]) -> bool {
    if v.is_empty() {
        return false;
    }
    let mut i = 0;
    let mut comp = 0;
    loop {
        let mut j = i;
        while j < v.len() && v[j] != b'.' {
            if v[j] < b'0' || v[j] > b'9' {
                return false;
            }
            j += 1;
        }
        if j == i {
            return false; // empty component
        }
        comp += 1;
        if j >= v.len() {
            break;
        }
        i = j + 1;
    }
    comp > 0
}

/// v3.3: decimal value of an all-digit slice (caller guarantees digits).
fn ver_num(s: &[u8]) -> usize {
    let mut v = 0usize;
    for &c in s {
        v = v.saturating_mul(10).saturating_add((c - b'0') as usize);
    }
    v
}

/// v3.3: compare versions (-1/0/1). Missing components read as 0
/// (`1.0` == `1.0.0`). Both sides must be ver_valid (caller ensures).
fn vercmp(a: &[u8], b: &[u8]) -> i8 {
    let mut i = 0usize;
    let mut j = 0usize;
    loop {
        let ae = i >= a.len();
        let be = j >= b.len();
        if ae && be {
            return 0;
        }
        let mut ie = i;
        while ie < a.len() && a[ie] != b'.' {
            ie += 1;
        }
        let mut je = j;
        while je < b.len() && b[je] != b'.' {
            je += 1;
        }
        let av = if ae { 0 } else { ver_num(&a[i..ie]) };
        let bv = if be { 0 } else { ver_num(&b[j..je]) };
        if av < bv {
            return -1;
        }
        if av > bv {
            return 1;
        }
        i = if ie < a.len() { ie + 1 } else { a.len() };
        j = if je < b.len() { je + 1 } else { b.len() };
    }
}

/// v3.3: does installed/fetched `have` satisfy (`want`, `op`)?
fn ver_sat(have: &[u8], want: &[u8], op: u8) -> bool {
    if !ver_valid(have) || !ver_valid(want) {
        return false;
    }
    let c = vercmp(have, want);
    if op == VATLEAST {
        c >= 0
    } else {
        c == 0
    }
}

/// split a dep/install token into (name, Option<(ver, op)>).
/// `foo` -> (foo, None); `foo=1.0` -> Exact; `foo>=1.0` -> AtLeast.
/// A `>` not followed by `=` degrades to name-only (loud later).
fn split_vreq(tok: &[u8]) -> (&[u8], Option<(&[u8], u8)>) {
    let mut e = 0;
    while e < tok.len() && tok[e] != b'=' && tok[e] != b'>' {
        e += 1;
    }
    if e >= tok.len() {
        return (tok, None);
    }
    if tok[e] == b'>' {
        if e + 1 >= tok.len() || tok[e + 1] != b'=' {
            return (&tok[..e], None);
        }
        return (&tok[..e], Some((&tok[e + 2..], VATLEAST)));
    }
    (&tok[..e], Some((&tok[e + 1..], VEXACT)))
}

/// list bare entry names of dir `path` (no trailing `/`); returns
/// count. Entry-count getdents discipline, same as cmd_ps (r = entries,
/// never bytes).
fn dir_names(path: &[u8], out: &mut [[u8; 64]; 32]) -> usize {
    let mut nb = [0u8; 512];
    let r = user_lib::getdents(path.as_ptr(), nb.as_mut_ptr(), 512);
    if r <= 0 {
        return 0;
    }
    let mut off = 0usize;
    let mut n = 0usize;
    let mut seen = 0isize;
    while seen < r && off < 511 && n < out.len() {
        let mut len = 0;
        while off + len < 511 && nb[off + len] != 0 {
            len += 1;
        }
        if len == 0 || len > 28 {
            break;
        }
        let mut dlen = len;
        if dlen > 0 && nb[off + dlen - 1] == b'/' {
            dlen -= 1;
        }
        if dlen > 0 {
            let m = dlen.min(63);
            out[n][..m].copy_from_slice(&nb[off..off + m]);
            out[n][m] = 0;
            n += 1;
        }
        off += len + 1;
        seen += 1;
    }
    n
}

/// existence probe (regular files; dirs use getdents like cmd_run).
fn path_exists(path: &[u8]) -> bool {
    let f = user_lib::open(path.as_ptr(), 0);
    if f < 0 {
        return false;
    }
    user_lib::close(f);
    true
}

/// does `/pkg/db` hold a `name ` line? (Absent db = empty, never error.)
fn pkg_db_has(name: &[u8]) -> bool {
    let f = user_lib::open(b"/pkg/db\0".as_ptr(), 0);
    if f < 0 {
        return false;
    }
    let mut b = [0u8; 2048];
    let mut n = 0usize;
    loop {
        if n >= b.len() {
            break;
        }
        let r = user_lib::read(f, unsafe { b.as_mut_ptr().add(n) }, b.len() - n);
        if r <= 0 {
            break;
        }
        n += r as usize;
    }
    user_lib::close(f);
    let mut i = 0;
    while i < n {
        let mut j = i;
        while j < n && b[j] != b'\n' {
            j += 1;
        }
        // line b[i..j]: `name version`
        if j - i > name.len() && &b[i..i + name.len()] == name && b[i + name.len()] == b' ' {
            return true;
        }
        i = j + 1;
    }
    false
}

/// append `name version manual|auto\n` to /pkg/db. false on I/O error.
/// v3.3: the manual flag (top-level installs) vs auto (pulled-in deps);
/// autoremove only collects auto orphans. Missing flag on old lines
/// reads as manual (never auto-remove user stuff by default).
fn pkg_db_add(name: &[u8], ver: &[u8], manual: bool) -> bool {
    let f = user_lib::open(
        b"/pkg/db\0".as_ptr(),
        user_lib::O_CREATE | user_lib::O_APPEND,
    );
    if f < 0 {
        return false;
    }
    let mut b = [0u8; 64];
    let m = name.len().min(23);
    b[..m].copy_from_slice(&name[..m]);
    b[m] = b' ';
    let v = ver.len().min(31);
    b[m + 1..m + 1 + v].copy_from_slice(&ver[..v]);
    let mut n = m + 1 + v;
    // manual/auto flag (v3.3).
    if n + 1 + 6 + 1 > b.len() {
        user_lib::close(f);
        return false;
    }
    b[n] = b' ';
    if manual {
        b[n + 1..n + 7].copy_from_slice(b"manual");
        n += 7;
    } else {
        b[n + 1..n + 5].copy_from_slice(b"auto");
        n += 5;
    }
    b[n] = b'\n';
    n += 1;
    let mut w = 0;
    while w < n {
        let r = user_lib::write(f, unsafe { b.as_ptr().add(w) }, n - w);
        if r <= 0 {
            user_lib::close(f);
            return false;
        }
        w += r as usize;
    }
    user_lib::close(f);
    true
}

/// rewrite /pkg/db without the `name ` line. false on I/O error
/// (caller keeps whatever state it can still report loudly).
fn pkg_db_del(name: &[u8]) -> bool {
    let f = user_lib::open(b"/pkg/db\0".as_ptr(), 0);
    if f < 0 {
        return false;
    }
    let mut b = [0u8; 2048];
    let mut n = 0usize;
    loop {
        if n >= b.len() {
            break;
        }
        let r = user_lib::read(f, unsafe { b.as_mut_ptr().add(n) }, b.len() - n);
        if r <= 0 {
            break;
        }
        n += r as usize;
    }
    user_lib::close(f);
    let f = user_lib::open(
        b"/pkg/db\0".as_ptr(),
        user_lib::O_CREATE | user_lib::O_TRUNC,
    );
    if f < 0 {
        return false;
    }
    let mut ok = true;
    let mut i = 0;
    while i < n {
        let mut j = i;
        while j < n && b[j] != b'\n' {
            j += 1;
        }
        let e = if j < n { j + 1 } else { j }; // keep the newline
        let keep = !(j - i > name.len() && &b[i..i + name.len()] == name && b[i + name.len()] == b' ');
        if keep {
            let mut w = i;
            while w < e {
                let r = user_lib::write(f, unsafe { b.as_ptr().add(w) }, e - w);
                if r <= 0 {
                    ok = false;
                    break;
                }
                w += r as usize;
            }
            if !ok {
                break;
            }
        }
        i = e;
    }
    user_lib::close(f);
    ok
}

/// `ctr install <host> <port> <pkg>`: fetch manifest + layers from
/// `/pkg/<pkg>/...`, unpack into the `/pkg/<pkg>/` store, hardlink
/// `bin/*` into `/bin`, record the db. Prints
/// `pkg-installed <name> <version>`. Post-unpack failures roll the
/// store back (db entry ⟺ fully installed); reinstall is refused
/// (upgrade is a later version's job).
/// v3.1: thin wrapper -- recursion lives in install_one (deps first).
/// v3.3: want carries an install-argv pin (`=ver` / `>=ver`), or None.
fn cmd_install(host: &[u8], port: &[u8], pkg: &[u8], want: Option<(&[u8], u8)>) {
    mkdir_p(b"/tmp\0");
    mkdir_p(b"/pkg\0");
    if pkg_db_has(pkg) {
        user_lib::print("ctr: already installed\n");
        user_lib::exit(1);
    }
    let mut stack = [[0u8; 64]; 8];
    let mut stklen = [0usize; 8];
    install_one(host, port, pkg, want, &mut stack, &mut stklen, 0, true);
    user_lib::exit(0);
}

/// installed version of pkg into ver_out; returns length, or
/// usize::MAX when absent. (db lines are `name version`.)
fn pkg_db_ver(pkg: &[u8], ver_out: &mut [u8; 32]) -> usize {
    let f = user_lib::open(b"/pkg/db\0".as_ptr(), 0);
    if f < 0 {
        return usize::MAX;
    }
    let mut b = [0u8; 2048];
    let mut n = 0usize;
    loop {
        if n >= b.len() {
            break;
        }
        let r = user_lib::read(f, unsafe { b.as_mut_ptr().add(n) }, b.len() - n);
        if r <= 0 {
            break;
        }
        n += r as usize;
    }
    user_lib::close(f);
    let mut i = 0;
    while i < n {
        let mut j = i;
        while j < n && b[j] != b'\n' {
            j += 1;
        }
        if j - i > pkg.len() && &b[i..i + pkg.len()] == pkg && b[i + pkg.len()] == b' ' {
            // version runs to the next space (v3.3 flag) or EOL.
            let vs = i + pkg.len() + 1;
            let mut ve = vs;
            while ve < j && b[ve] != b' ' {
                ve += 1;
            }
            let m = (ve - vs).min(31);
            ver_out[..m].copy_from_slice(&b[vs..vs + m]);
            return m;
        }
        i = j + 1;
    }
    usize::MAX
}

/// v3.3: manual flag of an installed pkg (db third field). Absent or
/// unparseable reads as manual (safe default: never auto-remove).
fn pkg_db_manual(pkg: &[u8]) -> bool {
    let f = user_lib::open(b"/pkg/db\0".as_ptr(), 0);
    if f < 0 {
        return true;
    }
    let mut b = [0u8; 2048];
    let mut n = 0usize;
    loop {
        if n >= b.len() {
            break;
        }
        let r = user_lib::read(f, unsafe { b.as_mut_ptr().add(n) }, b.len() - n);
        if r <= 0 {
            break;
        }
        n += r as usize;
    }
    user_lib::close(f);
    let mut i = 0;
    while i < n {
        let mut j = i;
        while j < n && b[j] != b'\n' {
            j += 1;
        }
        if j - i > pkg.len() && &b[i..i + pkg.len()] == pkg && b[i + pkg.len()] == b' ' {
            // third field, if any: `auto` counts, anything else manual.
            let vs = i + pkg.len() + 1;
            let mut ve = vs;
            while ve < j && b[ve] != b' ' {
                ve += 1;
            }
            if ve < j && &b[ve + 1..j] == b"auto" {
                return false;
            }
            return true;
        }
        i = j + 1;
    }
    true
}

/// parse manifest bytes: version + layer files + dep tokens.
/// Unknown `key:` lines ignored (forward-compat, see plan3.x §3).
/// Layer cap 8 (pull discipline); dep cap 8. Returns (vern, nl, ndeps).
/// v3.4: also captures `sha256:` (64 lowercase hex, else ignored here
/// and reported missing at verify time).
fn parse_manifest(
    mb: &[u8],
    mn: usize,
    ver: &mut [u8; 32],
    layers: &mut [[u8; 64]; 8],
    layern: &mut [usize; 8],
    deps: &mut [[u8; 64]; 8],
    depn: &mut [usize; 8],
    sha: &mut [u8; 64],
) -> (usize, usize, usize, usize) {
    let mut vern = 0usize;
    let mut nl = 0usize;
    let mut ndeps = 0usize;
    let mut shan = 0usize;
    let mut i = 0usize;
    while i < mn {
        let mut j = i;
        while j < mn && mb[j] != b'\n' {
            j += 1;
        }
        let mut e = j;
        if e > i && mb[e - 1] == b'\r' {
            e -= 1;
        }
        let line = &mb[i..e];
        if !line.is_empty() && line[0] != b'#' {
            let mut c = 0;
            while c < line.len() && line[c] != b':' {
                c += 1;
            }
            if c < line.len() {
                let (k, v) = (&line[..c], &line[c + 1..]);
                let v = if !v.is_empty() && v[0] == b' ' { &v[1..] } else { v };
                if k == b"version" && vern == 0 {
                    vern = v.len().min(31);
                    ver[..vern].copy_from_slice(&v[..vern]);
                } else if k == b"sha256" && shan == 0 {
                    // v3.4: 64 lowercase hex (validated at verify time).
                    // (Buffer is [u8; 64]: cap at 64, not 63 -- a truncated
                    // hash mismatches forever and everything fails.)
                    shan = v.len().min(64);
                    sha[..shan].copy_from_slice(&v[..shan]);
                } else if k == b"depends" {
                    // space-separated `name` / `name=ver` tokens.
                    let mut t = 0;
                    while t < v.len() && ndeps < 8 {
                        while t < v.len() && v[t] == b' ' {
                            t += 1;
                        }
                        if t >= v.len() {
                            break;
                        }
                        let mut u = t;
                        while u < v.len() && v[u] != b' ' {
                            u += 1;
                        }
                        let m = (u - t).min(63);
                        deps[ndeps][..m].copy_from_slice(&v[t..t + m]);
                        depn[ndeps] = m;
                        ndeps += 1;
                        t = u;
                    }
                }
                // (other keys ignored: forward-compat)
            } else if nl < 8 {
                let m = line.len().min(63);
                layers[nl][..m].copy_from_slice(&line[..m]);
                layern[nl] = m;
                nl += 1;
            }
        }
        i = j + 1;
    }
    (vern, nl, ndeps, shan)
}

/// v3.3: fetch `/pkg/<pkg>/index`, collect well-formed versions
/// (skips `#`/empty/non-conforming lines). Returns count (cap 8).
fn fetch_index(
    host: &[u8],
    port: &[u8],
    pkg: &[u8],
    vers: &mut [[u8; 32]; 8],
    versn: &mut [usize; 8],
) -> usize {
    let mut ipath = [0u8; 128];
    ipath[..5].copy_from_slice(b"/pkg/");
    ipath[5..5 + pkg.len()].copy_from_slice(pkg);
    let ipn = 5 + pkg.len();
    ipath[ipn..ipn + 6].copy_from_slice(b"/index");
    if !run_wget(host, port, &ipath[..ipn + 6], b"/tmp/pindex") {
        user_lib::print("ctr: pkg fetch failed (index)\n");
        user_lib::exit(1);
    }
    let f = user_lib::open(b"/tmp/pindex\0".as_ptr(), 0);
    if f < 0 {
        user_lib::print("ctr: pkg fetch failed (index-open)\n");
        user_lib::exit(1);
    }
    let mut b = [0u8; 512];
    let mut n = 0usize;
    loop {
        if n >= b.len() {
            break;
        }
        let r = user_lib::read(f, unsafe { b.as_mut_ptr().add(n) }, b.len() - n);
        if r <= 0 {
            break;
        }
        n += r as usize;
    }
    user_lib::close(f);
    user_lib::unlink(b"/tmp/pindex\0".as_ptr());
    let mut nv = 0;
    let mut i = 0;
    while i < n && nv < 8 {
        let mut j = i;
        while j < n && b[j] != b'\n' {
            j += 1;
        }
        let mut e = j;
        if e > i && b[e - 1] == b'\r' {
            e -= 1;
        }
        let line = &b[i..e];
        if !line.is_empty() && line[0] != b'#' && ver_valid(line) {
            let m = line.len().min(31);
            vers[nv][..m].copy_from_slice(&line[..m]);
            versn[nv] = m;
            nv += 1;
        }
        i = j + 1;
    }
    nv
}

/// install pkg + its transitive deps (DFS post-order: deps first).
/// want (from a parent's token or install argv) is enforced against
/// the db when already installed; otherwise the target version is
/// resolved (Exact = direct route, else index max satisfying) and the
/// fetched manifest must equal it. The ancestor chain lives in
/// `stack`/`stklen` as COPIES (borrows cannot outlive their frame
/// across recursion); `depth` is the chain length (cycle + depth
/// guard). Loud exits throughout (install discipline, see v2.8).
fn install_one(
    host: &[u8],
    port: &[u8],
    pkg: &[u8],
    want: Option<(&[u8], u8)>,
    stack: &mut [[u8; 64]; 8],
    stklen: &mut [usize; 8],
    depth: usize,
    manual: bool,
) {
    // already installed: skip iff any required version satisfies.
    if pkg_db_has(pkg) {
        if let Some((w, op)) = want {
            let mut vb = [0u8; 32];
            let vn = pkg_db_ver(pkg, &mut vb);
            if vn == usize::MAX || !ver_sat(&vb[..vn], w, op) {
                user_lib::print("ctr: version mismatch (");
                let _ = user_lib::write(1, pkg.as_ptr(), pkg.len());
                user_lib::print(")\n");
                user_lib::exit(1);
            }
        }
        return;
    }
    if depth >= 8 {
        user_lib::print("ctr: dependency depth\n");
        user_lib::exit(1);
    }
    let mut s = 0;
    while s < depth {
        if &stack[s][..stklen[s]] == pkg {
            user_lib::print("ctr: dependency cycle\n");
            user_lib::exit(1);
        }
        s += 1;
    }
    // resolve the version to fetch: pinned goes direct, otherwise the
    // index maximum satisfying the constraint (unconstrained = max).
    let mut target = [0u8; 32];
    let mut targetn = 0usize;
    match want {
        Some((w, VEXACT)) => {
            if !ver_valid(w) {
                user_lib::print("ctr: bad pkg version\n");
                user_lib::exit(1);
            }
            targetn = w.len().min(31);
            target[..targetn].copy_from_slice(&w[..targetn]);
        }
        _ => {
            let mut vers = [[0u8; 32]; 8];
            let mut versn = [0usize; 8];
            let nv = fetch_index(host, port, pkg, &mut vers, &mut versn);
            if nv == 0 {
                user_lib::print("ctr: no versions\n");
                user_lib::exit(1);
            }
            let mut best = usize::MAX;
            let mut vi = 0;
            while vi < nv {
                let ok = match want {
                    Some((w, op)) => ver_sat(&vers[vi][..versn[vi]], w, op),
                    None => true,
                };
                if ok
                    && (best == usize::MAX
                        || vercmp(&vers[vi][..versn[vi]], &vers[best][..versn[best]]) > 0)
                {
                    best = vi;
                }
                vi += 1;
            }
            if best == usize::MAX {
                user_lib::print("ctr: no matching version\n");
                user_lib::exit(1);
            }
            targetn = versn[best];
            target[..targetn].copy_from_slice(&vers[best][..targetn]);
        }
    }
    // manifest at the versioned route.
    let mut mpath = [0u8; 128];
    mpath[..5].copy_from_slice(b"/pkg/");
    mpath[5..5 + pkg.len()].copy_from_slice(pkg);
    let mpn = 5 + pkg.len();
    mpath[mpn] = b'/';
    mpath[mpn + 1..mpn + 1 + targetn].copy_from_slice(&target[..targetn]);
    let mpn = mpn + 1 + targetn;
    mpath[mpn..mpn + 9].copy_from_slice(b"/manifest");
    if !run_wget(host, port, &mpath[..mpn + 9], b"/tmp/manifest") {
        user_lib::print("ctr: pkg fetch failed (manifest)\n");
        user_lib::exit(1);
    }
    let mf = user_lib::open(b"/tmp/manifest\0".as_ptr(), 0);
    if mf < 0 {
        user_lib::print("ctr: pkg fetch failed (manifest-open)\n");
        user_lib::exit(1);
    }
    let mut mb = [0u8; 2048];
    let mut mn = 0usize;
    loop {
        if mn >= mb.len() {
            break;
        }
        let r = user_lib::read(mf, unsafe { mb.as_mut_ptr().add(mn) }, mb.len() - mn);
        if r <= 0 {
            break;
        }
        mn += r as usize;
    }
    user_lib::close(mf);
    let mut ver = [0u8; 32];
    let mut layers = [[0u8; 64]; 8];
    let mut layern = [0usize; 8];
    let mut deps = [[0u8; 64]; 8];
    let mut depn = [0usize; 8];
    let mut sha = [0u8; 64];
    let (vern, nl, ndeps, shan) = parse_manifest(&mb, mn, &mut ver, &mut layers, &mut layern, &mut deps, &mut depn, &mut sha);
    if nl == 0 {
        user_lib::print("ctr: pkg fetch failed (no-layers)\n");
        user_lib::exit(1);
    }
    // manifest version must equal the resolved target (index skew or
    // registry inconsistency screams here; the constraint itself was
    // enforced by resolution).
    if vern != targetn || &ver[..vern] != &target[..targetn] {
        user_lib::print("ctr: version skew (");
        let _ = user_lib::write(1, pkg.as_ptr(), pkg.len());
        user_lib::print(")\n");
        user_lib::exit(1);
    }
    // deps first (post-order). The chain slot holds a copy: the dep
    // name slices below borrow this frame's `deps` buffer and cannot
    // be stored across the recursive call.
    {
        let m = pkg.len().min(63);
        stack[depth][..m].copy_from_slice(&pkg[..m]);
        stklen[depth] = m;
    }
    let mut d = 0;
    while d < ndeps {
        let (dn, dw) = split_vreq(&deps[d][..depn[d]]);
        if dn.is_empty() || !valid_name(dn) {
            user_lib::print("ctr: bad dependency\n");
            user_lib::exit(1);
        }
        if let Some((w, _)) = dw {
            if !ver_valid(w) {
                user_lib::print("ctr: bad dependency version\n");
                user_lib::exit(1);
            }
        }
        install_one(host, port, dn, dw, stack, stklen, depth + 1, false);
        d += 1;
    }
    install_layers(host, port, pkg, &ver[..vern], &mb[..mn], &layers, &layern, nl, manual, &sha[..shan]);
}

/// v3.3: scan a tar file's headers, collecting regular-file paths
/// (prefix-joined; dirs skipped). Returns count (cap 64) or usize::MAX
/// on I/O or format failure. Same header layout untar() walks
/// (name[0..100], size octal[124..136], prefix[345..500]).
fn tar_files(tarpath: &[u8], out: &mut [[u8; 128]]) -> usize {
    let f = user_lib::open(tarpath.as_ptr(), 0);
    if f < 0 {
        return usize::MAX;
    }
    let mut n = 0usize;
    let mut blk = [0u8; 512];
    loop {
        let mut o = 0;
        let mut ok = true;
        while o < 512 {
            let r = user_lib::read(f, unsafe { blk.as_mut_ptr().add(o) }, 512 - o);
            if r <= 0 {
                ok = false;
                break;
            }
            o += r as usize;
        }
        if !ok {
            user_lib::close(f);
            return usize::MAX;
        }
        if is_zero_block(&blk) {
            break;
        }
        let size = parse_octal(&blk[124..136]).unwrap_or(usize::MAX);
        if size == usize::MAX {
            user_lib::close(f);
            return usize::MAX;
        }
        let mut nn = 0;
        while nn < 100 && blk[nn] != 0 {
            nn += 1;
        }
        let mut pn = 0;
        while pn < 155 && blk[345 + pn] != 0 {
            pn += 1;
        }
        if blk[156] != b'5' && n < out.len() {
            let mut w = 0;
            if pn > 0 {
                let m = pn.min(100);
                out[n][..m].copy_from_slice(&blk[345..345 + m]);
                w = m;
                if w < 127 {
                    out[n][w] = b'/';
                    w += 1;
                }
            }
            let m = nn.min(127 - w);
            out[n][w..w + m].copy_from_slice(&blk[..m]);
            w += m;
            out[n][w] = 0;
            n += 1;
        }
        let mut left = (size + 511) / 512;
        while left > 0 {
            let mut o2 = 0;
            let mut ok2 = true;
            while o2 < 512 {
                let r = user_lib::read(f, unsafe { blk.as_mut_ptr().add(o2) }, 512 - o2);
                if r <= 0 {
                    ok2 = false;
                    break;
                }
                o2 += r as usize;
            }
            if !ok2 {
                user_lib::close(f);
                return usize::MAX;
            }
            left -= 1;
        }
    }
    user_lib::close(f);
    n
}

/// v3.3: write the store file list (`files`: installed relative paths,
/// one per line) for upgrade GC.
fn store_write_files(root: &[u8; 64], rootn: usize, names: &[[u8; 128]; 64], nn: usize) -> bool {
    let mut fp = [0u8; 64];
    fp[..rootn].copy_from_slice(&root[..rootn]);
    fp[rootn..rootn + 6].copy_from_slice(b"/files");
    let f = user_lib::open(fp.as_ptr(), user_lib::O_CREATE | user_lib::O_TRUNC);
    if f < 0 {
        return false;
    }
    let mut ok = true;
    let mut k = 0;
    while k < nn && ok {
        let mut nlen = 0;
        while nlen < 128 && names[k][nlen] != 0 {
            nlen += 1;
        }
        let mut w = 0;
        while w < nlen {
            let r = user_lib::write(f, unsafe { names[k].as_ptr().add(w) }, nlen - w);
            if r <= 0 {
                ok = false;
                break;
            }
            w += r as usize;
        }
        if ok {
            let r = user_lib::write(f, b"\n".as_ptr(), 1);
            if r <= 0 {
                ok = false;
            }
        }
        k += 1;
    }
    user_lib::close(f);
    ok
}

/// v3.3: read the store file list. Returns count (cap 64);
/// a missing file reads as empty (pre-v3.3 stores).
fn store_read_files(root: &[u8; 64], rootn: usize, out: &mut [[u8; 128]; 64]) -> usize {
    let mut fp = [0u8; 64];
    fp[..rootn].copy_from_slice(&root[..rootn]);
    fp[rootn..rootn + 6].copy_from_slice(b"/files");
    let f = user_lib::open(fp.as_ptr(), 0);
    if f < 0 {
        return 0;
    }
    let mut b = [0u8; 4096];
    let mut n = 0usize;
    loop {
        if n >= b.len() {
            break;
        }
        let r = user_lib::read(f, unsafe { b.as_mut_ptr().add(n) }, b.len() - n);
        if r <= 0 {
            break;
        }
        n += r as usize;
    }
    user_lib::close(f);
    let mut nn = 0;
    let mut i = 0;
    while i < n && nn < out.len() {
        let mut j = i;
        while j < n && b[j] != b'\n' {
            j += 1;
        }
        if j > i {
            let m = (j - i).min(127);
            out[nn][..m].copy_from_slice(&b[i..i + m]);
            out[nn][m] = 0;
            nn += 1;
        }
        i = j + 1;
    }
    nn
}

/// v3.4: verify the concatenated layer tars (/tmp/p0..p<nl-1>.tar)
/// against the manifest hex. Missing/short hash -> `sha256 missing`;
/// mismatch or unreadable tar -> `sha256 mismatch`. True on success
/// (callers roll back / abort loud on false).
fn verify_tars(nl: usize, hex: &[u8]) -> bool {
    if hex.is_empty() {
        user_lib::print("ctr: sha256 missing\n");
        return false;
    }
    if hex.len() != 64 {
        user_lib::print("ctr: sha256 mismatch\n");
        return false;
    }
    // (lowercase only: pkgbuild/registry emit lowercase, write it down.)
    let mut li = 0;
    while li < hex.len() {
        let c = hex[li];
        if !((c >= b'0' && c <= b'9') || (c >= b'a' && c <= b'f')) {
            user_lib::print("ctr: sha256 mismatch\n");
            return false;
        }
        li += 1;
    }
    let mut st = user_lib::Sha256::new();
    let mut li = 0usize;
    while li < nl {
        let mut ob = [0u8; 32];
        ob[..7].copy_from_slice(b"/tmp/p0");
        ob[6] = b'0' + li as u8;
        ob[7..11].copy_from_slice(b".tar");
        let f = user_lib::open(ob.as_ptr(), 0);
        if f < 0 {
            user_lib::print("ctr: sha256 mismatch\n");
            return false;
        }
        let mut blk = [0u8; 512];
        loop {
            let r = user_lib::read(f, blk.as_mut_ptr(), blk.len());
            if r < 0 {
                user_lib::close(f);
                user_lib::print("ctr: sha256 mismatch\n");
                return false;
            }
            if r == 0 {
                break;
            }
            st.update(&blk[..r as usize]);
        }
        user_lib::close(f);
        li += 1;
    }
    let sum = st.finalize();
    let mut hb = [0u8; 64];
    user_lib::sha256_hex(&sum, &mut hb);
    if &hb[..] != hex {
        user_lib::print("ctr: sha256 mismatch\n");
        return false;
    }
    true
}

/// unpack + link + db for an already-resolved package (deps done).
/// mb is the fetched manifest (a copy is stored for remove's
/// needed-by scan). Post-unpack failures roll the store back
/// (db entry ⟺ fully installed). Returns normally (the TOP-LEVEL
/// cmd_install exits); failures exit loud and abort the whole tree --
/// a dep must never exit(0) out from under its parent (v3.1 lesson:
/// the shared exit killed the parent's own install).
fn install_layers(
    host: &[u8],
    port: &[u8],
    pkg: &[u8],
    ver: &[u8],
    mb: &[u8],
    layers: &[[u8; 64]; 8],
    layern: &[usize; 8],
    nl: usize,
    manual: bool,
    sha: &[u8],
) {
    let mut root = [0u8; 64];
    root[..5].copy_from_slice(b"/pkg/");
    root[5..5 + pkg.len()].copy_from_slice(pkg);
    let rootn = 5 + pkg.len();
    mkdir_p(&root);
    let rollback = |root: &[u8; 64]| -> ! {
        rm_all(root);
        user_lib::print("ctr: pkg install failed\n");
        user_lib::exit(1);
    };
    let mut li = 0usize;
    while li < nl {
        let layer = &layers[li][..layern[li]];
        // v3.3: versioned layer route.
        let mut rpath = [0u8; 128];
        rpath[..5].copy_from_slice(b"/pkg/");
        rpath[5..5 + pkg.len()].copy_from_slice(pkg);
        let rpn = 5 + pkg.len();
        rpath[rpn] = b'/';
        rpath[rpn + 1..rpn + 1 + ver.len()].copy_from_slice(ver);
        let rpn = rpn + 1 + ver.len();
        rpath[rpn] = b'/';
        rpath[rpn + 1..rpn + 1 + layer.len()].copy_from_slice(layer);
        let mut ob = [0u8; 32];
        ob[..7].copy_from_slice(b"/tmp/p0");
        ob[6] = b'0' + li as u8;
        ob[7..11].copy_from_slice(b".tar");
        if !run_wget(host, port, &rpath[..rpn + 1 + layer.len()], &ob[..11]) {
            rollback(&root);
        }
        li += 1;
    }
    // v3.4: verify BEFORE unpacking (tampered bytes never touch the store).
    if !verify_tars(nl, sha) {
        rollback(&root);
    }
    let mut li = 0usize;
    while li < nl {
        let mut ob = [0u8; 32];
        ob[..7].copy_from_slice(b"/tmp/p0");
        ob[6] = b'0' + li as u8;
        ob[7..11].copy_from_slice(b".tar");
        let tf = user_lib::open(ob.as_ptr(), 0);
        if tf < 0 {
            rollback(&root);
        }
        let f = untar(tf, &root[..rootn], rootn);
        user_lib::close(tf);
        // (tars stay until the files list is scanned below.)
        if f <= 0 {
            // (empty package is bogus, like pull's empty guard)
            rollback(&root);
        }
        li += 1;
    }
    user_lib::unlink(b"/tmp/manifest\0".as_ptr());
    // v3.1: stash a manifest copy in the store for remove's needed-by
    // scan (rm_all takes it with the store; bin listing is unaffected).
    {
        let mut mp = [0u8; 64];
        mp[..rootn].copy_from_slice(&root[..rootn]);
        mp[rootn..rootn + 9].copy_from_slice(b"/manifest");
        let f = user_lib::open(mp.as_ptr(), user_lib::O_CREATE | user_lib::O_TRUNC);
        if f < 0 {
            rollback(&root);
        }
        let mut w = 0;
        while w < mb.len() {
            let r = user_lib::write(f, unsafe { mb.as_ptr().add(w) }, mb.len() - w);
            if r <= 0 {
                user_lib::close(f);
                rollback(&root);
            }
            w += r as usize;
        }
        user_lib::close(f);
    }
    // v3.3: scan the kept tars into the store file list (upgrade GC),
    // then drop the tars.
    {
        let mut names = [[0u8; 128]; 64];
        let mut nn = 0usize;
        let mut li = 0usize;
        let mut ok = true;
        while li < nl && ok {
            let mut ob = [0u8; 32];
            ob[..7].copy_from_slice(b"/tmp/p0");
            ob[6] = b'0' + li as u8;
            ob[7..11].copy_from_slice(b".tar");
            if nn >= 64 {
                ok = false;
            } else {
                let got = tar_files(&ob, &mut names[nn..]);
                if got == usize::MAX || nn + got > 64 {
                    ok = false;
                } else {
                    nn += got;
                }
            }
            user_lib::unlink(ob.as_ptr());
            li += 1;
        }
        if !ok || !store_write_files(&root, rootn, &names, nn) {
            rollback(&root);
        }
    }
    // 4. link store/bin/* into /bin -- after a full shadow pre-check
    // (an existing /bin name refuses the whole install: no clobber,
    // and remove can never delete a file it did not install).
    let mut binpath = [0u8; 64];
    binpath[..rootn].copy_from_slice(&root[..rootn]);
    binpath[rootn..rootn + 4].copy_from_slice(b"/bin");
    let mut names = [[0u8; 64]; 32];
    let nn = dir_names(&binpath, &mut names);
    let mut k = 0usize;
    while k < nn {
        let mut nlen = 0;
        while nlen < 64 && names[k][nlen] != 0 {
            nlen += 1;
        }
        let mut dst = [0u8; 64];
        dst[..5].copy_from_slice(b"/bin/");
        dst[5..5 + nlen].copy_from_slice(&names[k][..nlen]);
        if path_exists(&dst) {
            user_lib::print("ctr: shadows /bin/");
            let _ = user_lib::write(1, names[k].as_ptr(), nlen);
            user_lib::print("\n");
            rm_all(&root);
            user_lib::exit(1);
        }
        k += 1;
    }
    let mut k = 0usize;
    while k < nn {
        let mut nlen = 0;
        while nlen < 64 && names[k][nlen] != 0 {
            nlen += 1;
        }
        let mut src = [0u8; 64];
        src[..rootn + 4].copy_from_slice(&binpath[..rootn + 4]);
        src[rootn + 4] = b'/';
        src[rootn + 5..rootn + 5 + nlen].copy_from_slice(&names[k][..nlen]);
        let mut dst = [0u8; 64];
        dst[..5].copy_from_slice(b"/bin/");
        dst[5..5 + nlen].copy_from_slice(&names[k][..nlen]);
        if user_lib::link(src.as_ptr(), dst.as_ptr()) != 0 {
            rollback(&root);
        }
        k += 1;
    }
    if !pkg_db_add(pkg, ver, manual) {
        // db unwritable: roll the files back too (db entry ⟺ installed).
        let mut k = 0usize;
        while k < nn {
            let mut nlen = 0;
            while nlen < 64 && names[k][nlen] != 0 {
                nlen += 1;
            }
            let mut dst = [0u8; 64];
            dst[..5].copy_from_slice(b"/bin/");
            dst[5..5 + nlen].copy_from_slice(&names[k][..nlen]);
            user_lib::unlink(dst.as_ptr());
            k += 1;
        }
        rollback(&root);
    }
    user_lib::print("pkg-installed ");
    let _ = user_lib::write(1, pkg.as_ptr(), pkg.len());
    user_lib::print(" ");
    let _ = user_lib::write(1, ver.as_ptr(), ver.len());
    user_lib::print("\n");
}

/// first installed package (other than `pkg`) whose stored manifest
/// `depends:` names `pkg` (any `=ver` still counts as needing).
/// Copies the needer name into `out`, true iff found. Scans
/// /pkg/*/manifest (v3.1 stashes one per install; missing/unreadable
/// entries are skipped, never fatal).
fn pkg_needed_by(pkg: &[u8], out: &mut [u8; 32]) -> bool {
    let mut dirs = [[0u8; 64]; 32];
    let mut base = [0u8; 64];
    base[..5].copy_from_slice(b"/pkg/");
    let nd = dir_names(&base, &mut dirs);
    let mut k = 0usize;
    while k < nd {
        let mut nlen = 0;
        while nlen < 64 && dirs[k][nlen] != 0 {
            nlen += 1;
        }
        let entry = &dirs[k][..nlen];
        // self + the db file can never be needers.
        if entry == pkg || entry == b"db" {
            k += 1;
            continue;
        }
        let mut mp = [0u8; 64];
        mp[..5].copy_from_slice(b"/pkg/");
        mp[5..5 + nlen].copy_from_slice(entry);
        let mpn = 5 + nlen;
        if mpn + 9 >= 63 {
            k += 1;
            continue;
        }
        mp[mpn..mpn + 9].copy_from_slice(b"/manifest");
        let f = user_lib::open(mp.as_ptr(), 0);
        if f < 0 {
            k += 1;
            continue;
        }
        let mut mb = [0u8; 2048];
        let mut mn = 0usize;
        loop {
            if mn >= mb.len() {
                break;
            }
            let r = user_lib::read(f, unsafe { mb.as_mut_ptr().add(mn) }, mb.len() - mn);
            if r <= 0 {
                break;
            }
            mn += r as usize;
        }
        user_lib::close(f);
        let mut ver = [0u8; 32];
        let mut layers = [[0u8; 64]; 8];
        let mut layern = [0usize; 8];
        let mut deps = [[0u8; 64]; 8];
        let mut depn = [0usize; 8];
        let mut sha_ign = [0u8; 64];
        let (_, _, ndeps, _) = parse_manifest(&mb, mn, &mut ver, &mut layers, &mut layern, &mut deps, &mut depn, &mut sha_ign);
        let mut d = 0;
        while d < ndeps {
            let tok = &deps[d][..depn[d]];
            let mut e = 0;
            while e < tok.len() && tok[e] != b'=' {
                e += 1;
            }
            if &tok[..e] == pkg {
                let m = nlen.min(31);
                out[..m].copy_from_slice(&entry[..m]);
                out[m] = 0;
                return true;
            }
            d += 1;
        }
        k += 1;
    }
    false
}

/// v3.3: installed package names from /pkg/db into `out`.
/// Returns count (cap 32).
fn pkg_db_names(out: &mut [[u8; 64]; 32]) -> usize {
    let f = user_lib::open(b"/pkg/db\0".as_ptr(), 0);
    if f < 0 {
        return 0;
    }
    let mut b = [0u8; 2048];
    let mut n = 0usize;
    loop {
        if n >= b.len() {
            break;
        }
        let r = user_lib::read(f, unsafe { b.as_mut_ptr().add(n) }, b.len() - n);
        if r <= 0 {
            break;
        }
        n += r as usize;
    }
    user_lib::close(f);
    let mut nn = 0;
    let mut i = 0;
    while i < n && nn < out.len() {
        let mut j = i;
        while j < n && b[j] != b'\n' {
            j += 1;
        }
        let mut e = i;
        while e < j && b[e] != b' ' {
            e += 1;
        }
        if e > i {
            let m = (e - i).min(63);
            out[nn][..m].copy_from_slice(&b[i..i + m]);
            out[nn][m] = 0;
            nn += 1;
        }
        i = j + 1;
    }
    nn
}

/// v3.3: shared remove body (unlink /bin links + delete store).
/// No db touch, no prints, no exits -- callers own those.
fn pkg_remove_files(pkg: &[u8]) -> bool {
    let mut root = [0u8; 64];
    root[..5].copy_from_slice(b"/pkg/");
    root[5..5 + pkg.len()].copy_from_slice(pkg);
    let rootn = 5 + pkg.len();
    let mut binpath = [0u8; 64];
    binpath[..rootn].copy_from_slice(&root[..rootn]);
    binpath[rootn..rootn + 4].copy_from_slice(b"/bin");
    let mut names = [[0u8; 64]; 32];
    let nn = dir_names(&binpath, &mut names);
    let mut k = 0usize;
    while k < nn {
        let mut nlen = 0;
        while nlen < 64 && names[k][nlen] != 0 {
            nlen += 1;
        }
        let mut dst = [0u8; 64];
        dst[..5].copy_from_slice(b"/bin/");
        dst[5..5 + nlen].copy_from_slice(&names[k][..nlen]);
        user_lib::unlink(dst.as_ptr());
        k += 1;
    }
    rm_all(&root)
}

/// `ctr remove <pkg>`: unlink the /bin links, delete the store,
/// strip the db line. Prints `pkg-removed <name>`.
/// v3.1: refuses while another installed package depends on pkg
/// (`ctr: needed by <other>`), found via the stored manifests.
fn cmd_pkg_remove(pkg: &[u8]) {
    if !pkg_db_has(pkg) {
        user_lib::print("ctr: not installed\n");
        user_lib::exit(1);
    }
    let mut needer = [0u8; 32];
    if pkg_needed_by(pkg, &mut needer) {
        user_lib::print("ctr: needed by ");
        let mut nlen = 0;
        while nlen < 32 && needer[nlen] != 0 {
            nlen += 1;
        }
        let _ = user_lib::write(1, needer.as_ptr(), nlen);
        user_lib::print("\n");
        user_lib::exit(1);
    }
    if !pkg_remove_files(pkg) {
        user_lib::print("ctr: rm failed\n");
        user_lib::exit(1);
    }
    if !pkg_db_del(pkg) {
        user_lib::print("ctr: db update failed\n");
        user_lib::exit(1);
    }
    user_lib::print("pkg-removed ");
    let _ = user_lib::write(1, pkg.as_ptr(), pkg.len());
    user_lib::print("\n");
    user_lib::exit(0);
}

/// v3.3: download one layer tarball to /tmp/p<idx>.tar. True on success.
fn fetch_layer(
    host: &[u8],
    port: &[u8],
    pkg: &[u8],
    ver: &[u8],
    layer: &[u8],
    idx: usize,
) -> bool {
    let mut rpath = [0u8; 128];
    rpath[..5].copy_from_slice(b"/pkg/");
    rpath[5..5 + pkg.len()].copy_from_slice(pkg);
    let rpn = 5 + pkg.len();
    rpath[rpn] = b'/';
    rpath[rpn + 1..rpn + 1 + ver.len()].copy_from_slice(ver);
    let rpn = rpn + 1 + ver.len();
    rpath[rpn] = b'/';
    rpath[rpn + 1..rpn + 1 + layer.len()].copy_from_slice(layer);
    let mut ob = [0u8; 32];
    ob[..7].copy_from_slice(b"/tmp/p0");
    ob[6] = b'0' + idx as u8;
    ob[7..11].copy_from_slice(b".tar");
    run_wget(host, port, &rpath[..rpn + 1 + layer.len()], &ob[..11])
}

/// v3.3: NUL-terminated path equality.
fn path_eq(a: &[u8; 128], b: &[u8; 128]) -> bool {
    let mut i = 0;
    loop {
        if a[i] != b[i] {
            return false;
        }
        if a[i] == 0 {
            return true;
        }
        i += 1;
        if i >= 128 {
            return true;
        }
    }
}

/// v3.3: delete store files in `old` but absent from `new`
/// (plus their /bin links for bin/ paths).
fn gc_stale_files(root: &[u8; 64], rootn: usize, old: &[[u8; 128]; 64], oldn: usize, new: &[[u8; 128]; 64], newn: usize) {
    let mut k = 0;
    while k < oldn {
        let mut found = false;
        let mut q = 0;
        while q < newn {
            if path_eq(&old[k], &new[q]) {
                found = true;
                break;
            }
            q += 1;
        }
        if !found {
            let mut nlen = 0;
            while nlen < 128 && old[k][nlen] != 0 {
                nlen += 1;
            }
            if rootn + 1 + nlen < 63 {
                let mut sp = [0u8; 64];
                sp[..rootn].copy_from_slice(&root[..rootn]);
                sp[rootn] = b'/';
                sp[rootn + 1..rootn + 1 + nlen].copy_from_slice(&old[k][..nlen]);
                user_lib::unlink(sp.as_ptr());
            }
            // bin/<f> -> drop the /bin/<f> link too.
            if nlen > 4 && &old[k][..4] == b"bin/" {
                let rest = &old[k][4..nlen];
                if 5 + rest.len() < 63 {
                    let mut dst = [0u8; 64];
                    dst[..5].copy_from_slice(b"/bin/");
                    dst[5..5 + rest.len()].copy_from_slice(rest);
                    user_lib::unlink(dst.as_ptr());
                }
            }
        }
        k += 1;
    }
}

/// v3.3: refresh every /bin link from store/bin (unlink + link each).
/// Overwrites via O_TRUNC mint new inodes, so pre-upgrade links are
/// stale and must all be re-pointed. False on any failure.
fn relink_bin(root: &[u8; 64], rootn: usize) -> bool {
    let mut binpath = [0u8; 64];
    binpath[..rootn].copy_from_slice(&root[..rootn]);
    binpath[rootn..rootn + 4].copy_from_slice(b"/bin");
    let mut names = [[0u8; 64]; 32];
    let nn = dir_names(&binpath, &mut names);
    let mut k = 0usize;
    while k < nn {
        let mut nlen = 0;
        while nlen < 64 && names[k][nlen] != 0 {
            nlen += 1;
        }
        let mut src = [0u8; 64];
        src[..rootn + 4].copy_from_slice(&binpath[..rootn + 4]);
        src[rootn + 4] = b'/';
        src[rootn + 5..rootn + 5 + nlen].copy_from_slice(&names[k][..nlen]);
        let mut dst = [0u8; 64];
        dst[..5].copy_from_slice(b"/bin/");
        dst[5..5 + nlen].copy_from_slice(&names[k][..nlen]);
        user_lib::unlink(dst.as_ptr());
        if user_lib::link(src.as_ptr(), dst.as_ptr()) != 0 {
            return false;
        }
        k += 1;
    }
    true
}

/// v3.3: upgrade one installed package to the index maximum.
/// Prints `pkg-upgraded <name> <newver>`, or `already latest <name>`
/// (exit 0 either way; failures exit loud). Flow: new deps first
/// (old store intact on dep failure), then download + overwrite +
/// GC stale files + relink + manifest/files/db refresh.
/// Known gap: dependents' pins are NOT re-verified (see _doc/v3.3.md).
fn upgrade_one(host: &[u8], port: &[u8], pkg: &[u8]) {
    let mut cur = [0u8; 32];
    let curn = pkg_db_ver(pkg, &mut cur);
    if curn == usize::MAX {
        user_lib::print("ctr: not installed\n");
        user_lib::exit(1);
    }
    let manual = pkg_db_manual(pkg);
    let mut vers = [[0u8; 32]; 8];
    let mut versn = [0usize; 8];
    let nv = fetch_index(host, port, pkg, &mut vers, &mut versn);
    if nv == 0 {
        user_lib::print("ctr: no versions\n");
        user_lib::exit(1);
    }
    let mut best = 0;
    let mut vi = 1;
    while vi < nv {
        if vercmp(&vers[vi][..versn[vi]], &vers[best][..versn[best]]) > 0 {
            best = vi;
        }
        vi += 1;
    }
    if vercmp(&vers[best][..versn[best]], &cur[..curn]) <= 0 {
        user_lib::print("already latest ");
        let _ = user_lib::write(1, pkg.as_ptr(), pkg.len());
        user_lib::print("\n");
        return;
    }
    let nb = best;
    // new manifest at the resolved route; must equal the selection.
    let mut mpath = [0u8; 128];
    mpath[..5].copy_from_slice(b"/pkg/");
    mpath[5..5 + pkg.len()].copy_from_slice(pkg);
    let mpn = 5 + pkg.len();
    mpath[mpn] = b'/';
    mpath[mpn + 1..mpn + 1 + versn[nb]].copy_from_slice(&vers[nb][..versn[nb]]);
    let mpn = mpn + 1 + versn[nb];
    mpath[mpn..mpn + 9].copy_from_slice(b"/manifest");
    if !run_wget(host, port, &mpath[..mpn + 9], b"/tmp/manifest") {
        user_lib::print("ctr: pkg fetch failed (manifest)\n");
        user_lib::exit(1);
    }
    let mf = user_lib::open(b"/tmp/manifest\0".as_ptr(), 0);
    if mf < 0 {
        user_lib::print("ctr: pkg fetch failed (manifest-open)\n");
        user_lib::exit(1);
    }
    let mut mb = [0u8; 2048];
    let mut mn = 0usize;
    loop {
        if mn >= mb.len() {
            break;
        }
        let r = user_lib::read(mf, unsafe { mb.as_mut_ptr().add(mn) }, mb.len() - mn);
        if r <= 0 {
            break;
        }
        mn += r as usize;
    }
    user_lib::close(mf);
    let mut ver = [0u8; 32];
    let mut layers = [[0u8; 64]; 8];
    let mut layern = [0usize; 8];
    let mut deps = [[0u8; 64]; 8];
    let mut depn = [0usize; 8];
    let mut usha = [0u8; 64];
    let (vern, nl, ndeps, ushan) =
        parse_manifest(&mb, mn, &mut ver, &mut layers, &mut layern, &mut deps, &mut depn, &mut usha);
    if nl == 0 {
        user_lib::print("ctr: pkg fetch failed (no-layers)\n");
        user_lib::exit(1);
    }
    if vern != versn[nb] || &ver[..vern] != &vers[nb][..versn[nb]] {
        user_lib::print("ctr: version skew (");
        let _ = user_lib::write(1, pkg.as_ptr(), pkg.len());
        user_lib::print(")\n");
        user_lib::exit(1);
    }
    // new deps first (their constraints rule; old store untouched).
    let mut stack = [[0u8; 64]; 8];
    let mut stklen = [0usize; 8];
    {
        let m = pkg.len().min(63);
        stack[0][..m].copy_from_slice(&pkg[..m]);
        stklen[0] = m;
    }
    let mut d = 0;
    while d < ndeps {
        let (dn, dw) = split_vreq(&deps[d][..depn[d]]);
        if dn.is_empty() || !valid_name(dn) {
            user_lib::print("ctr: bad dependency\n");
            user_lib::exit(1);
        }
        if let Some((w, _)) = dw {
            if !ver_valid(w) {
                user_lib::print("ctr: bad dependency version\n");
                user_lib::exit(1);
            }
        }
        install_one(host, port, dn, dw, &mut stack, &mut stklen, 1, false);
        d += 1;
    }
    // download into /tmp (kept), verify, then overwrite the live store.
    let mut root = [0u8; 64];
    root[..5].copy_from_slice(b"/pkg/");
    root[5..5 + pkg.len()].copy_from_slice(pkg);
    let rootn = 5 + pkg.len();
    let up_fail = |msg: &str| -> ! {
        user_lib::print(msg);
        user_lib::exit(1);
    };
    let mut li = 0usize;
    while li < nl {
        if !fetch_layer(host, port, pkg, &ver[..vern], &layers[li][..layern[li]], li) {
            up_fail("ctr: pkg fetch failed (layer)\n");
        }
        li += 1;
    }
    // v3.4: verify BEFORE unpacking (tampered bytes never touch the store).
    // usha/ushan came from this manifest's own parse above.
    if !verify_tars(nl, &usha[..ushan]) {
        up_fail("ctr: upgrade failed (sha)\n");
    }
    let mut li = 0usize;
    while li < nl {
        let mut ob = [0u8; 32];
        ob[..7].copy_from_slice(b"/tmp/p0");
        ob[6] = b'0' + li as u8;
        ob[7..11].copy_from_slice(b".tar");
        let tf = user_lib::open(ob.as_ptr(), 0);
        if tf < 0 {
            up_fail("ctr: pkg fetch failed (layer-open)\n");
        }
        let f = untar(tf, &root[..rootn], rootn);
        user_lib::close(tf);
        if f <= 0 {
            up_fail("ctr: pkg fetch failed (untar)\n");
        }
        li += 1;
    }
    // GC: new file set from the kept tars vs old store list.
    let mut newset = [[0u8; 128]; 64];
    let mut newn = 0usize;
    let mut ok = true;
    let mut li = 0usize;
    while li < nl && ok {
        let mut ob = [0u8; 32];
        ob[..7].copy_from_slice(b"/tmp/p0");
        ob[6] = b'0' + li as u8;
        ob[7..11].copy_from_slice(b".tar");
        if newn >= 64 {
            ok = false;
        } else {
            let got = tar_files(&ob, &mut newset[newn..]);
            if got == usize::MAX || newn + got > 64 {
                ok = false;
            } else {
                newn += got;
            }
        }
        user_lib::unlink(ob.as_ptr());
        li += 1;
    }
    if !ok {
        up_fail("ctr: upgrade failed (scan)\n");
    }
    let mut oldset = [[0u8; 128]; 64];
    let oldn = store_read_files(&root, rootn, &mut oldset);
    gc_stale_files(&root, rootn, &oldset, oldn, &newset, newn);
    if !relink_bin(&root, rootn) {
        up_fail("ctr: upgrade failed (relink)\n");
    }
    if !store_write_files(&root, rootn, &newset, newn) {
        up_fail("ctr: upgrade failed (files)\n");
    }
    // manifest refresh.
    {
        let mut mp = [0u8; 64];
        mp[..rootn].copy_from_slice(&root[..rootn]);
        mp[rootn..rootn + 9].copy_from_slice(b"/manifest");
        let f = user_lib::open(mp.as_ptr(), user_lib::O_CREATE | user_lib::O_TRUNC);
        if f < 0 {
            up_fail("ctr: upgrade failed (manifest)\n");
        }
        let mut w = 0;
        let mut okw = true;
        while w < mn {
            let r = user_lib::write(f, unsafe { mb.as_ptr().add(w) }, mn - w);
            if r <= 0 {
                okw = false;
                break;
            }
            w += r as usize;
        }
        user_lib::close(f);
        if !okw {
            up_fail("ctr: upgrade failed (manifest)\n");
        }
    }
    user_lib::unlink(b"/tmp/manifest\0".as_ptr());
    if !pkg_db_del(pkg) || !pkg_db_add(pkg, &ver[..vern], manual) {
        up_fail("ctr: db update failed\n");
    }
    user_lib::print("pkg-upgraded ");
    let _ = user_lib::write(1, pkg.as_ptr(), pkg.len());
    user_lib::print(" ");
    let _ = user_lib::write(1, ver.as_ptr(), vern);
    user_lib::print("\n");
}

/// v3.3: `ctr upgrade <host> <port> [<pkg>]` -- single package or all.
fn cmd_upgrade(host: &[u8], port: &[u8], pkg_opt: Option<&[u8]>) {
    match pkg_opt {
        Some(pkg) => {
            if !valid_name(pkg) {
                user_lib::print("ctr: bad name\n");
                user_lib::exit(1);
            }
            upgrade_one(host, port, pkg);
        }
        None => {
            let mut names = [[0u8; 64]; 32];
            let nn = pkg_db_names(&mut names);
            let mut k = 0;
            while k < nn {
                let mut nlen = 0;
                while nlen < 64 && names[k][nlen] != 0 {
                    nlen += 1;
                }
                upgrade_one(host, port, &names[k][..nlen]);
                k += 1;
            }
        }
    }
    user_lib::exit(0);
}

/// v3.3: `ctr autoremove` -- delete AUTO-installed packages no other
/// installed package depends on (fixpoint: chains collapse pass by
/// pass, cap 16). Manually installed packages are roots and never
/// collected. Prints `autoremove <name>` per removal; silent with
/// exit 0 when there is nothing to do.
fn cmd_autoremove() {
    let mut pass = 0;
    loop {
        let mut names = [[0u8; 64]; 32];
        let nn = pkg_db_names(&mut names);
        let mut needer = [0u8; 32];
        let mut removed = false;
        let mut k = 0usize;
        while k < nn {
            let mut nlen = 0;
            while nlen < 64 && names[k][nlen] != 0 {
                nlen += 1;
            }
            let nm = &names[k][..nlen];
            if pkg_db_manual(nm) {
                k += 1;
                continue;
            }
            if !pkg_needed_by(nm, &mut needer) {
                if !pkg_remove_files(nm) || !pkg_db_del(nm) {
                    user_lib::print("ctr: autoremove failed (");
                    let _ = user_lib::write(1, nm.as_ptr(), nlen);
                    user_lib::print(")\n");
                    user_lib::exit(1);
                }
                user_lib::print("autoremove ");
                let _ = user_lib::write(1, nm.as_ptr(), nlen);
                user_lib::print("\n");
                removed = true;
            }
            k += 1;
        }
        pass += 1;
        if !removed || pass >= 16 {
            break;
        }
    }
    user_lib::exit(0);
}

/// v3.4: `ctr login <token>` -- store a registry token at /pkg/token
/// (attached as a bearer header by run_wget). Token charset
/// [A-Za-z0-9-_], 1-63 bytes (no CRLF smuggling into HTTP headers).
/// Prints `login ok`.
fn cmd_login(tok: &[u8]) {
    if tok.is_empty() || tok.len() > 63 {
        user_lib::print("ctr: bad token\n");
        user_lib::exit(1);
    }
    for &c in tok {
        let ok = (c >= b'0' && c <= b'9')
            || (c >= b'A' && c <= b'Z')
            || (c >= b'a' && c <= b'z')
            || c == b'-'
            || c == b'_';
        if !ok {
            user_lib::print("ctr: bad token\n");
            user_lib::exit(1);
        }
    }
    mkdir_p(b"/pkg\0");
    let f = user_lib::open(b"/pkg/token\0".as_ptr(), user_lib::O_CREATE | user_lib::O_TRUNC);
    if f < 0 {
        user_lib::print("ctr: login failed\n");
        user_lib::exit(1);
    }
    let mut w = 0;
    while w < tok.len() {
        let r = user_lib::write(f, unsafe { tok.as_ptr().add(w) }, tok.len() - w);
        if r <= 0 {
            user_lib::close(f);
            user_lib::print("ctr: login failed\n");
            user_lib::exit(1);
        }
        w += r as usize;
    }
    user_lib::close(f);
    user_lib::print("login ok\n");
    user_lib::exit(0);
}

/// v3.5: `ctr volume create|rm|ls` + `run -v` (bind host dirs into
/// the jail; see _doc/v3.5.md). Volumes are plain `/vol/<v>/` dirs.

/// build `/vol/<v>` (NUL-terminated) into `out`.
fn vol_path(v: &[u8], out: &mut [u8; 64]) {
    out[..5].copy_from_slice(b"/vol/");
    out[5..5 + v.len()].copy_from_slice(v);
    out[5 + v.len()] = 0;
}

/// v3.5: volume existence via the PARENT listing (getdents returns 0,
/// not -1, for missing paths -- it cannot tell "missing dir" from
/// "empty dir", so probe /vol for the name instead).
fn vol_exists(v: &[u8]) -> bool {
    let mut names = [[0u8; 64]; 32];
    let nn = dir_names(b"/vol\0", &mut names);
    let mut k = 0usize;
    while k < nn {
        let mut nlen = 0;
        while nlen < 64 && names[k][nlen] != 0 {
            nlen += 1;
        }
        if nlen == v.len() && &names[k][..nlen] == v {
            return true;
        }
        k += 1;
    }
    false
}

fn cmd_vol_create(v: &[u8]) {
    mkdir_p(b"/vol\0");
    if vol_exists(v) {
        user_lib::print("ctr: volume exists\n");
        user_lib::exit(1);
    }
    let mut root = [0u8; 64];
    vol_path(v, &mut root);
    mkdir_p(&root);
    if !vol_exists(v) {
        user_lib::print("ctr: volume create failed\n");
        user_lib::exit(1);
    }
    user_lib::print("vol-created ");
    let _ = user_lib::write(1, v.as_ptr(), v.len());
    user_lib::print("\n");
    user_lib::exit(0);
}

fn cmd_vol_rm(v: &[u8]) {
    if !vol_exists(v) {
        user_lib::print("ctr: no such volume\n");
        user_lib::exit(1);
    }
    let mut root = [0u8; 64];
    vol_path(v, &mut root);
    if !rm_all(&root) {
        user_lib::print("ctr: rm failed\n");
        user_lib::exit(1);
    }
    user_lib::print("vol-removed ");
    let _ = user_lib::write(1, v.as_ptr(), v.len());
    user_lib::print("\n");
    user_lib::exit(0);
}

fn cmd_vol_ls() {
    user_lib::print("VOL NAME\n");
    let mut names = [[0u8; 64]; 32];
    let nn = dir_names(b"/vol\0", &mut names);
    let mut k = 0usize;
    while k < nn {
        let mut nlen = 0;
        while nlen < 64 && names[k][nlen] != 0 {
            nlen += 1;
        }
        if nlen > 0 {
            user_lib::print("VOL ");
            let _ = user_lib::write(1, names[k].as_ptr(), nlen);
            user_lib::print("\n");
        }
        k += 1;
    }
    user_lib::exit(0);
}

/// v3.5: one `-v` bind (NUL-terminated sides).
#[derive(Clone, Copy)]
pub struct VolBind {
    pub vol: [u8; 64],   // volume name (for /vol/<v>)
    pub cpath: [u8; 64], // jail-absolute target (starts with /)
}

/// `ctr list`: installed packages (`PKGLS NAME VERSION` + rows).
/// A missing db is an empty list, exit 0.
/// v3.3: prints name + version only (the manual/auto flag stays hidden).
fn cmd_pkg_list() {
    user_lib::print("PKGLS NAME VERSION\n");
    let f = user_lib::open(b"/pkg/db\0".as_ptr(), 0);
    if f < 0 {
        user_lib::exit(0);
    }
    let mut b = [0u8; 2048];
    let mut n = 0usize;
    loop {
        if n >= b.len() {
            break;
        }
        let r = user_lib::read(f, unsafe { b.as_mut_ptr().add(n) }, b.len() - n);
        if r <= 0 {
            break;
        }
        n += r as usize;
    }
    user_lib::close(f);
    let mut i = 0;
    while i < n {
        let mut j = i;
        while j < n && b[j] != b'\n' {
            j += 1;
        }
        if j > i {
            // first two fields only.
            let mut e = i;
            while e < j && b[e] != b' ' {
                e += 1;
            }
            let mut e2 = if e < j { e + 1 } else { j };
            while e2 < j && b[e2] != b' ' {
                e2 += 1;
            }
            if e2 > i {
                user_lib::print("PKGLS ");
                let _ = user_lib::write(1, unsafe { b.as_ptr().add(i) }, e2 - i);
                user_lib::print("\n");
            }
        }
        i = j + 1;
    }
    user_lib::exit(0);
}

/// v2.9: `ctr logs <name>` -- dump /ctr/<name>/log to stdout.
/// Liveness is NOT required (stopped containers keep their log until
/// rm, docker-style). Missing log -> loud, exit 1.
fn cmd_logs(name: &[u8]) {
    let mut lp = [0u8; 64];
    log_path(name, &mut lp);
    let f = user_lib::open(lp.as_ptr(), 0);
    if f < 0 {
        user_lib::print("ctr: no log\n");
        user_lib::exit(1);
    }
    let mut b = [0u8; 64];
    loop {
        let r = user_lib::read(f, b.as_mut_ptr(), b.len());
        if r <= 0 {
            break;
        }
        let mut w = 0usize;
        while w < r as usize {
            let k = user_lib::write(1, unsafe { b.as_ptr().add(w) }, r as usize - w);
            if k <= 0 {
                user_lib::close(f);
                user_lib::exit(1);
            }
            w += k as usize;
        }
    }
    user_lib::close(f);
    user_lib::exit(0);
}

/// v2.9: `ctr exec <name> <prog> [args...]` -- run a process inside a
/// RUNNING container (its pid ns, rootfs, and cgroup), stdio inherited,
/// exit code passed through like a foreground run. Refuses stopped or
/// unknown containers (`ctr: not running`, distinct from run's
/// `no such image` so the suite can tell them apart).
fn cmd_exec(name: &[u8], prog: &[u8], args: &[&[u8]]) {
    let (pid, start, cg) = match read_state(name) {
        Some(t) => t,
        None => {
            user_lib::print("ctr: no such container\n");
            user_lib::exit(1);
        }
    };
    if !state_alive(pid, start) {
        user_lib::print("ctr: not running\n");
        user_lib::exit(1);
    }
    // same jail root as run (must exist; the container is alive, so it does).
    let mut root = [0u8; 64];
    root[..5].copy_from_slice(b"/ctr/");
    root[5..5 + name.len()].copy_from_slice(name);
    root[5 + name.len()] = 0;
    let mut probe = [0u8; 64];
    if user_lib::getdents(root.as_ptr(), probe.as_mut_ptr(), 64) < 0 {
        user_lib::print("ctr: no such image (pull first)\n");
        user_lib::exit(1);
    }
    let mut progpath = [0u8; 64];
    let mut toks = [[0u8; 64]; 16];
    let mut av: [*const u8; 17] = [core::ptr::null(); 17];
    build_argv(prog, args, &mut progpath, &mut toks, &mut av);
    let cpid = user_lib::fork();
    if cpid == 0 {
        // join first (ns from the live pid1, group from the state
        // file), then jail + exec. Any failure is loud, exit 127.
        if user_lib::nsenter(pid as isize) != 0 {
            user_lib::print("ctr: nsenter failed\n");
            user_lib::exit(127);
        }
        if user_lib::cgenter(cg as isize) != 0 {
            user_lib::print("ctr: cgenter failed\n");
            user_lib::exit(127);
        }
        if user_lib::chroot(root.as_ptr()) != 0 {
            user_lib::print("ctr: chroot failed\n");
            user_lib::exit(127);
        }
        if user_lib::chdir(b"/\0".as_ptr()) != 0 {
            user_lib::print("ctr: chdir failed\n");
            user_lib::exit(127);
        }
        let _ = user_lib::exec(progpath.as_ptr(), av.as_ptr() as usize);
        user_lib::print("ctr: exec failed\n");
        user_lib::exit(127);
    } else if cpid > 0 {
        // stdio inherited; exit code passed through (foreground semantics).
        let code = wait_for(cpid);
        if code < 0 {
            user_lib::exit(1);
        }
        user_lib::exit(code);
    } else {
        user_lib::print("ctr: fork failed\n");
        user_lib::exit(1);
    }
}

#[no_mangle]
pub extern "C" fn main(argc: usize, argv: *const *const u8) {
    if argc < 2 {
        user_lib::print("usage: ctr <name> | ctr run [-d] [--memory N] [--cpu P] [--weight W] <name> <prog> [args...] | ctr pull <host> <port> <image> | ctr ps | ctr stop <name> | ctr rm <name> | ctr logs <name> | ctr exec <name> <prog> [args...] | ctr install <host> <port> <pkg>[=ver] | ctr remove <pkg> | ctr list | ctr upgrade <host> <port> [<pkg>] | ctr autoremove | ctr login <token> | ctr volume create <v> | ctr volume rm <v> | ctr volume ls\n");
        user_lib::exit(1);
    }
    let a1 = match unsafe { user_lib::argv_str(argv, 1, argc) } {
        Some(s) => s,
        None => {
            user_lib::print("ctr: bad args\n");
            user_lib::exit(1);
        }
    };
    if a1 == b"run" {
        // ctr run [-d] [--memory N] [--cpu P] [--weight W] <name> <prog> [args...]
        // flags in any order, before <name>; values as separate tokens
        // or --flag=N. See _doc/v2.8.md §1.
        let mut k = 2usize;
        let mut detached = false;
        let mut quota = Quota {
            mem_frames: 0,
            mem_set: false,
            cpu_pct: 100,
            cpu_set: false,
            weight: 1,
            weight_set: false,
        };
        // take a flag value: `--memory=N` suffix wins, else next argv.
        // Returns (value-bytes, next-k) or exits loudly.
        // v3.5: `-v SPEC` / `-v=SPEC` (volumes, SPEC = V:C, cap 4).
        let mut vols = [VolBind { vol: [0u8; 64], cpath: [0u8; 64] }; 4];
        let mut nvols = 0usize;
        while k < argc {
            let s = unsafe { user_lib::argv_str(argv, k, argc).unwrap_or(b"") };
            if s == b"-d" {
                detached = true;
                k += 1;
                continue;
            }
            if s == b"-v" || starts_with(s, b"-v=") {
                let inline = strip_prefix_eq(s, b"-v=");
                let vs = if !inline.is_empty() {
                    k += 1;
                    inline
                } else {
                    if k + 1 >= argc {
                        bad_flag();
                    }
                    let v = unsafe { user_lib::argv_str(argv, k + 1, argc).unwrap_or(b"") };
                    k += 2;
                    v
                };
                // split FIRST ':' (cpath may not contain another? it may
                // not matter -- first split is the documented rule).
                let mut e = 0;
                while e < vs.len() && vs[e] != b':' {
                    e += 1;
                }
                if nvols >= 4 || e == 0 || e >= vs.len() {
                    user_lib::print("ctr: bad -v (want V:/cpath)\n");
                    user_lib::exit(1);
                }
                let (vn, cn) = (&vs[..e], &vs[e + 1..]);
                if !valid_name(vn) || cn.is_empty() || cn[0] != b'/' {
                    user_lib::print("ctr: bad -v (want V:/cpath)\n");
                    user_lib::exit(1);
                }
                let m = vn.len().min(63);
                vols[nvols].vol[..m].copy_from_slice(&vn[..m]);
                vols[nvols].vol[m] = 0;
                let m = cn.len().min(63);
                vols[nvols].cpath[..m].copy_from_slice(&cn[..m]);
                vols[nvols].cpath[m] = 0;
                nvols += 1;
                continue;
            }
            let which: u8; // 1=mem 2=cpu 3=weight
            let inline: &[u8];
            if s == b"--memory" || starts_with(s, b"--memory=") {
                which = 1;
                inline = strip_prefix_eq(s, b"--memory=");
            } else if s == b"--cpu" || starts_with(s, b"--cpu=") {
                which = 2;
                inline = strip_prefix_eq(s, b"--cpu=");
            } else if s == b"--weight" || starts_with(s, b"--weight=") {
                which = 3;
                inline = strip_prefix_eq(s, b"--weight=");
            } else {
                break;
            }
            let vs = if !inline.is_empty() {
                k += 1;
                inline
            } else {
                if k + 1 >= argc {
                    bad_flag();
                }
                let v = unsafe { user_lib::argv_str(argv, k + 1, argc).unwrap_or(b"") };
                k += 2;
                v
            };
            if which == 1 {
                match parse_mem(vs) {
                    Some(f) => {
                        quota.mem_frames = f;
                        quota.mem_set = true;
                    }
                    None => bad_flag(),
                }
            } else if which == 2 {
                match parse_dec(vs) {
                    Some(p) if p <= 100 => {
                        quota.cpu_pct = p;
                        quota.cpu_set = true;
                    }
                    _ => {
                        user_lib::print("ctr: bad --cpu (0-100)\n");
                        user_lib::exit(1);
                    }
                }
            } else {
                // v2.8: no client-side range check -- the kernel owns
                // 1-1000 (returns -1 outside it) and cmd_run reports
                // `ctr: cgsetshare failed` loudly. Unparseable input
                // still fails here. See _doc/v2.8.md §1.
                match parse_dec(vs) {
                    Some(w) => {
                        quota.weight = w;
                        quota.weight_set = true;
                    }
                    _ => {
                        user_lib::print("ctr: bad --weight (1-1000)\n");
                        user_lib::exit(1);
                    }
                }
            }
        }
        if argc < k + 2 {
            user_lib::print("usage: ctr run [-d] [--memory N] [--cpu P] [--weight W] [-v V:/cpath] <name> <prog> [args...]\n");
            user_lib::exit(1);
        }
        let name = unsafe { user_lib::argv_str(argv, k, argc).unwrap_or(b"") };
        let prog = unsafe { user_lib::argv_str(argv, k + 1, argc).unwrap_or(b"") };
        if !valid_name(name) || prog.is_empty() {
            user_lib::print("ctr: bad name or prog\n");
            user_lib::exit(1);
        }
        let mut extra: [&[u8]; 15] = [b""; 15];
        let mut ne = 0usize;
        let mut q = k + 2;
        while q < argc && ne < 15 {
            extra[ne] = unsafe { user_lib::argv_str(argv, q, argc).unwrap_or(b"") };
            ne += 1;
            q += 1;
        }
        cmd_run(name, prog, &extra[..ne], detached, &quota, &vols, nvols);
    } else if a1 == b"ps" {
        // ctr ps (no args)
        if argc != 2 {
            user_lib::print("usage: ctr ps\n");
            user_lib::exit(1);
        }
        cmd_ps();
        user_lib::exit(0);
    } else if a1 == b"stop" {
        // ctr stop <name>
        if argc != 3 {
            user_lib::print("usage: ctr stop <name>\n");
            user_lib::exit(1);
        }
        let name = unsafe { user_lib::argv_str(argv, 2, argc).unwrap_or(b"") };
        if !valid_name(name) {
            user_lib::print("ctr: bad name\n");
            user_lib::exit(1);
        }
        cmd_stop(name);
    } else if a1 == b"rm" {
        // ctr rm <name>
        if argc != 3 {
            user_lib::print("usage: ctr rm <name>\n");
            user_lib::exit(1);
        }
        let name = unsafe { user_lib::argv_str(argv, 2, argc).unwrap_or(b"") };
        if !valid_name(name) {
            user_lib::print("ctr: bad name\n");
            user_lib::exit(1);
        }
        cmd_rm(name);
    } else if a1 == b"volume" {
        // v3.5: ctr volume create <v> | ctr volume rm <v> | ctr volume ls
        if argc < 3 {
            user_lib::print("usage: ctr volume create <v> | ctr volume rm <v> | ctr volume ls\n");
            user_lib::exit(1);
        }
        let sub = unsafe { user_lib::argv_str(argv, 2, argc).unwrap_or(b"") };
        if sub == b"ls" {
            if argc != 3 {
                user_lib::print("usage: ctr volume ls\n");
                user_lib::exit(1);
            }
            cmd_vol_ls();
        } else if sub == b"create" || sub == b"rm" {
            if argc != 4 {
                user_lib::print("usage: ctr volume create <v> | ctr volume rm <v>\n");
                user_lib::exit(1);
            }
            let v = unsafe { user_lib::argv_str(argv, 3, argc).unwrap_or(b"") };
            if !valid_name(v) {
                user_lib::print("ctr: bad name\n");
                user_lib::exit(1);
            }
            if sub == b"create" {
                cmd_vol_create(v);
            } else {
                cmd_vol_rm(v);
            }
        } else {
            user_lib::print("usage: ctr volume create <v> | ctr volume rm <v> | ctr volume ls\n");
            user_lib::exit(1);
        }
    } else if a1 == b"logs" {
        // v2.9: ctr logs <name>
        if argc != 3 {
            user_lib::print("usage: ctr logs <name>\n");
            user_lib::exit(1);
        }
        let name = unsafe { user_lib::argv_str(argv, 2, argc).unwrap_or(b"") };
        if !valid_name(name) {
            user_lib::print("ctr: bad name\n");
            user_lib::exit(1);
        }
        cmd_logs(name);
    } else if a1 == b"exec" {
        // v2.9: ctr exec <name> <prog> [args...]
        if argc < 4 {
            user_lib::print("usage: ctr exec <name> <prog> [args...]\n");
            user_lib::exit(1);
        }
        let name = unsafe { user_lib::argv_str(argv, 2, argc).unwrap_or(b"") };
        let prog = unsafe { user_lib::argv_str(argv, 3, argc).unwrap_or(b"") };
        if !valid_name(name) || prog.is_empty() {
            user_lib::print("ctr: bad name or prog\n");
            user_lib::exit(1);
        }
        let mut extra: [&[u8]; 15] = [b""; 15];
        let mut ne = 0usize;
        let mut q = 4usize;
        while q < argc && ne < 15 {
            extra[ne] = unsafe { user_lib::argv_str(argv, q, argc).unwrap_or(b"") };
            ne += 1;
            q += 1;
        }
        cmd_exec(name, prog, &extra[..ne]);
    } else if a1 == b"pull" {
        // ctr pull <host> <port> <image>
        if argc != 5 {
            user_lib::print("usage: ctr pull <host> <port> <image>\n");
            user_lib::exit(1);
        }
        let host = unsafe { user_lib::argv_str(argv, 2, argc).unwrap_or(b"") };
        let port = unsafe { user_lib::argv_str(argv, 3, argc).unwrap_or(b"") };
        let image = unsafe { user_lib::argv_str(argv, 4, argc).unwrap_or(b"") };
        if host.is_empty() || port.is_empty() || !valid_name(image) {
            user_lib::print("ctr: bad host/port/image\n");
            user_lib::exit(1);
        }
        cmd_pull(host, port, image);
    } else if a1 == b"install" {
        // v3.0: ctr install <host> <port> <pkg>; v3.3: <pkg> takes
        // an optional =ver / >=ver pin (unconstrained = latest).
        if argc != 5 {
            user_lib::print("usage: ctr install <host> <port> <pkg>[=ver]\n");
            user_lib::exit(1);
        }
        let host = unsafe { user_lib::argv_str(argv, 2, argc).unwrap_or(b"") };
        let port = unsafe { user_lib::argv_str(argv, 3, argc).unwrap_or(b"") };
        let arg = unsafe { user_lib::argv_str(argv, 4, argc).unwrap_or(b"") };
        if host.is_empty() || port.is_empty() {
            user_lib::print("ctr: bad host/port/pkg\n");
            user_lib::exit(1);
        }
        let (pkg, want) = split_vreq(arg);
        if !valid_name(pkg) {
            user_lib::print("ctr: bad host/port/pkg\n");
            user_lib::exit(1);
        }
        if let Some((w, _)) = want {
            if !ver_valid(w) {
                user_lib::print("ctr: bad pkg version\n");
                user_lib::exit(1);
            }
        }
        // (a bare `>` without `=` degrades to unconstrained in
        // split_vreq -- reject it loudly here instead.)
        {
            let mut q = 0;
            while q < arg.len() {
                if arg[q] == b'>' && (q + 1 >= arg.len() || arg[q + 1] != b'=') {
                    user_lib::print("ctr: bad pkg version\n");
                    user_lib::exit(1);
                }
                q += 1;
            }
        }
        cmd_install(host, port, pkg, want);
    } else if a1 == b"remove" {
        // v3.0: ctr remove <pkg> (packages, not containers -- see cmd_rm)
        if argc != 3 {
            user_lib::print("usage: ctr remove <pkg>\n");
            user_lib::exit(1);
        }
        let pkg = unsafe { user_lib::argv_str(argv, 2, argc).unwrap_or(b"") };
        if !valid_name(pkg) {
            user_lib::print("ctr: bad name\n");
            user_lib::exit(1);
        }
        cmd_pkg_remove(pkg);
    } else if a1 == b"list" {
        // v3.0: ctr list (no args)
        if argc != 2 {
            user_lib::print("usage: ctr list\n");
            user_lib::exit(1);
        }
        cmd_pkg_list();
    } else if a1 == b"upgrade" {
        // v3.3: ctr upgrade <host> <port> [<pkg>]
        if argc != 4 && argc != 5 {
            user_lib::print("usage: ctr upgrade <host> <port> [<pkg>]\n");
            user_lib::exit(1);
        }
        let host = unsafe { user_lib::argv_str(argv, 2, argc).unwrap_or(b"") };
        let port = unsafe { user_lib::argv_str(argv, 3, argc).unwrap_or(b"") };
        if host.is_empty() || port.is_empty() {
            user_lib::print("ctr: bad host/port\n");
            user_lib::exit(1);
        }
        if argc == 5 {
            cmd_upgrade(host, port, Some(unsafe { user_lib::argv_str(argv, 4, argc).unwrap_or(b"") }));
        } else {
            cmd_upgrade(host, port, None);
        }
    } else if a1 == b"autoremove" {
        // v3.3: ctr autoremove (no args)
        if argc != 2 {
            user_lib::print("usage: ctr autoremove\n");
            user_lib::exit(1);
        }
        cmd_autoremove();
    } else if a1 == b"login" {
        // v3.4: ctr login <token>
        if argc != 3 {
            user_lib::print("usage: ctr login <token>\n");
            user_lib::exit(1);
        }
        let tok = unsafe { user_lib::argv_str(argv, 2, argc).unwrap_or(b"") };
        cmd_login(tok);
    } else {
        if !valid_name(a1) {
            user_lib::print("ctr: bad name\n");
            user_lib::exit(1);
        }
        cmd_assemble(a1);
    }
}
