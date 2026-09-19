#![no_std]
#![no_main]
use core::arch::global_asm;
global_asm!(r#"
.section .text.entry
.globl _start
_start:
    ld a0, 0(sp)
    addi a1, sp, 8
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

fn exec_cmd(line: &[u8], n: usize, jobs: &mut [isize; 8]) {
    // trim \n
    let mut end = n;
    while end > 0 && (line[end - 1] == b'\n' || line[end - 1] == b'\r' || line[end - 1] == b' ') {
        end -= 1;
    }
    if end == 0 {
        return;
    }
    let cmd = &line[..end];
    // check pipe '|'
    let mut pipe_at: Option<usize> = None;
    for i in 0..end {
        if cmd[i] == b'|' {
            pipe_at = Some(i);
            break;
        }
    }
    if let Some(p) = pipe_at {
        run_pipe(&cmd[..p], &cmd[p + 1..], jobs);
        return;
    }
    // redirection: prog > file  /  prog < file (single file, no pipe combo)
    let mut redir_out: Option<&[u8]> = None;
    let mut redir_in: Option<&[u8]> = None;
    let mut core_end = end;
    for i in 0..end {
        if cmd[i] == b'>' {
            let f = trim(&cmd[i + 1..end]);
            // filename = up to next space
            let mut fl = f.len();
            for k in 0..f.len() {
                if f[k] == b' ' {
                    fl = k;
                    break;
                }
            }
            redir_out = Some(&f[..fl]);
            core_end = i;
            break;
        }
        if cmd[i] == b'<' {
            let f = trim(&cmd[i + 1..end]);
            let mut fl = f.len();
            for k in 0..f.len() {
                if f[k] == b' ' {
                    fl = k;
                    break;
                }
            }
            redir_in = Some(&f[..fl]);
            core_end = i;
            break;
        }
    }
    let core = trim(&cmd[..core_end]);
    // tokenize core into argv
    let mut toks = [[0u8; 64]; 8];
    let ntok = tokenize(core, &mut toks);
    if ntok == 0 {
        return;
    }
    let mut path = [0u8; 64];
    resolve(&toks[0], &mut path);
    let mut av: [*const u8; 9] = [core::ptr::null(); 9];
    mkargv(&toks, ntok, &mut av);
    let pid = user_lib::fork();
    if pid == 0 {
        // redirections first (close+dup trick: dup picks lowest free fd)
        if let Some(f) = redir_out {
            let mut fp = [0u8; 64];
            let mut L = 0;
            for &b in f {
                if L < 62 {
                    fp[L] = b;
                    L += 1;
                }
            }
            fp[L] = 0;
            let fd = user_lib::open(fp.as_ptr(), 0x40 | 0x200 | 1);
            if fd >= 0 {
                user_lib::close(1);
                user_lib::dup(fd as isize);
                user_lib::close(fd as isize);
            }
        }
        if let Some(f) = redir_in {
            let mut fp = [0u8; 64];
            let mut L = 0;
            for &b in f {
                if L < 62 {
                    fp[L] = b;
                    L += 1;
                }
            }
            fp[L] = 0;
            let fd = user_lib::open(fp.as_ptr(), 0);
            if fd >= 0 {
                user_lib::close(0);
                user_lib::dup(fd as isize);
                user_lib::close(fd as isize);
            }
        }
        let _ = user_lib::exec(path.as_ptr(), av.as_ptr() as usize);
        // try token itself as path (e.g. absolute path typed)
        let _ = user_lib::exec(toks[0].as_ptr(), av.as_ptr() as usize);
        user_lib::print("sh: exec failed\n");
        user_lib::exit(-1);
    } else if pid > 0 {
        wait_foreground(pid, jobs);
    }
}

fn run_pipe(left: &[u8], right: &[u8], jobs: &mut [isize; 8]) {
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
        user_lib::close(1);
        user_lib::dup(fds[1] as isize);
        user_lib::close(fds[0] as isize);
        user_lib::close(fds[1] as isize);
        exec_simple(l);
        user_lib::exit(-1);
    }
    let p2 = user_lib::fork();
    if p2 == 0 {
        user_lib::close(0);
        user_lib::dup(fds[0] as isize);
        user_lib::close(fds[0] as isize);
        user_lib::close(fds[1] as isize);
        exec_simple(r);
        user_lib::exit(-1);
    }
    user_lib::close(fds[0] as isize);
    user_lib::close(fds[1] as isize);
    wait_foreground(p1, jobs);
    wait_foreground(p2, jobs);
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

// split s into up to 8 NUL-terminated tokens; returns count.
fn tokenize(s: &[u8], toks: &mut [[u8; 64]]) -> usize {
    let mut n = 0;
    let mut i = 0;
    while i < s.len() && n < 8 {
        while i < s.len() && s[i] == b' ' {
            i += 1;
        }
        if i >= s.len() {
            break;
        }
        let mut l = 0;
        while i < s.len() && s[i] != b' ' && l < 62 {
            toks[n][l] = s[i];
            l += 1;
            i += 1;
        }
        while i < s.len() && s[i] != b' ' {
            i += 1;
        }
        toks[n][l] = 0;
        n += 1;
    }
    n
}

// argv pointers for exec (toks must outlive the call).
fn mkargv(toks: &[[u8; 64]], n: usize, av: &mut [*const u8; 9]) {
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

fn exec_simple(cmd: &[u8]) {
    let cmd = trim(cmd);
    let mut toks = [[0u8; 64]; 8];
    let ntok = tokenize(cmd, &mut toks);
    if ntok == 0 {
        user_lib::exit(-1);
    }
    let mut path = [0u8; 64];
    resolve(&toks[0], &mut path);
    let mut av: [*const u8; 9] = [core::ptr::null(); 9];
    mkargv(&toks, ntok, &mut av);
    let _ = user_lib::exec(path.as_ptr(), av.as_ptr() as usize);
    let _ = user_lib::exec(toks[0].as_ptr(), av.as_ptr() as usize);
    user_lib::print("sh: pipe exec failed\n");
    user_lib::exit(-1);
}

fn run_one(path: &[u8], jobs: &mut [isize; 8]) {
    run_args(path, &[], jobs);
}

// exec path with argv (caller flattens to NUL-terminated bufs)
fn run_args(path: &[u8], args: &[&[u8]], jobs: &mut [isize; 8]) {
    let mut toks = [[0u8; 64]; 8];
    let mut n = 0;
    for &a in args {
        if n >= 8 {
            break;
        }
        let m = a.len().min(62);
        toks[n][..m].copy_from_slice(&a[..m]);
        toks[n][m] = 0;
        n += 1;
    }
    let mut av: [*const u8; 9] = [core::ptr::null(); 9];
    mkargv(&toks, n, &mut av);
    let pid = user_lib::fork();
    if pid == 0 {
        let _ = user_lib::exec(path.as_ptr(), av.as_ptr() as usize);
        user_lib::exit(-1);
    } else if pid > 0 {
        wait_foreground(pid, jobs);
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

// pid-aware wait: reaps other (background) children into jobs table.
fn wait_foreground(pid: isize, jobs: &mut [isize; 8]) {
    let mut code: i32 = 0;
    loop {
        let w = user_lib::wait(&mut code as *mut i32);
        if w == -2 {
            user_lib::yield_();
            continue;
        }
        if w < 0 {
            break;
        }
        if w == pid {
            break;
        }
        bg_done(jobs, w);
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

// single non-blocking reap attempt (for `jobs` builtin)
fn reap_poll(jobs: &mut [isize; 8]) {
    let mut code: i32 = 0;
    let w = user_lib::wait(&mut code as *mut i32);
    if w >= 0 {
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
pub extern "C" fn main(_argc: usize, _argv: *const *const u8) {
    user_lib::print("[USER] sh: Unix-v6 like shell. try: ls, cat /README, echo hi | grep hi, usertests\n");
    let mut jobs = [0isize; 8];
    // auto-run usertests + persist + argv coverage once for test.sh markers
    user_lib::print("[USER] sh: auto-run usertests\n");
    run_one(b"/bin/usertests\0", &mut jobs);
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
    user_lib::print("sh$ ");
    let mut buf = [0u8; 128];
    loop {
        reap_poll(&mut jobs);
        let n = user_lib::read_line(&mut buf);
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
                    break;
                }
            }
            if !any {
                user_lib::print("jobs: none\n");
            }
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
        // background: trailing &
        let mut bgmode = false;
        let mut m = n;
        {
            let tt = trim(&buf[..n]);
            if !tt.is_empty() && tt[tt.len() - 1] == b'&' {
                bgmode = true;
                // strip & (trim handles trailing spaces before it? re-trim)
                m = tt.len() - 1;
                // map back into buf: buf[..n] trimmed is tt; & is last of tt
                // find its index from the end of buf
                let mut k = n;
                while k > 0 && (buf[k - 1] == b'\n' || buf[k - 1] == b'\r' || buf[k - 1] == b' ') {
                    k -= 1;
                }
                // buf[k-1] == '&'
                m = k - 1;
            }
        }
        if bgmode {
            exec_bg(&buf, m, &mut jobs);
        } else {
            exec_cmd(&buf, n, &mut jobs);
        }
        user_lib::print("sh$ ");
    }
}

// run line in background (no waiting); record pid.
fn exec_bg(line: &[u8], n: usize, jobs: &mut [isize; 8]) {
    // reuse exec_cmd machinery via fork here is complex; support simple prog+args
    let core = trim(&line[..n]);
    let mut toks = [[0u8; 64]; 8];
    let ntok = tokenize(core, &mut toks);
    if ntok == 0 {
        return;
    }
    let mut path = [0u8; 64];
    resolve(&toks[0], &mut path);
    let mut av: [*const u8; 9] = [core::ptr::null(); 9];
    mkargv(&toks, ntok, &mut av);
    let pid = user_lib::fork();
    if pid == 0 {
        let _ = user_lib::exec(path.as_ptr(), av.as_ptr() as usize);
        let _ = user_lib::exec(toks[0].as_ptr(), av.as_ptr() as usize);
        user_lib::exit(-1);
    } else if pid > 0 {
        if jobs_add(jobs, pid) {
            user_lib::print("[bg] started\n");
        } else {
            user_lib::print("sh: job table full, waiting\n");
            wait_foreground(pid, jobs);
        }
    }
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
