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
/// v2.8: quota flags for `run` (all optional, all default-off).
pub struct Quota {
    pub mem_frames: usize, // 0 = unlimited (skip cglimit)
    pub mem_set: bool,
    pub cpu_pct: usize, // default 100 (skip cgsetcpu unless set)
    pub cpu_set: bool,
    pub weight: usize, // default 1 (skip cgsetshare unless set)
    pub weight_set: bool,
}

fn cmd_run(name: &[u8], prog: &[u8], args: &[&[u8]], detached: bool, q: &Quota) {
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
    // prog resolution (same as the shell): contains `/` -> as-is
    // (jailed by chroot after), bare name -> /bin/<prog>.
    let mut progpath = [0u8; 64];
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
        nul_copy(&mut progpath, prog);
    } else {
        if prog.len() > 57 {
            user_lib::print("ctr: prog too long\n");
            user_lib::exit(1);
        }
        progpath[..5].copy_from_slice(b"/bin/");
        progpath[5..5 + prog.len()].copy_from_slice(prog);
        progpath[5 + prog.len()] = 0;
    }
    // exec argv: argv[0] = prog as typed, then extra args (cap 6).
    let mut toks = [[0u8; 64]; 7];
    nul_copy(&mut toks[0], prog);
    let mut n = 1usize;
    for &a in args {
        if n >= 7 {
            break;
        }
        nul_copy(&mut toks[n], &a[..a.len().min(63)]);
        n += 1;
    }
    let mut av: [*const u8; 8] = [core::ptr::null(); 8];
    let mut i = 0;
    while i < n {
        av[i] = toks[i].as_ptr();
        i += 1;
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
    } else if pid > 0 {
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
            user_lib::print("detached ");
            dbg_num(pid as usize);
            // v2.8: echo applied quotas (defaults for unset: mem=0
            // unlimited, cpu=100, weight=1). Requested==effective:
            // any failed set above already aborted the run.
            user_lib::print(" mem=");
            dbg_num(if q.mem_set { q.mem_frames } else { 0 });
            user_lib::print(" cpu=");
            dbg_num(if q.cpu_set { q.cpu_pct } else { 100 });
            user_lib::print(" weight=");
            dbg_num(if q.weight_set { q.weight } else { 1 });
            user_lib::print("\n");
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

/// run `/bin/wget <host> <port> <path> <outfile>`; true iff exit code 0.
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
    let mut av: [*const u8; 6] = [
        w0.as_ptr(),
        hb.as_ptr(),
        pb.as_ptr(),
        qb.as_ptr(),
        ob.as_ptr(),
        core::ptr::null(),
    ];
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
    user_lib::print("usage: ctr run [-d] [--memory N] [--cpu P] [--weight W] <name> <prog> [args...]\n");
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

#[no_mangle]
pub extern "C" fn main(argc: usize, argv: *const *const u8) {
    if argc < 2 {
        user_lib::print("usage: ctr <name> | ctr run [-d] [--memory N] [--cpu P] [--weight W] <name> <prog> [args...] | ctr pull <host> <port> <image> | ctr ps | ctr stop <name> | ctr rm <name>\n");
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
        while k < argc {
            let s = unsafe { user_lib::argv_str(argv, k, argc).unwrap_or(b"") };
            if s == b"-d" {
                detached = true;
                k += 1;
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
            user_lib::print("usage: ctr run [-d] [--memory N] [--cpu P] [--weight W] <name> <prog> [args...]\n");
            user_lib::exit(1);
        }
        let name = unsafe { user_lib::argv_str(argv, k, argc).unwrap_or(b"") };
        let prog = unsafe { user_lib::argv_str(argv, k + 1, argc).unwrap_or(b"") };
        if !valid_name(name) || prog.is_empty() {
            user_lib::print("ctr: bad name or prog\n");
            user_lib::exit(1);
        }
        let mut extra: [&[u8]; 6] = [b""; 6];
        let mut ne = 0usize;
        let mut q = k + 2;
        while q < argc && ne < 6 {
            extra[ne] = unsafe { user_lib::argv_str(argv, q, argc).unwrap_or(b"") };
            ne += 1;
            q += 1;
        }
        cmd_run(name, prog, &extra[..ne], detached, &quota);
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
    } else {
        if !valid_name(a1) {
            user_lib::print("ctr: bad name\n");
            user_lib::exit(1);
        }
        cmd_assemble(a1);
    }
}
