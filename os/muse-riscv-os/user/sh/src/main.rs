#![no_std]
#![no_main]
use core::arch::global_asm;
global_asm!(r#"
.section .text.entry
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

fn exec_cmd(line: &[u8], n: usize, jobs: &mut [isize; 8], env: &Env) {
    // trim \n
    let mut end = n;
    while end > 0 && (line[end - 1] == b'\n' || line[end - 1] == b'\r' || line[end - 1] == b' ') {
        end -= 1;
    }
    if end == 0 {
        return;
    }
    let cmd = &line[..end];
    // check pipe '|' (quote-aware, v0.7)
    if let Some(p) = find_unquoted(cmd, b'|') {
        run_pipe(&cmd[..p], &cmd[p + 1..], jobs, env);
        return;
    }
    // redirection: prog > file / prog >> file (append) / prog < file
    //   prog 2> file / prog 2>> file (stderr). single file, no pipe combo.
    // v0.7: operator scan is quote-aware. v0.11: quoted filenames may
    // contain spaces; $VAR expands in filenames.
    let mut redir_out: Option<&[u8]> = None;
    let mut redir_in: Option<&[u8]> = None;
    let mut append_out = false;
    let mut redir_fd: isize = 1;
    let mut core_end = end;
    // first unquoted operator wins (either direction), as before
    let gt = find_unquoted(cmd, b'>');
    let lt = find_unquoted(cmd, b'<');
    let use_out = match (gt, lt) {
        (Some(_), None) => true,
        (None, Some(_)) => false,
        (Some(g), Some(l)) => g < l,
        (None, None) => false,
    };
    if gt.is_some() && use_out {
        let i = gt.unwrap();
        let mut j = i + 1;
        if j < end && cmd[j] == b'>' {
            append_out = true;
            j += 1;
        }
        let f = trim(&cmd[j..end]);
        // v0.11: quoted filename runs to matching quote, else next space
        let fl = if !f.is_empty() && (f[0] == b'\'' || f[0] == b'"') {
            let q = f[0];
            let mut k = 1;
            while k < f.len() && f[k] != q {
                k += 1;
            }
            if k < f.len() { k + 1 } else { f.len() }
        } else {
            let mut k = f.len();
            for x in 0..f.len() {
                if f[x] == b' ' {
                    k = x;
                    break;
                }
            }
            k
        };
        redir_out = Some(&f[..fl]);
        // trailing "2>" means stderr
        if i > 0 && cmd[i - 1] == b'2' && (i == 1 || cmd[i - 2] == b' ') {
            redir_fd = 2;
            core_end = i - 1;
        } else {
            core_end = i;
        }
    } else if lt.is_some() {
        let i = lt.unwrap();
        let f = trim(&cmd[i + 1..end]);
        // v0.11: quoted filename runs to matching quote, else next space
        let fl = if !f.is_empty() && (f[0] == b'\'' || f[0] == b'"') {
            let q = f[0];
            let mut k = 1;
            while k < f.len() && f[k] != q {
                k += 1;
            }
            if k < f.len() { k + 1 } else { f.len() }
        } else {
            let mut k = f.len();
            for x in 0..f.len() {
                if f[x] == b' ' {
                    k = x;
                    break;
                }
            }
            k
        };
        redir_in = Some(&f[..fl]);
        core_end = i;
    }
    let core = trim(&cmd[..core_end]);
    // tokenize core into argv (+quote mask), then glob-expand
    let mut toks = [[0u8; 64]; 16];
    let mut lit = [[false; 64]; 16];
    let ntok = tokenize_env(core, &mut toks, &mut lit, env);
    if ntok == 0 {
        return;
    }
    let mut xtoks = [[0u8; 64]; 16];
    let ntok = expand_globs(&toks, ntok, &lit, &mut xtoks);
    if ntok == 0 {
        return;
    }
    let toks = xtoks;
    let mut path = [0u8; 64];
    resolve(&toks[0], &mut path);
    let mut av: [*const u8; 17] = [core::ptr::null(); 17];
    mkargv(&toks, ntok, &mut av);
    let pid = user_lib::fork();
    if pid == 0 {
        // flatten sh env for execve (v0.11); buffers live in parent frame,
        // inherited across fork
        let mut kv = [[0u8; 96]; 16];
        let mut ev: [*const u8; 17] = [core::ptr::null(); 17];
        mkenvp(env, &mut kv, &mut ev);
        // redirections via dup2 (v0.5; v0.11 quoted/$VAR filenames)
        if let Some(f) = redir_out {
            let mut fp = [0u8; 64];
            redir_name(f, env, &mut fp);
            let oflags = if append_out {
                user_lib::O_CREATE | user_lib::O_APPEND | 1
            } else {
                0x40 | 0x200 | 1
            };
            let fd = user_lib::open(fp.as_ptr(), oflags);
            if fd >= 0 {
                user_lib::dup2(fd as isize, redir_fd);
                user_lib::close(fd as isize);
            }
        }
        if let Some(f) = redir_in {
            let mut fp = [0u8; 64];
            redir_name(f, env, &mut fp);
            let fd = user_lib::open(fp.as_ptr(), 0);
            if fd >= 0 {
                user_lib::dup2(fd as isize, 0);
                user_lib::close(fd as isize);
            }
        }
        let _ = user_lib::execve(path.as_ptr(), av.as_ptr() as usize, ev.as_ptr() as usize);
        // try token itself as path (e.g. absolute path typed)
        let _ = user_lib::execve(toks[0].as_ptr(), av.as_ptr() as usize, ev.as_ptr() as usize);
        // v0.9: output-only builtin fallback so run_capture("ps") works
        // (stateful builtins like cd/export stay prompt-only)
        if ntok == 1 && toks[0][0] == b'p' && toks[0][1] == b's' && toks[0][2] == 0 {
            builtin_ps();
            user_lib::exit(0);
        }
        user_lib::print("sh: exec failed\n");
        user_lib::exit(-1);
    } else if pid > 0 {
        user_lib::setfg(pid);
        wait_foreground(pid, jobs);
        user_lib::setfg(user_lib::getpid());
    }
}

fn run_pipe(left: &[u8], right: &[u8], jobs: &mut [isize; 8], env: &Env) {
    let mut fds = [0i32; 2];
    if user_lib::pipe(fds.as_mut_ptr()) != 0 {
        user_lib::print("sh: pipe failed\n");
        return;
    }
    // trim spaces
    let l = trim(left);
    let r = trim(right);
    let p1 = user_lib::fork();
    if p1 == 0 {
        user_lib::dup2(fds[1] as isize, 1);
        user_lib::close(fds[0] as isize);
        user_lib::close(fds[1] as isize);
        exec_simple(l, env);
        user_lib::exit(-1);
    }
    let p2 = user_lib::fork();
    if p2 == 0 {
        user_lib::dup2(fds[0] as isize, 0);
        user_lib::close(fds[0] as isize);
        user_lib::close(fds[1] as isize);
        exec_simple(r, env);
        user_lib::exit(-1);
    }
    user_lib::close(fds[0] as isize);
    user_lib::close(fds[1] as isize);
    user_lib::setfg(p2);
    wait_foreground(p1, jobs);
    wait_foreground(p2, jobs);
    user_lib::setfg(user_lib::getpid());
}

fn trim(s: &[u8]) -> &[u8] {
    let mut a = 0;
    let mut b = s.len();
    while a < b && (s[a] == b' ' || s[a] == b'\n' || s[a] == b'\r') {
        a += 1;
    }
    while b > a && (s[b - 1] == b' ' || s[b - 1] == b'\n' || s[b - 1] == b'\r') {
        b -= 1;
    }
    &s[a..b]
}

// ---- v0.7: shell-local environment (16 entries, passed by ref, no globals)
struct Env {
    n: usize,
    names: [[u8; 32]; 16],
    nlen: [usize; 16],
    vals: [[u8; 64]; 16],
    vlen: [usize; 16],
}

impl Env {
    fn new() -> Self {
        Self {
            n: 0,
            names: [[0; 32]; 16],
            nlen: [0; 16],
            vals: [[0; 64]; 16],
            vlen: [0; 16],
        }
    }
    fn get(&self, name: &[u8]) -> Option<&[u8]> {
        for i in 0..self.n {
            if &self.names[i][..self.nlen[i]] == name {
                return Some(&self.vals[i][..self.vlen[i]]);
            }
        }
        None
    }
    fn set(&mut self, name: &[u8], val: &[u8]) -> bool {
        if name.is_empty() || name.len() > 31 || val.len() > 63 {
            return false;
        }
        for i in 0..self.n {
            if &self.names[i][..self.nlen[i]] == name {
                self.vals[i][..val.len()].copy_from_slice(val);
                self.vlen[i] = val.len();
                return true;
            }
        }
        if self.n >= 16 {
            return false;
        }
        let i = self.n;
        self.names[i][..name.len()].copy_from_slice(name);
        self.nlen[i] = name.len();
        self.vals[i][..val.len()].copy_from_slice(val);
        self.vlen[i] = val.len();
        self.n += 1;
        true
    }
    fn unset(&mut self, name: &[u8]) -> bool {
        for i in 0..self.n {
            if &self.names[i][..self.nlen[i]] == name {
                // swap-remove with last
                let l = self.n - 1;
                if i != l {
                    self.names[i] = self.names[l];
                    self.nlen[i] = self.nlen[l];
                    self.vals[i] = self.vals[l];
                    self.vlen[i] = self.vlen[l];
                }
                self.n -= 1;
                return true;
            }
        }
        false
    }
}

fn is_name_byte(b: u8) -> bool {
    (b >= b'a' && b <= b'z')
        || (b >= b'A' && b <= b'Z')
        || (b >= b'0' && b <= b'9')
        || b == b'_'
}

// first index of `target` outside quotes/backslash-escape. None if absent.
fn find_unquoted(s: &[u8], target: u8) -> Option<usize> {
    let mut q = 0u8; // 0 none, 1 single, 2 double
    let mut i = 0;
    while i < s.len() {
        let b = s[i];
        if q == 1 {
            if b == b'\'' {
                q = 0;
            }
        } else if q == 2 {
            if b == b'"' {
                q = 0;
            } else if b == b'\\' {
                i += 1;
            }
        } else if b == b'\'' {
            q = 1;
        } else if b == b'"' {
            q = 2;
        } else if b == b'\\' {
            i += 1;
        } else if b == target {
            return Some(i);
        }
        i += 1;
    }
    None
}

// split s into up to 8 NUL-terminated tokens with quote/$VAR/backslash
// handling (v0.7). Returns count. Redirection filenames with quoted spaces
// are NOT supported (only argv quoting); see _doc/v0.7.md.
fn tokenize_env(s: &[u8], toks: &mut [[u8; 64]], lit: &mut [[bool; 64]], env: &Env) -> usize {
    let mut n = 0;
    let mut i = 0;
    while i < s.len() && n < toks.len() {
        while i < s.len() && s[i] == b' ' {
            i += 1;
        }
        if i >= s.len() {
            break;
        }
        let mut l = 0;
        let mut q = 0u8;
        while i < s.len() && l < 62 {
            let b = s[i];
            if q == 1 {
                if b == b'\'' {
                    q = 0;
                } else {
                    toks[n][l] = b;
                    lit[n][l] = true;
                    l += 1;
                }
                i += 1;
            } else if q == 2 {
                if b == b'"' {
                    q = 0;
                    i += 1;
                } else if b == b'\\' && i + 1 < s.len() {
                    i += 1;
                    toks[n][l] = s[i];
                    lit[n][l] = true;
                    l += 1;
                    i += 1;
                } else if b == b'$' {
                    i = expand_var(s, i, toks, n, &mut l, lit, env, true);
                } else {
                    toks[n][l] = b;
                    lit[n][l] = true;
                    l += 1;
                    i += 1;
                }
            } else if b == b' ' {
                break;
            } else if b == b'\'' {
                q = 1;
                i += 1;
            } else if b == b'"' {
                q = 2;
                i += 1;
            } else if b == b'\\' && i + 1 < s.len() {
                i += 1;
                toks[n][l] = s[i];
                lit[n][l] = true;
                l += 1;
                i += 1;
            } else if b == b'$' {
                i = expand_var(s, i, toks, n, &mut l, lit, env, false);
            } else {
                toks[n][l] = b;
                lit[n][l] = false;
                l += 1;
                i += 1;
            }
        }
        // token too long: skip rest of it (quoted or not)
        if l >= 62 {
            let mut qq = q;
            while i < s.len() {
                let b = s[i];
                if qq == 1 {
                    if b == b'\'' {
                        qq = 0;
                    }
                } else if qq == 2 {
                    if b == b'"' {
                        qq = 0;
                    } else if b == b'\\' {
                        i += 1;
                    }
                } else if b == b' ' {
                    break;
                } else if b == b'\'' {
                    qq = 1;
                } else if b == b'"' {
                    qq = 2;
                } else if b == b'\\' {
                    i += 1;
                }
                i += 1;
            }
        }
        toks[n][l] = 0;
        n += 1;
    }
    n
}

// expand $NAME at s[i]=='$'; appends value to toks[n] (cap 62 via l).
// Returns new i (past the name, or past '$' if no name follows).
fn expand_var(s: &[u8], i: usize, toks: &mut [[u8; 64]], n: usize, l: &mut usize, lit: &mut [[bool; 64]], env: &Env, litval: bool) -> usize {
    let mut j = i + 1;
    while j < s.len() && is_name_byte(s[j]) {
        j += 1;
    }
    if j == i + 1 {
        // lone '$': literal (never globs)
        if *l < 62 {
            toks[n][*l] = b'$';
            lit[n][*l] = true;
            *l += 1;
        }
        return i + 1;
    }
    if let Some(v) = env.get(&s[i + 1..j]) {
        for &b in v {
            if *l >= 62 {
                break;
            }
            toks[n][*l] = b;
            lit[n][*l] = litval;
            *l += 1;
        }
    }
    j
}

// argv pointers for exec (toks must outlive the call).
fn mkargv(toks: &[[u8; 64]], n: usize, av: &mut [*const u8]) {
    for i in 0..n {
        av[i] = toks[i].as_ptr();
    }
    av[n] = core::ptr::null();
}

// resolve prog token to path: /bin/<prog>, else token itself.
fn resolve(prog: &[u8], path: &mut [u8; 64]) {
    let mut l = 0;
    for &b in b"/bin/" {
        path[l] = b;
        l += 1;
    }
    let mut k = 0;
    while prog[k] != 0 && l < 62 {
        path[l] = prog[k];
        l += 1;
        k += 1;
    }
    path[l] = 0;
}

// ---- v0.11 helpers ----

// flatten sh Env into "NAME=VAL\0" buffers + NULL-terminated ptr array.
fn mkenvp(env: &Env, kv: &mut [[u8; 96]; 16], ptrs: &mut [*const u8; 17]) -> usize {
    let mut n = 0;
    for i in 0..env.n {
        let nl = env.nlen[i].min(31);
        let vl = env.vlen[i].min(63);
        if nl + 1 + vl + 1 > 96 {
            continue;
        }
        kv[n][..nl].copy_from_slice(&env.names[i][..nl]);
        kv[n][nl] = b'=';
        kv[n][nl + 1..nl + 1 + vl].copy_from_slice(&env.vals[i][..vl]);
        kv[n][nl + 1 + vl] = 0;
        ptrs[n] = kv[n].as_ptr();
        n += 1;
    }
    ptrs[n] = core::ptr::null();
    n
}

// $VAR expansion (no quotes, no glob) into NUL-terminated out (cap 64).
fn expand_str(s: &[u8], env: &Env, out: &mut [u8; 64]) -> usize {
    let mut l = 0;
    let mut i = 0;
    while i < s.len() && l < 62 {
        if s[i] == b'$' {
            let mut j = i + 1;
            while j < s.len() && is_name_byte(s[j]) {
                j += 1;
            }
            if j == i + 1 {
                out[l] = b'$';
                l += 1;
                i += 1;
            } else {
                if let Some(v) = env.get(&s[i + 1..j]) {
                    for &b in v {
                        if l >= 62 {
                            break;
                        }
                        out[l] = b;
                        l += 1;
                    }
                }
                i = j;
            }
        } else {
            out[l] = s[i];
            l += 1;
            i += 1;
        }
    }
    out[l] = 0;
    l
}

// redirection filename: strip one quote layer, then $VAR-expand.
fn redir_name(f: &[u8], env: &Env, out: &mut [u8; 64]) {
    let inner: &[u8];
    if f.len() >= 2 && (f[0] == b'\'' || f[0] == b'"') {
        let q = f[0];
        let mut k = 1;
        while k < f.len() && f[k] != q {
            k += 1;
        }
        inner = &f[1..k];
    } else {
        inner = f;
    }
    expand_str(inner, env, out);
}

// true if the NUL-terminated token has * or ? outside quotes/escapes.
fn token_globs(tok: &[u8; 64], lit: &[bool; 64]) -> bool {
    let mut k = 0;
    while k < 64 && tok[k] != 0 {
        if !lit[k] && (tok[k] == b'*' || tok[k] == b'?') {
            return true;
        }
        k += 1;
    }
    false
}

fn tok_len(tok: &[u8; 64]) -> usize {
    let mut k = 0;
    while k < 64 && tok[k] != 0 {
        k += 1;
    }
    k
}

// fnmatch with * (any run) and ? (single byte). No char classes.
fn fnmatch(pat: &[u8], name: &[u8]) -> bool {
    let (mut px, mut nx) = (0usize, 0usize);
    let (mut star, mut ss) = (None, 0usize);
    while nx < name.len() {
        if px < pat.len() && (pat[px] == b'?' || pat[px] == name[nx]) {
            px += 1;
            nx += 1;
        } else if px < pat.len() && pat[px] == b'*' {
            star = Some(px);
            px += 1;
            ss = nx;
        } else if let Some(sp) = star {
            px = sp + 1;
            ss += 1;
            nx = ss;
        } else {
            return false;
        }
    }
    while px < pat.len() && pat[px] == b'*' {
        px += 1;
    }
    px == pat.len()
}

// expand unquoted globs via getdents. out[] capped at out.len() argv
// entries; overflow tokens are dropped; zero-match keeps the literal token.
// Returns new argc.
fn expand_globs(
    toks: &[[u8; 64]],
    ntok: usize,
    lit: &[[bool; 64]],
    out: &mut [[u8; 64]],
) -> usize {
    let mut n = 0;
    for t in 0..ntok {
        let tl = tok_len(&toks[t]);
        if n >= out.len() {
            break;
        }
        if !token_globs(&toks[t], &lit[t]) {
            out[n] = toks[t];
            n += 1;
            continue;
        }
        // split dir prefix at last '/'
        let tok = &toks[t][..tl];
        let mut slash: Option<usize> = None;
        for k in 0..tl {
            if tok[k] == b'/' {
                slash = Some(k);
            }
        }
        // dir part ('.' for bare patterns) and pattern part
        let dir: &[u8] = match slash {
            Some(k) => &tok[..k + 1],
            None => b".",
        };
        let pat: &[u8] = match slash {
            Some(k) => &tok[k + 1..],
            None => tok,
        };
        let mut dp = [0u8; 128];
        let dl = dir.len().min(126);
        dp[..dl].copy_from_slice(&dir[..dl]);
        dp[dl] = 0;
        let mut nb = [0u8; 512];
        let r = user_lib::getdents(dp.as_ptr(), nb.as_mut_ptr(), 512);
        if r <= 0 {
            out[n] = toks[t]; // keep literal (nullglob off)
            n += 1;
            continue;
        }
        // walk exactly r NUL-terminated names (never scan past them:
        // the rest of nb[] is uninitialized stack)
        let mut off = 0;
        let mut seen = 0;
        let mut matched = 0;
        while seen < r as usize && off < 511 && n < 8 {
            let mut e = off;
            while e < 512 && nb[e] != 0 {
                e += 1;
            }
            if e >= 512 {
                break;
            }
            seen += 1;
            let name = &nb[off..e];
            off = e + 1;
            if name.is_empty() {
                continue;
            }
            // hidden files only on explicit dot patterns (bash-like)
            if name[0] != b'.' || (!pat.is_empty() && pat[0] == b'.') {
                if fnmatch(pat, name) {
                    let mut o = 0usize;
                    if slash.is_some() {
                        for &b in dir {
                            if o >= 62 {
                                break;
                            }
                            out[n][o] = b;
                            o += 1;
                        }
                    }
                    for &b in name {
                        if o >= 62 {
                            break;
                        }
                        out[n][o] = b;
                        o += 1;
                    }
                    out[n][o] = 0;
                    n += 1;
                    matched += 1;
                }
            }
        }
        if matched == 0 && n < out.len() {
            out[n] = toks[t]; // keep literal (nullglob off)
            n += 1;
        }
    }
    n
}

fn exec_simple(cmd: &[u8], env: &Env) {
    let cmd = trim(cmd);
    let mut toks = [[0u8; 64]; 16];
    let mut lit = [[false; 64]; 16];
    let ntok = tokenize_env(cmd, &mut toks, &mut lit, env);
    if ntok == 0 {
        user_lib::exit(-1);
    }
    let mut xtoks = [[0u8; 64]; 16];
    let ntok = expand_globs(&toks, ntok, &lit, &mut xtoks);
    if ntok == 0 {
        user_lib::exit(-1);
    }
    let toks = xtoks;
    let mut path = [0u8; 64];
    resolve(&toks[0], &mut path);
    let mut av: [*const u8; 17] = [core::ptr::null(); 17];
    mkargv(&toks, ntok, &mut av);
    let mut kv = [[0u8; 96]; 16];
    let mut ev: [*const u8; 17] = [core::ptr::null(); 17];
    mkenvp(env, &mut kv, &mut ev);
    let _ = user_lib::execve(path.as_ptr(), av.as_ptr() as usize, ev.as_ptr() as usize);
    let _ = user_lib::execve(toks[0].as_ptr(), av.as_ptr() as usize, ev.as_ptr() as usize);
    user_lib::print("sh: pipe exec failed\n");
    user_lib::exit(-1);
}

fn run_one(path: &[u8], jobs: &mut [isize; 8]) {
    run_args(path, &[], jobs);
}

// exec path with argv (caller flattens to NUL-terminated bufs)
fn run_args(path: &[u8], args: &[&[u8]], jobs: &mut [isize; 8]) {
    let mut toks = [[0u8; 64]; 16];
    let mut n = 0;
    for &a in args {
        if n >= toks.len() {
            break;
        }
        let m = a.len().min(62);
        toks[n][..m].copy_from_slice(&a[..m]);
        toks[n][m] = 0;
        n += 1;
    }
    let mut av: [*const u8; 17] = [core::ptr::null(); 17];
    mkargv(&toks, n, &mut av);
    let pid = user_lib::fork();
    if pid == 0 {
        let _ = user_lib::exec(path.as_ptr(), av.as_ptr() as usize);
        user_lib::exit(-1);
    } else if pid > 0 {
        user_lib::setfg(pid);
        wait_foreground(pid, jobs);
        user_lib::setfg(user_lib::getpid());
    }
}

fn wait_pid() {
    let mut code: i32 = 0;
    loop {
        let w = user_lib::wait(&mut code as *mut i32);
        if w == -2 {
            user_lib::yield_();
            continue;
        }
        break;
    }
}

// print process table (shared by the `ps` builtin and run_capture, v0.9)
fn builtin_ps() {
    let mut pb = [0u8; 512];
    let r = user_lib::ps(pb.as_mut_ptr(), 512);
    if r > 0 {
        write_bytes(1, &pb[..r as usize]);
    } else {
        user_lib::print("sh: ps failed\n");
    }
}

// pid-aware wait: reaps other (background) children into jobs table.
// v0.5: uses waitpid(target) for precise reap.
// v0.7: also clears the target's own jobs slot (stale reaped pids used to
// linger because only non-target reaps went through bg_done).
fn wait_foreground(pid: isize, jobs: &mut [isize; 8]) {
    let mut code: i32 = 0;
    loop {
        let w = user_lib::waitpid(pid, &mut code as *mut i32, 0);
        if w == -2 {
            user_lib::yield_();
            continue;
        }
        if w < 0 {
            break;
        }
        bg_done(jobs, w);
        if w == pid {
            break;
        }
    }
}

fn bg_done(jobs: &mut [isize; 8], pid: isize) {
    for j in jobs.iter_mut() {
        if *j == pid {
            *j = 0;
            user_lib::print("[bg] done\n");
            break;
        }
    }
}

fn jobs_add(jobs: &mut [isize; 8], pid: isize) -> bool {
    for j in jobs.iter_mut() {
        if *j == 0 {
            *j = pid;
            return true;
        }
    }
    false
}

// write raw bytes to fd (avoids &str UTF-8 constraints)
fn write_bytes(fd: isize, b: &[u8]) {
    if !b.is_empty() {
        user_lib::write(fd, b.as_ptr(), b.len());
    }
}

fn print_isize(v: isize) {
    print_isize_to(1, v);
}

fn print_isize_to(fd: isize, v: isize) {
    let mut tmp = [0u8; 20];
    let mut s = v;
    let neg = s < 0;
    if neg {
        s = -s;
    }
    let mut l = 0;
    if s == 0 {
        tmp[0] = b'0';
        l = 1;
    } else {
        let mut rev = [0u8; 20];
        let mut rl = 0;
        while s > 0 && rl < 20 {
            rev[rl] = b'0' + (s % 10) as u8;
            s /= 10;
            rl += 1;
        }
        let mut k = 0;
        if neg && l < 20 {
            tmp[0] = b'-';
            l = 1;
        }
        while rl > 0 && l < 20 {
            rl -= 1;
            tmp[l] = rev[rl];
            l += 1;
            k += 1;
        }
        let _ = k;
    }
    write_bytes(fd, &tmp[..l]);
}

// bring a bg job to foreground. arg=None => most recent. Returns pid or -1.
fn fg_job(jobs: &mut [isize; 8], arg: Option<isize>) -> isize {
    let target = match arg {
        Some(p) => {
            let mut found = false;
            for j in jobs.iter() {
                if *j == p {
                    found = true;
                    break;
                }
            }
            if !found {
                user_lib::print("fg: no such job\n");
                return -1;
            }
            p
        }
        None => {
            let mut t = -1;
            for j in jobs.iter() {
                if *j != 0 {
                    t = *j;
                }
            }
            if t < 0 {
                user_lib::print("fg: no jobs\n");
                return -1;
            }
            t
        }
    };
    user_lib::setfg(target);
    wait_foreground(target, jobs);
    user_lib::setfg(user_lib::getpid());
    target
}

// ---- v0.7: history (16 lines) + line editor ----
struct Hist {
    lines: [[u8; 128]; 16],
    lens: [usize; 16],
    n: usize, // total pushed (cap display at 16)
}

impl Hist {
    fn new() -> Self {
        Self {
            lines: [[0; 128]; 16],
            lens: [0; 16],
            n: 0,
        }
    }
    fn push(&mut self, line: &[u8]) {
        if line.is_empty() {
            return;
        }
        // skip consecutive duplicates
        if self.n > 0 {
            let l = (self.n - 1) % 16;
            if self.lens[l] == line.len() && self.lines[l][..line.len()] == *line {
                return;
            }
        }
        let i = self.n % 16;
        let m = line.len().min(127);
        self.lines[i][..m].copy_from_slice(&line[..m]);
        self.lens[i] = m;
        self.n += 1;
    }
    // k=1 => newest. None if out of range.
    fn get_rel(&self, k: usize) -> Option<&[u8]> {
        let avail = self.n.min(16);
        if k == 0 || k > avail {
            return None;
        }
        let i = (self.n - k) % 16;
        Some(&self.lines[i][..self.lens[i]])
    }
}

// read one byte with bounded retries (ESC-sequence guard); -1 on give-up
fn read_byte_retry() -> isize {
    let mut b = [0u8; 1];
    let mut tries = 0;
    loop {
        let r = user_lib::read(0, b.as_mut_ptr(), 1);
        if r > 0 {
            return b[0] as isize;
        }
        tries += 1;
        if tries > 200 {
            return -1;
        }
        user_lib::yield_();
    }
}

fn redraw(buf: &[u8]) {
    write_bytes(1, b"\r");
    user_lib::print("sh$ ");
    write_bytes(1, buf);
    write_bytes(1, b"\x1b[K");
}

// prompt line editor: echo, backspace, ESC[A/B history. Returns line len
// (0 = EOF-empty, caller yields). Non-empty lines enter history.
fn read_edit(buf: &mut [u8; 128], hist: &mut Hist) -> usize {
    let mut len = 0;
    let mut nav = 0usize; // 0 = draft, else 1-based history depth
    let mut draft = [0u8; 128];
    let mut draft_len = 0;
    let mut draft_saved = false;
    loop {
        let mut b = [0u8; 1];
        let r = user_lib::read(0, b.as_mut_ptr(), 1);
        if r <= 0 {
            if len == 0 {
                return 0;
            }
            user_lib::yield_();
            continue;
        }
        let c = b[0];
        if c == b'\n' || c == b'\r' {
            write_bytes(1, b"\n");
            break;
        } else if c == 0x7f || c == 0x08 {
            if len > 0 {
                len -= 1;
                write_bytes(1, b"\x08 \x08");
            }
        } else if c == 0x1b {
            // ESC sequence: expect [A (up) / [B (down); ignore the rest
            let c1 = read_byte_retry();
            if c1 != b'[' as isize {
                continue;
            }
            let c2 = read_byte_retry();
            if c2 == b'A' as isize {
                let avail = hist.n.min(16);
                if nav < avail {
                    if !draft_saved {
                        draft[..len].copy_from_slice(&buf[..len]);
                        draft_len = len;
                        draft_saved = true;
                    }
                    nav += 1;
                    if let Some(h) = hist.get_rel(nav) {
                        len = h.len().min(127);
                        buf[..len].copy_from_slice(&h[..len]);
                        redraw(&buf[..len]);
                    } else {
                        nav -= 1;
                    }
                }
            } else if c2 == b'B' as isize {
                if nav > 1 {
                    nav -= 1;
                    if let Some(h) = hist.get_rel(nav) {
                        len = h.len().min(127);
                        buf[..len].copy_from_slice(&h[..len]);
                        redraw(&buf[..len]);
                    }
                } else if nav == 1 {
                    nav = 0;
                    len = draft_len;
                    buf[..len].copy_from_slice(&draft[..len]);
                    redraw(&buf[..len]);
                }
            }
        } else if c >= 0x20 && c < 0x7f {
            if len < 127 {
                buf[len] = c;
                len += 1;
                write_bytes(1, &buf[len - 1..len]);
            }
        }
        // other control bytes ignored (Ctrl-C/D handled by kernel)
    }
    if len > 0 {
        hist.push(&buf[..len]);
    }
    len
}

// run `line` with stdout captured into `out` (cap). Returns bytes captured.
// v0.7 test helper: outputs are small (<1KB), pipe never fills.
fn run_capture(line: &[u8], jobs: &mut [isize; 8], env: &Env, out: &mut [u8]) -> usize {
    let mut fds = [0i32; 2];
    if user_lib::pipe(fds.as_mut_ptr()) != 0 {
        return 0;
    }
    let pid = user_lib::fork();
    if pid == 0 {
        user_lib::dup2(fds[1] as isize, 1);
        user_lib::close(fds[0] as isize);
        user_lib::close(fds[1] as isize);
        let mut dj = [0isize; 8];
        exec_cmd(line, line.len(), &mut dj, env);
        user_lib::exit(0);
    }
    if pid < 0 {
        user_lib::close(fds[0] as isize);
        user_lib::close(fds[1] as isize);
        return 0;
    }
    user_lib::close(fds[1] as isize);
    let mut n = 0;
    loop {
        if n < out.len() {
            let r = user_lib::read(fds[0] as isize, unsafe { out.as_mut_ptr().add(n) }, out.len() - n);
            if r > 0 {
                n += r as usize;
                continue;
            }
        }
        // pipe empty (or full): child done?
        let mut code: i32 = 0;
        let w = user_lib::waitpid(pid, &mut code as *mut i32, 1);
        if w == pid || w < 0 {
            // reaped (or lost): drain once more then stop
            if n < out.len() {
                let r = user_lib::read(fds[0] as isize, unsafe { out.as_mut_ptr().add(n) }, out.len() - n);
                if r > 0 {
                    n += r as usize;
                }
            }
            break;
        }
        user_lib::yield_();
    }
    user_lib::close(fds[0] as isize);
    // restore fg (child's exec_cmd clobbered the shared fg pid)
    user_lib::setfg(user_lib::getpid());
    n
}

// single non-blocking reap attempt (for `jobs` builtin)
fn reap_poll(jobs: &mut [isize; 8]) {
    // v1.4/v1.5: MUST be WNOHANG. A blocking wait() sleeps until ANY child
    // exits -- with an immortal background child (webserver) that is never,
    // and the prompt loop stalls before read_edit (halt bytes rot unread).
    // w==0 (nothing yet) and w<0 (no children) both mean "nothing reaped".
    let mut code: i32 = 0;
    let w = user_lib::waitpid(-1, &mut code as *mut i32, 1);
    if w > 0 {
        bg_done(jobs, w);
    }
}

// fork+exec, return child pid (parent) or -1; no waiting.
fn spawn_one(path: &[u8]) -> isize {
    let pid = user_lib::fork();
    if pid == 0 {
        let _ = user_lib::exec(path.as_ptr(), 0);
        user_lib::exit(-1);
    }
    pid
}

#[no_mangle]
pub extern "C" fn main(argc: usize, argv: *const *const u8) {
    user_lib::print("[USER] sh: Unix-v6 like shell. try: ls, cat /README, echo hi | grep hi, usertests\n");
    user_lib::print("[USER] sh: builtins: cd pwd df jobs fg kill halt export unset env chroot unshare; quotes + $VAR + history\n");
    let mut jobs = [0isize; 8];
    let mut env = Env::new();
    let mut hist = Hist::new();
    // v2.0: respawned shells (init passes "--quick") skip autorun:
    // recovery must take seconds, and test.sh run3 needs the prompt
    // promptly after respawn (else its halt input rots in the pipe).
    let mut quick = false;
    if argc >= 2 {
        if let Some(a) = unsafe { user_lib::argv_str(argv, 1, argc) } {
            if a.len() == 7 && a[0] == b'-' && a[1] == b'-' && a[2] == b'q' {
                quick = true;
            }
        }
    }
    if !quick {
    // auto-run usertests + persist + argv coverage once for test.sh markers
    user_lib::print("[USER] sh: auto-run usertests\n");
    run_one(b"/bin/usertests\0", &mut jobs);
    run_one(b"/bin/smp_test\0", &mut jobs);
    run_one(b"/bin/chroot_test\0", &mut jobs);
    run_one(b"/bin/nstest\0", &mut jobs);
    run_one(b"/bin/cgtest\0", &mut jobs);
    run_one(b"/bin/reclaim_test\0", &mut jobs);
    run_one(b"/bin/stress\0", &mut jobs);
    // v1.5: webserver backgrounds (never exits by design); the test
    // client connects from the host.
    let _ws = spawn_one(b"/bin/webserver\0");
    run_one(b"/bin/udpping\0", &mut jobs);
    run_one(b"/bin/ping\0", &mut jobs);
    // v1.7: online clients (host stubs must be up: test.sh starts them).
    // nslookup resolves via the stub DNS (10.0.2.2:5353); wget/curl fetch
    // /test.txt from the stub HTTP (10.0.2.2:8090).
    run_args(
        b"/bin/nslookup\0",
        &[b"nslookup", b"test.local", b"10.0.2.2", b"15353"],
        &mut jobs,
    );
    run_args(
        b"/bin/wget\0",
        &[b"wget", b"10.0.2.2", b"8090", b"/test.txt", b"/dl.txt"],
        &mut jobs,
    );
    run_args(
        b"/bin/curl\0",
        &[b"curl", b"10.0.2.2", b"8090", b"/test.txt"],
        &mut jobs,
    );
    // v2.3: image suite (registry stub must be up: test.sh starts it).
    // pull the test image, then run its /bin/echo inside a container.
    run_args(
        b"/bin/ctr\0",
        &[b"ctr", b"pull", b"10.0.2.2", b"8091", b"testimg"],
        &mut jobs,
    );
    run_args(
        b"/bin/ctr\0",
        &[b"ctr", b"run", b"testimg", b"/bin/echo", b"hello-from-image"],
        &mut jobs,
    );
    // v2.4: lifecycle suite (`ctr ps/stop/rm` over `run -d`). `life` is
    // assembled (links all of /bin, incl sleeper); sleeper runs 30s but
    // stop kills it at once -- order only, no timing.
    run_args(
        b"/bin/ctr\0",
        &[b"ctr", b"life"],
        &mut jobs,
    );
    run_args(
        b"/bin/ctr\0",
        &[b"ctr", b"run", b"-d", b"--memory", b"100000", b"--cpu", b"50", b"--weight", b"8", b"life", b"/bin/linger", b"30"],
        &mut jobs,
    );
    // v2.8: tiny-cap negative (5 frames can't map sleeper's text+stack;
    // exec fails deterministically -- see _doc/v2.8.md §2). Foreground:
    // no state file, exit 127, suite asserts the `exec failed` marker.
    run_args(
        b"/bin/ctr\0",
        &[b"ctr", b"run", b"--memory", b"5", b"life", b"/bin/sleeper", b"1"],
        &mut jobs,
    );
    // v2.9: exec into the live container (life's linger still runs;
    // must precede `stop life`). Marker: echo output on our console.
    run_args(
        b"/bin/ctr\0",
        &[b"ctr", b"exec", b"life", b"/bin/echo", b"hello-from-exec"],
        &mut jobs,
    );
    run_args(
        b"/bin/ctr\0",
        &[b"ctr", b"ps"],
        &mut jobs,
    );
    run_args(
        b"/bin/ctr\0",
        &[b"ctr", b"stop", b"life"],
        &mut jobs,
    );
    run_args(
        b"/bin/ctr\0",
        &[b"ctr", b"ps"],
        &mut jobs,
    );
    run_args(
        b"/bin/ctr\0",
        &[b"ctr", b"rm", b"life"],
        &mut jobs,
    );
    // v2.9: logs suite (fresh name; linger/sleeper print nothing, so
    // the detached prog is /bin/echo -- its line lands in the log file,
    // `ctr logs` reads it back even after the container exited).
    run_args(
        b"/bin/ctr\0",
        &[b"ctr", b"logtest"],
        &mut jobs,
    );
    run_args(
        b"/bin/ctr\0",
        &[b"ctr", b"run", b"-d", b"logtest", b"/bin/echo", b"hello-from-log"],
        &mut jobs,
    );
    // v2.9: the detached echo may not have been scheduled yet when run
    // -d returns (its parent is reaped, it is reparented to init). Give
    // it 2s to exec+write before reading the log back -- echo needs ms;
    // the margin is for loaded-host MTTCG, same spirit as §11's margins.
    user_lib::sleep(200);
    run_args(
        b"/bin/ctr\0",
        &[b"ctr", b"logs", b"logtest"],
        &mut jobs,
    );
    run_args(
        b"/bin/ctr\0",
        &[b"ctr", b"rm", b"logtest"],
        &mut jobs,
    );
    // v3.0: package suite (install -> list -> run -> cat store -> remove).
    // `hello` is the registry's first package (echo ELF under a
    // collision-free name + hello.txt payload); see _doc/v3.0.md §2.
    // v3.3: pinned to =1.0 (unpinned now means latest, see _doc/v3.3.md).
    run_args(
        b"/bin/ctr\0",
        &[b"ctr", b"install", b"10.0.2.2", b"8091", b"hello=1.0"],
        &mut jobs,
    );
    run_args(
        b"/bin/ctr\0",
        &[b"ctr", b"list"],
        &mut jobs,
    );
    run_args(b"/bin/hello\0", &[b"hello", b"hello-from-pkg"], &mut jobs);
    run_args(b"/bin/cat\0", &[b"cat", b"/pkg/hello/hello.txt"], &mut jobs);
    run_args(
        b"/bin/ctr\0",
        &[b"ctr", b"remove", b"hello"],
        &mut jobs,
    );
    // v3.1: dependency suite (db is clean: v3.0 removed hello above).
    // farewell pulls hello=1.0 first (topological); remove-hello is
    // refused while needed; loopy self-depends (cycle negative).
    run_args(
        b"/bin/ctr\0",
        &[b"ctr", b"install", b"10.0.2.2", b"8091", b"farewell"],
        &mut jobs,
    );
    run_args(
        b"/bin/ctr\0",
        &[b"ctr", b"list"],
        &mut jobs,
    );
    run_args(
        b"/bin/farewell\0",
        &[b"farewell", b"farewell-marker"],
        &mut jobs,
    );
    run_args(
        b"/bin/ctr\0",
        &[b"ctr", b"remove", b"hello"],
        &mut jobs,
    );
    run_args(
        b"/bin/ctr\0",
        &[b"ctr", b"install", b"10.0.2.2", b"8091", b"loopy"],
        &mut jobs,
    );
    run_args(
        b"/bin/ctr\0",
        &[b"ctr", b"remove", b"farewell"],
        &mut jobs,
    );
    run_args(
        b"/bin/ctr\0",
        &[b"ctr", b"remove", b"hello"],
        &mut jobs,
    );
    // v3.2: pipeline package (fortune comes from tools/pkgdemo via
    // pkgbuild, never through the workspace build; see _doc/v3.2.md).
    run_args(
        b"/bin/ctr\0",
        &[b"ctr", b"install", b"10.0.2.2", b"8091", b"fortune"],
        &mut jobs,
    );
    run_args(
        b"/bin/ctr\0",
        &[b"ctr", b"list"],
        &mut jobs,
    );
    run_args(
        b"/bin/fortune\0",
        &[b"fortune", b"hello-from-fortune"],
        &mut jobs,
    );
    run_args(
        b"/bin/ctr\0",
        &[b"ctr", b"remove", b"fortune"],
        &mut jobs,
    );
    // v3.3: upgrade chain (db is clean: v3.1/v3.2 removed everything).
    // hello 1.0 -> 2.0, then a farewell install proving the dep pin
    // still bites (db hello is 2.0, need is =1.0).
    run_args(
        b"/bin/ctr\0",
        &[b"ctr", b"install", b"10.0.2.2", b"8091", b"hello=1.0"],
        &mut jobs,
    );
    run_args(
        b"/bin/ctr\0",
        &[b"ctr", b"upgrade", b"10.0.2.2", b"8091", b"hello"],
        &mut jobs,
    );
    run_args(b"/bin/cat\0", &[b"cat", b"/pkg/hello/hello.txt"], &mut jobs);
    run_args(
        b"/bin/ctr\0",
        &[b"ctr", b"list"],
        &mut jobs,
    );
    run_args(
        b"/bin/ctr\0",
        &[b"ctr", b"install", b"10.0.2.2", b"8091", b"farewell"],
        &mut jobs,
    );
    run_args(
        b"/bin/ctr\0",
        &[b"ctr", b"remove", b"hello"],
        &mut jobs,
    );
    // v3.3: autoremove orphan (install pulls hello=1.0 as a dep;
    // removing farewell orphans it).
    run_args(
        b"/bin/ctr\0",
        &[b"ctr", b"install", b"10.0.2.2", b"8091", b"farewell"],
        &mut jobs,
    );
    run_args(
        b"/bin/ctr\0",
        &[b"ctr", b"remove", b"farewell"],
        &mut jobs,
    );
    run_args(
        b"/bin/ctr\0",
        &[b"ctr", b"autoremove"],
        &mut jobs,
    );
    // v3.4: trust chain (db is clean: autoremove above emptied it).
    // secret is 401 without a token; login unlocks it; tampered dies
    // on sha256 mismatch. See _doc/v3.4.md §2.
    run_args(
        b"/bin/ctr\0",
        &[b"ctr", b"install", b"10.0.2.2", b"8091", b"secret"],
        &mut jobs,
    );
    run_args(
        b"/bin/ctr\0",
        &[b"ctr", b"login", b"test-token"],
        &mut jobs,
    );
    run_args(
        b"/bin/ctr\0",
        &[b"ctr", b"install", b"10.0.2.2", b"8091", b"secret"],
        &mut jobs,
    );
    run_args(
        b"/bin/ctr\0",
        &[b"ctr", b"list"],
        &mut jobs,
    );
    run_args(
        b"/bin/secret\0",
        &[b"secret", b"secret-marker"],
        &mut jobs,
    );
    run_args(
        b"/bin/ctr\0",
        &[b"ctr", b"remove", b"secret"],
        &mut jobs,
    );
    run_args(
        b"/bin/ctr\0",
        &[b"ctr", b"install", b"10.0.2.2", b"8091", b"tampered"],
        &mut jobs,
    );
    // v3.5: volume suite (bind proof: written outside, read inside).
    // greet.txt is created inline (/WD/F precedent below); the volc
    // root is assembled; cat runs jailed with -v data1:/data.
    run_args(
        b"/bin/ctr\0",
        &[b"ctr", b"volume", b"create", b"data1"],
        &mut jobs,
    );
    {
        let fd = user_lib::open(
            b"/vol/data1/greet.txt\0".as_ptr(),
            user_lib::O_CREATE | user_lib::O_TRUNC | 1,
        );
        if fd >= 0 {
            let d = b"hi-vol\n";
            user_lib::write(fd, d.as_ptr(), d.len());
            user_lib::close(fd);
        }
    }
    run_args(
        b"/bin/ctr\0",
        &[b"ctr", b"volc"],
        &mut jobs,
    );
    run_args(
        b"/bin/ctr\0",
        &[
            b"ctr",
            b"run",
            b"-v",
            b"data1:/data",
            b"volc",
            b"/bin/cat",
            b"/data/greet.txt",
        ],
        &mut jobs,
    );
    run_args(
        b"/bin/ctr\0",
        &[b"ctr", b"volume", b"ls"],
        &mut jobs,
    );
    run_args(
        b"/bin/ctr\0",
        &[b"ctr", b"rm", b"volc"],
        &mut jobs,
    );
    run_args(
        b"/bin/ctr\0",
        &[b"ctr", b"volume", b"rm", b"data1"],
        &mut jobs,
    );
    // v3.7: published package (pubdemo is PUT to the live registry by
    // test.sh before boot; the binary inside is still fortune).
    run_args(
        b"/bin/ctr\0",
        &[b"ctr", b"install", b"10.0.2.2", b"8091", b"pubdemo"],
        &mut jobs,
    );
    run_args(
        b"/bin/ctr\0",
        &[b"ctr", b"list"],
        &mut jobs,
    );
    run_args(
        b"/bin/fortune\0",
        &[b"fortune", b"hello-from-pub"],
        &mut jobs,
    );
    run_args(
        b"/bin/ctr\0",
        &[b"ctr", b"remove", b"pubdemo"],
        &mut jobs,
    );
    // v3.9: crates.io end-to-end (repeat's guest-args/guest-fmt come
    // from the registry-published crates, not path; see _doc/v3.9.md).
    run_args(
        b"/bin/ctr\0",
        &[b"ctr", b"install", b"10.0.2.2", b"8091", b"repeat"],
        &mut jobs,
    );
    run_args(
        b"/bin/ctr\0",
        &[b"ctr", b"list"],
        &mut jobs,
    );
    run_args(
        b"/bin/repeat\0",
        &[b"repeat", b"3", b"yo"],
        &mut jobs,
    );
    run_args(
        b"/bin/ctr\0",
        &[b"ctr", b"remove", b"repeat"],
        &mut jobs,
    );
    run_one(b"/bin/persist\0", &mut jobs);
    run_args(b"/bin/cat\0", &[b"cat", b"/TESTDATA"], &mut jobs);
    run_args(b"/bin/echo\0", &[b"echo", b"hello-arg"], &mut jobs);
    // bg coverage: pipe_test background + ls foreground
    let bg = spawn_one(b"/bin/pipe_test\0");
    if bg > 0 {
        if jobs_add(&mut jobs, bg) {
            user_lib::print("[bg] pipe_test &\n");
        }
        run_args(b"/bin/ls\0", &[b"ls"], &mut jobs);
        wait_foreground(bg, &mut jobs);
        user_lib::print("[TEST] bg PASS\n");
    }
    // sleep coverage (blocking sleep, woken by timer)
    user_lib::sleep(5);
    user_lib::print("[TEST] sleep PASS\n");
    // kill coverage: spin child in user mode, kill it, expect -9
    {
        let k = user_lib::fork();
        if k == 0 {
            loop {
                user_lib::yield_();
            }
        } else if k > 0 {
            user_lib::kill(k);
            let mut code: i32 = 0;
            loop {
                let w = user_lib::wait(&mut code as *mut i32);
                if w == -2 {
                    user_lib::yield_();
                    continue;
                }
                break;
            }
            if code == -9 {
                user_lib::print("[TEST] kill PASS\n");
            } else {
                user_lib::print("[TEST] kill FAIL\n");
            }
        }
    }
    // ---- v0.5 coverage: chdir/getcwd, append, lseek, dup2, waitpid, fsstat ----
    {
        user_lib::mkdir(b"/WD\0".as_ptr());
        // create /WD/F with "line1\n"
        let fd = user_lib::open(
            b"/WD/F\0".as_ptr(),
            user_lib::O_CREATE | user_lib::O_TRUNC | 1,
        );
        if fd >= 0 {
            let d = b"line1\n";
            user_lib::write(fd, d.as_ptr(), d.len());
            user_lib::close(fd);
        }
        // chdir + getcwd
        let mut ok_chdir = false;
        if user_lib::chdir(b"/WD\0".as_ptr()) == 0 {
            let mut cb = [0u8; 128];
            if user_lib::getcwd(cb.as_mut_ptr(), 128) > 0
                && cb[0] == b'/' && cb[1] == b'W' && cb[2] == b'D' && cb[3] == 0
            {
                ok_chdir = true;
            }
        }
        // relative open "F" (proves cwd-relative resolve)
        let mut ok_rel = false;
        let fr = user_lib::open(b"F\0".as_ptr(), 0);
        if fr >= 0 {
            let mut rb = [0u8; 6];
            let n = user_lib::read(fr, rb.as_mut_ptr(), 6);
            if n == 6 && rb == *b"line1\n" {
                ok_rel = true;
            }
            user_lib::close(fr);
        }
        if ok_chdir && ok_rel {
            user_lib::print("[TEST] chdir PASS\n");
        } else {
            user_lib::print("[TEST] chdir FAIL\n");
        }
        // append "line2\n" via relative path + O_APPEND
        let fa = user_lib::open(b"F\0".as_ptr(), user_lib::O_CREATE | user_lib::O_APPEND | 1);
        if fa >= 0 {
            let d = b"line2\n";
            user_lib::write(fa, d.as_ptr(), d.len());
            user_lib::close(fa);
        }
        let mut ok_append = false;
        let fr2 = user_lib::open(b"F\0".as_ptr(), 0);
        if fr2 >= 0 {
            let mut rb = [0u8; 12];
            let n = user_lib::read(fr2, rb.as_mut_ptr(), 12);
            if n == 12 && rb == *b"line1\nline2\n" {
                ok_append = true;
            }
            user_lib::close(fr2);
        }
        if ok_append {
            user_lib::print("[TEST] append PASS\n");
        } else {
            user_lib::print("[TEST] append FAIL\n");
        }
        // lseek: SET read first byte, END-1 read last byte
        let mut ok_lseek = false;
        let fl = user_lib::open(b"F\0".as_ptr(), 0);
        if fl >= 0 {
            let mut b1 = [0u8; 1];
            let mut b2 = [0u8; 1];
            user_lib::lseek(fl, 0, 0);
            let n1 = user_lib::read(fl, b1.as_mut_ptr(), 1);
            user_lib::lseek(fl, -1, 2);
            let n2 = user_lib::read(fl, b2.as_mut_ptr(), 1);
            if n1 == 1 && n2 == 1 && b1[0] == b'l' && b2[0] == b'\n' {
                ok_lseek = true;
            }
            // dup2: alias fd 7, rewind via alias, read first byte
            let mut ok_dup2 = false;
            if user_lib::dup2(fl, 7) == 7 {
                let mut b3 = [0u8; 1];
                user_lib::lseek(7, 0, 0);
                let n3 = user_lib::read(7, b3.as_mut_ptr(), 1);
                if n3 == 1 && b3[0] == b'l' {
                    ok_dup2 = true;
                }
                user_lib::close(7);
            }
            if ok_dup2 {
                user_lib::print("[TEST] dup2 PASS\n");
            } else {
                user_lib::print("[TEST] dup2 FAIL\n");
            }
            user_lib::close(fl);
        }
        if ok_lseek {
            user_lib::print("[TEST] lseek PASS\n");
        } else {
            user_lib::print("[TEST] lseek FAIL\n");
        }
        // waitpid: fast-exit child, precise reap + exit code
        {
            let k = user_lib::fork();
            if k == 0 {
                user_lib::exit(42);
            } else if k > 0 {
                let mut code: i32 = 0;
                let mut ok_wp = false;
                loop {
                    let w = user_lib::waitpid(k, &mut code as *mut i32, 0);
                    if w == -2 {
                        user_lib::yield_();
                        continue;
                    }
                    if w == k && code == 42 {
                        ok_wp = true;
                    }
                    break;
                }
                if ok_wp {
                    user_lib::print("[TEST] waitpid PASS\n");
                } else {
                    user_lib::print("[TEST] waitpid FAIL\n");
                }
            }
        }
        // fsstat/df
        {
            let mut sb = [0u8; 64];
            let r = user_lib::fsstat(sb.as_mut_ptr(), 64);
            if r > 6 && sb[0] == b't' && sb[1] == b'o' && sb[2] == b't' && sb[3] == b'a' && sb[4] == b'l' {
                user_lib::print("[TEST] df PASS\n");
            } else {
                user_lib::print("[TEST] df FAIL\n");
            }
        }
        // v1.8: monotonic clock (strictly increasing, sleep(5)=50ms nominal,
        // 30ms floor for emulation slop)
        {
            let t0 = user_lib::uptime_ms();
            user_lib::sleep(5);
            let t1 = user_lib::uptime_ms();
            if t0 >= 0 && t1 > t0 && t1 - t0 >= 30 {
                user_lib::print("[TEST] time PASS\n");
            } else {
                user_lib::print("[TEST] time FAIL\n");
            }
        }
        // back to root for interactive prompt
        user_lib::chdir(b"/\0".as_ptr());
    }
    // ---- v0.7 coverage: quotes, env, fg, history ----
    {
        // quote: single quotes preserve inner spaces as one argv
        // (note: echo appends its own "[TEST] echo PASS" line)
        let mut cb = [0u8; 64];
        let n = run_capture(b"echo 'a  b'\n", &mut jobs, &env, &mut cb);
        if n == 22 && cb[..22] == *b"a  b\n[TEST] echo PASS\n" {
            user_lib::print("[TEST] quote PASS\n");
        } else {
            user_lib::print("[TEST] quote FAIL\n");
        }
        // env: export + $VAR expansion (double quotes + bare)
        env.set(b"F", b"/WD/QQ");
        let mut cb2 = [0u8; 64];
        let n2 = run_capture(b"echo pre-$F-post\n", &mut jobs, &env, &mut cb2);
        if n2 == 33 && cb2[..33] == *b"pre-/WD/QQ-post\n[TEST] echo PASS\n" {
            user_lib::print("[TEST] env PASS\n");
        } else {
            user_lib::print("[TEST] env FAIL\n");
        }
        // fg: fast-exit bg job, bring to foreground, jobs table drains
        let bg2 = exec_bg(b"echo hi", 7, &mut jobs, &env);
        if bg2 > 0 && fg_job(&mut jobs, Some(bg2)) == bg2 {
            let mut drained = true;
            for j in jobs.iter() {
                if *j != 0 {
                    drained = false;
                }
            }
            if drained {
                user_lib::print("[TEST] fg PASS\n");
            } else {
                user_lib::print("[TEST] fg FAIL\n");
            }
        } else {
            user_lib::print("[TEST] fg FAIL\n");
        }
        // history: buffer unit ops (push/nav/dup-skip)
        {
            let mut h = Hist::new();
            h.push(b"aaa");
            h.push(b"bb");
            h.push(b"bb"); // dup skipped
            let ok = h.get_rel(1) == Some(&b"bb"[..])
                && h.get_rel(2) == Some(&b"aaa"[..])
                && h.get_rel(3).is_none();
            if ok {
                user_lib::print("[TEST] history PASS\n");
            } else {
                user_lib::print("[TEST] history FAIL\n");
            }
        }
        // v0.9: ps lists init (pid 1 => line starts with "1 ");
        // strace observes getpid (log assertion in test.sh)
        {
            let mut pb = [0u8; 512];
            let n = run_capture(b"ps\n", &mut jobs, &env, &mut pb);
            let mut found = false;
            let mut ls = 0;
            while ls < n {
                let mut le = ls;
                while le < n && pb[le] != b'\n' {
                    le += 1;
                }
                // line "1 0 R ..." (pid=1 is always init)
                if le > ls + 1 && pb[ls] == b'1' && pb[ls + 1] == b' ' {
                    found = true;
                    break;
                }
                ls = le + 1;
            }
            if found {
                user_lib::print("[TEST] ps PASS\n");
            } else {
                user_lib::print("[TEST] ps FAIL\n");
            }
        }
        {
            let me = user_lib::getpid();
            user_lib::trace(me, 1);
            let _ = user_lib::getpid();
            user_lib::trace(me, 0);
            user_lib::print("[TEST] strace DONE\n");
        }
        // ---- v0.11 coverage: glob, exec envp, quoted/$ redir names ----
        {
            // glob: /WD/* expands (some entry like /WD/F appears)
            let mut gb = [0u8; 128];
            let gn = run_capture(b"echo /WD/*\n", &mut jobs, &env, &mut gb);
            let mut gok = false;
            if gn >= 2 {
                // absolute expansion always contains "/<name>";
                // the bare pattern has no "/F" in it
                for k in 0..gn - 1 {
                    if gb[k] == b'/' && gb[k + 1] == b'F' {
                        gok = true;
                        break;
                    }
                }
            }
            // quoted star stays literal: output starts with "/WD/*\n"
            // (echo appends its own PASS line after)
            let mut qb = [0u8; 64];
            let qn = run_capture(b"echo \"/WD/*\"\n", &mut jobs, &env, &mut qb);
            let qok = qn >= 6
                && qb[0] == b'/'
                && qb[1] == b'W'
                && qb[2] == b'D'
                && qb[3] == b'/'
                && qb[4] == b'*'
                && qb[5] == b'\n';
            if gok && qok {
                user_lib::print("[TEST] glob PASS\n");
            } else {
                user_lib::print("[TEST] glob FAIL\n");
            }
        }
        {
            // exec envp: printenv inherits sh env ("FPE=hello-envp\n" = 15B)
            env.set(b"FPE", b"hello-envp");
            let mut pb = [0u8; 256];
            let n = run_capture(b"printenv FPE\n", &mut jobs, &env, &mut pb);
            let mut ok = false;
            if n >= 15 {
                for k in 0..n - 14 {
                    if pb[k..k + 15] == *b"FPE=hello-envp\n" {
                        ok = true;
                        break;
                    }
                }
            }
            if ok {
                user_lib::print("[TEST] envp PASS\n");
            } else {
                user_lib::print("[TEST] envp FAIL\n");
            }
        }
        {
            // quoted + $VAR redirection filenames. Note: echo appends its
            // own PASS line to the file, so assert on the "data\n" prefix.
            env.set(b"RF", b"/WD/RF");
            let mut z = [0u8; 16];
            let _ = run_capture(b"echo data > $RF\n", &mut jobs, &env, &mut z);
            let mut cb = [0u8; 64];
            let n = run_capture(b"cat \"$RF\"\n", &mut jobs, &env, &mut cb);
            if n >= 5 && cb[..5] == *b"data\n" {
                user_lib::print("[TEST] redirenv PASS\n");
            } else {
                user_lib::print("[TEST] redirenv FAIL\n");
            }
        }
    }
    } // end: if !quick (autorun skipped on respawn)
    // prompt runs as foreground for Ctrl-C
    user_lib::setfg(user_lib::getpid());
    user_lib::print("sh$ ");
    let mut buf = [0u8; 128];
    loop {
        reap_poll(&mut jobs);
        let n = read_edit(&mut buf, &mut hist);
        if n == 0 {
            user_lib::yield_();
            continue;
        }
        // builtins
        let t = trim(&buf[..n]);
        if t.len() == 4 && t[0] == b'h' && t[1] == b'a' && t[2] == b'l' && t[3] == b't' {
            user_lib::print("sh: draining jobs\n");
            drain_jobs(&mut jobs);
            user_lib::print("sh: halting\n");
            user_lib::shutdown();
        }
        if t.len() == 4 && t[0] == b'j' && t[1] == b'o' && t[2] == b'b' && t[3] == b's' {
            reap_poll(&mut jobs);
            let mut any = false;
            for j in jobs.iter() {
                if *j != 0 {
                    any = true;
                    user_lib::print("job pid=");
                    print_isize(*j);
                    user_lib::print("\n");
                }
            }
            if !any {
                user_lib::print("jobs: none\n");
            }
            user_lib::print("sh$ ");
            continue;
        }
        // builtin: cd / pwd / df (v0.5)
        if t.len() == 2 && t[0] == b'c' && t[1] == b'd' {
            let r = user_lib::chdir(b"/\0".as_ptr());
            if r != 0 {
                user_lib::print("sh: cd failed\n");
            }
            user_lib::print("sh$ ");
            continue;
        }
        if t.len() > 3 && t[0] == b'c' && t[1] == b'd' && t[2] == b' ' {
            let arg = trim(&t[3..]);
            let mut pb = [0u8; 128];
            let m = arg.len().min(126);
            pb[..m].copy_from_slice(&arg[..m]);
            pb[m] = 0;
            let r = user_lib::chdir(pb.as_ptr());
            if r != 0 {
                user_lib::print("sh: cd failed\n");
            }
            user_lib::print("sh$ ");
            continue;
        }
        // v2.0/v2.1: chroot <dir> (chdir / to re-anchor), unshare (pid ns)
        if t.len() > 7
            && t[0] == b'c'
            && t[1] == b'h'
            && t[2] == b'r'
            && t[3] == b'o'
            && t[4] == b'o'
            && t[5] == b't'
            && t[6] == b' '
        {
            let arg = trim(&t[7..]);
            let mut pb = [0u8; 128];
            let m = arg.len().min(126);
            pb[..m].copy_from_slice(&arg[..m]);
            pb[m] = 0;
            if user_lib::chroot(pb.as_ptr()) != 0 {
                user_lib::print("sh: chroot failed\n");
            } else {
                let _ = user_lib::chdir(b"/\0".as_ptr());
            }
            user_lib::print("sh$ ");
            continue;
        }
        if t.len() == 7
            && t[0] == b'u'
            && t[1] == b'n'
            && t[2] == b's'
            && t[3] == b'h'
            && t[4] == b'a'
            && t[5] == b'r'
            && t[6] == b'e'
        {
            if user_lib::unshare(user_lib::CLONE_NEWPID) != 0 {
                user_lib::print("sh: unshare failed\n");
            } else {
                user_lib::print("sh: next child starts a new pid ns\n");
            }
            user_lib::print("sh$ ");
            continue;
        }
        if t.len() == 3 && t[0] == b'p' && t[1] == b'w' && t[2] == b'd' {
            let mut cb = [0u8; 128];
            let r = user_lib::getcwd(cb.as_mut_ptr(), 128);
            if r > 0 {
                let mut n = 0;
                while n < 127 && cb[n] != 0 {
                    n += 1;
                }
                let s = unsafe { core::str::from_utf8_unchecked(&cb[..n]) };
                user_lib::print(s);
                user_lib::print("\n");
            } else {
                user_lib::print("sh: pwd failed\n");
            }
            user_lib::print("sh$ ");
            continue;
        }
        if t.len() == 2 && t[0] == b'd' && t[1] == b'f' {
            let mut sb = [0u8; 64];
            let r = user_lib::fsstat(sb.as_mut_ptr(), 64);
            if r > 0 {
                let mut n = 0;
                while n < 63 && sb[n] != 0 && sb[n] != b'\n' {
                    n += 1;
                }
                let s = unsafe { core::str::from_utf8_unchecked(&sb[..n]) };
                user_lib::print(s);
                user_lib::print("\n");
            } else {
                user_lib::print("sh: df failed\n");
            }
            user_lib::print("sh$ ");
            continue;
        }
        // builtin: export NAME=val / unset NAME / env (v0.7)
        if t.len() > 7 && &t[..7] == b"export " {
            let rest = trim(&t[7..]);
            let mut eq: Option<usize> = None;
            for k in 0..rest.len() {
                if rest[k] == b'=' {
                    eq = Some(k);
                    break;
                }
            }
            if let Some(e) = eq {
                let name = trim(&rest[..e]);
                let val = &rest[e + 1..];
                if name.is_empty() || !env.set(name, val) {
                    user_lib::print("sh: export failed\n");
                }
            } else {
                user_lib::print("sh: usage: export NAME=val\n");
            }
            user_lib::print("sh$ ");
            continue;
        }
        if t.len() > 6 && &t[..6] == b"unset " {
            let name = trim(&t[6..]);
            if !env.unset(name) {
                user_lib::print("sh: unset failed\n");
            }
            user_lib::print("sh$ ");
            continue;
        }
        if t.len() == 3 && &t[..3] == b"env" {
            for i in 0..env.n {
                write_bytes(1, &env.names[i][..env.nlen[i]]);
                write_bytes(1, b"=");
                write_bytes(1, &env.vals[i][..env.vlen[i]]);
                write_bytes(1, b"\n");
            }
            user_lib::print("sh$ ");
            continue;
        }
        // builtin: ps (v0.9)
        if t.len() == 2 && &t[..2] == b"ps" {
            builtin_ps();
            user_lib::print("sh$ ");
            continue;
        }
        // builtin: fg [pid] (v0.7)
        if (t.len() == 2 && &t[..2] == b"fg")
            || (t.len() > 3 && &t[..3] == b"fg " && t[2] == b' ')
        {
            let mut arg: Option<isize> = None;
            if t.len() > 3 {
                let mut pid: isize = 0;
                let mut ok = false;
                for &b in trim(&t[3..]) {
                    if b >= b'0' && b <= b'9' {
                        pid = pid * 10 + (b - b'0') as isize;
                        ok = true;
                    } else {
                        ok = false;
                        break;
                    }
                }
                if !ok {
                    user_lib::print("sh: usage: fg [pid]\n");
                    user_lib::print("sh$ ");
                    continue;
                }
                arg = Some(pid);
            }
            fg_job(&mut jobs, arg);
            user_lib::print("sh$ ");
            continue;
        }
        // builtin: kill <pid>
        if t.len() > 5 && t[0] == b'k' && t[1] == b'i' && t[2] == b'l' && t[3] == b'l' && t[4] == b' ' {
            let mut pid: isize = 0;
            let mut ok = false;
            for &b in &t[5..] {
                if b >= b'0' && b <= b'9' {
                    pid = pid * 10 + (b - b'0') as isize;
                    ok = true;
                } else if b != b' ' {
                    ok = false;
                    break;
                }
            }
            if ok && user_lib::kill(pid) == 0 {
                user_lib::print("sh: killed\n");
            } else {
                user_lib::print("sh: kill failed\n");
            }
            user_lib::print("sh$ ");
            continue;
        }
        // background: trailing & outside quotes (v0.7 quote-aware)
        let mut bgmode = false;
        let mut m = n;
        {
            let tt = trim(&buf[..n]);
            if !tt.is_empty() && tt[tt.len() - 1] == b'&' {
                // map back into buf: find trailing & index, check unquoted
                let mut k = n;
                while k > 0 && (buf[k - 1] == b'\n' || buf[k - 1] == b'\r' || buf[k - 1] == b' ') {
                    k -= 1;
                }
                // buf[k-1] == '&'; bg only if the LAST & is outside quotes
                let mut last_amp: Option<usize> = None;
                let mut off = 0;
                while off < k {
                    match find_unquoted(&buf[off..k], b'&') {
                        Some(p) => {
                            last_amp = Some(off + p);
                            off += p + 1;
                        }
                        None => break,
                    }
                }
                if last_amp == Some(k - 1) {
                    bgmode = true;
                    m = k - 1;
                }
            }
        }
        if bgmode {
            exec_bg(&buf, m, &mut jobs, &env);
        } else {
            exec_cmd(&buf, n, &mut jobs, &env);
        }
        user_lib::print("sh$ ");
    }
}

// run line in background (no waiting); record pid. Returns child pid or -1.
fn exec_bg(line: &[u8], n: usize, jobs: &mut [isize; 8], env: &Env) -> isize {
    // reuse exec_cmd machinery via fork here is complex; support simple prog+args
    let core = trim(&line[..n]);
    let mut toks = [[0u8; 64]; 16];
    let mut lit = [[false; 64]; 16];
    let ntok = tokenize_env(core, &mut toks, &mut lit, env);
    if ntok == 0 {
        return -1;
    }
    let mut xtoks = [[0u8; 64]; 16];
    let ntok = expand_globs(&toks, ntok, &lit, &mut xtoks);
    if ntok == 0 {
        return -1;
    }
    let toks = xtoks;
    let mut path = [0u8; 64];
    resolve(&toks[0], &mut path);
    let mut av: [*const u8; 17] = [core::ptr::null(); 17];
    mkargv(&toks, ntok, &mut av);
    let mut kv = [[0u8; 96]; 16];
    let mut ev: [*const u8; 17] = [core::ptr::null(); 17];
    mkenvp(env, &mut kv, &mut ev);
    let pid = user_lib::fork();
    if pid == 0 {
        let _ = user_lib::execve(path.as_ptr(), av.as_ptr() as usize, ev.as_ptr() as usize);
        let _ = user_lib::execve(toks[0].as_ptr(), av.as_ptr() as usize, ev.as_ptr() as usize);
        user_lib::exit(-1);
    } else if pid > 0 {
        if jobs_add(jobs, pid) {
            user_lib::print("[bg] started\n");
        } else {
            user_lib::print("sh: job table full, waiting\n");
            wait_foreground(pid, jobs);
        }
    }
    pid
}

fn drain_jobs(jobs: &mut [isize; 8]) {
    loop {
        let mut any = false;
        for j in jobs.iter() {
            if *j != 0 {
                any = true;
                break;
            }
        }
        if !any {
            break;
        }
        let mut code: i32 = 0;
        let w = user_lib::wait(&mut code as *mut i32);
        if w == -2 {
            user_lib::yield_();
            continue;
        }
        if w < 0 {
            break;
        }
        bg_done(jobs, w);
    }
}
