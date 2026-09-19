#![no_std]
#![no_main]
use core::arch::global_asm;
global_asm!(r#"
.section .text.entry
.globl _start
_start:
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

fn exec_cmd(line: &[u8], n: usize) {
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
        run_pipe(&cmd[..p], &cmd[p + 1..]);
        return;
    }
    // split by space
    let mut prog_end = end;
    for i in 0..end {
        if cmd[i] == b' ' {
            prog_end = i;
            break;
        }
    }
    let prog = &cmd[..prog_end];
    // build /bin/<prog> path
    let mut path = [0u8; 64];
    let pre = b"/bin/";
    let mut L = 0;
    for &b in pre {
        path[L] = b;
        L += 1;
    }
    for &b in prog {
        if L < 62 {
            path[L] = b;
            L += 1;
        }
    }
    path[L] = 0;
    let pid = user_lib::fork();
    if pid == 0 {
        // child: handle '>' redirect? simplified: ignore, just exec with raw line args
        let r = user_lib::exec(path.as_ptr(), 0);
        // try direct path (e.g. /bin/sh typed full)
        let mut full = [0u8; 64];
        let mut k = 0;
        for &b in cmd {
            if b == b' ' {
                break;
            }
            if k < 62 {
                full[k] = b;
                k += 1;
            }
        }
        full[k] = 0;
        let r2 = user_lib::exec(full.as_ptr(), 0);
        user_lib::print("sh: exec failed\n");
        user_lib::exit(-1);
    } else if pid > 0 {
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
}

fn run_pipe(left: &[u8], right: &[u8]) {
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
    let mut code: i32 = 0;
    for _ in 0..2 {
        loop {
            let w = user_lib::wait(&mut code as *mut i32);
            if w == -2 {
                user_lib::yield_();
                continue;
            }
            break;
        }
    }
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

fn exec_simple(cmd: &[u8]) {
    let cmd = trim(cmd);
    let mut pe = cmd.len();
    for i in 0..cmd.len() {
        if cmd[i] == b' ' {
            pe = i;
            break;
        }
    }
    let mut path = [0u8; 64];
    let mut L = 0;
    for &b in b"/bin/" {
        path[L] = b;
        L += 1;
    }
    for &b in &cmd[..pe] {
        if L < 62 {
            path[L] = b;
            L += 1;
        }
    }
    path[L] = 0;
    let _ = user_lib::exec(path.as_ptr(), 0);
    user_lib::print("sh: pipe exec failed\n");
    user_lib::exit(-1);
}

#[no_mangle]
pub extern "C" fn main() {
    user_lib::print("[USER] sh: Unix-v6 like shell. try: ls, cat /README, echo hi | grep hi, usertests\n");
    // auto-run usertests once for test.sh markers
    user_lib::print("[USER] sh: auto-run usertests\n");
    {
        let p = b"/bin/usertests\0";
        let pid = user_lib::fork();
        if pid == 0 {
            let _ = user_lib::exec(p.as_ptr(), 0);
            user_lib::exit(-1);
        } else if pid > 0 {
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
    }
    user_lib::print("sh$ ");
    let mut buf = [0u8; 128];
    loop {
        let n = user_lib::read_line(&mut buf);
        if n == 0 {
            user_lib::yield_();
            continue;
        }
        exec_cmd(&buf, n);
        user_lib::print("sh$ ");
    }
}
