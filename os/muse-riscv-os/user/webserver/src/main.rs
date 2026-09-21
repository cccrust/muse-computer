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

// v1.5 static web server demo (TCP): listen on 80, serve GET /path from
// the fs (streamed 4K chunks, 64K cap), then close. First valid GET prints
// [TEST] web PASS (test.sh marker); the server keeps serving afterwards
// (run in background via sh autorun / `webserver &`).
fn send_all(fd: isize, buf: &[u8]) -> bool {
    let mut off = 0;
    let mut spins = 0;
    while off < buf.len() {
        let r = user_lib::send(fd, unsafe { buf.as_ptr().add(off) }, buf.len() - off);
        if r > 0 {
            off += r as usize;
            spins = 0;
            continue;
        }
        if r == -2 {
            // backpressure / not ready: yield, bounded spins then fail
            user_lib::sleep(1);
            spins += 1;
            if spins > 500 {
                return false;
            }
            continue;
        }
        return false;
    }
    true
}

// recv until "\r\n\r\n", EOF, error, or cap. Returns bytes read.
fn recv_head(fd: isize, buf: &mut [u8]) -> usize {
    let mut n = 0;
    let mut spins = 0;
    while n < buf.len() {
        let r = user_lib::recv(fd, unsafe { buf.as_mut_ptr().add(n) }, buf.len() - n);
        if r > 0 {
            n += r as usize;
            spins = 0;
            if ends_header(&buf[..n]) {
                break;
            }
            continue;
        }
        if r == 0 {
            break; // EOF
        }
        if r == -2 {
            user_lib::sleep(1);
            spins += 1;
            if spins > 500 {
                break;
            }
            continue;
        }
        break; // hard error
    }
    n
}

fn ends_header(b: &[u8]) -> bool {
    if b.len() < 4 {
        return false;
    }
    let mut i = 0;
    while i + 4 <= b.len() {
        if b[i] == b'\r' && b[i + 1] == b'\n' && b[i + 2] == b'\r' && b[i + 3] == b'\n' {
            return true;
        }
        i += 1;
    }
    false
}

// parse "GET /path HTTP/..." -> path slice (no leading validation yet).
// Returns None if not a GET line.
fn get_path(req: &[u8]) -> Option<&[u8]> {
    if req.len() < 6 || &req[..4] != b"GET " {
        return None;
    }
    let mut e = 4;
    while e < req.len() && req[e] != b' ' && req[e] != b'\r' && req[e] != b'\n' {
        e += 1;
    }
    if e == 4 || e > 120 {
        return None;
    }
    Some(&req[4..e])
}

fn valid_path(p: &[u8]) -> bool {
    if p.is_empty() || p[0] != b'/' {
        return false;
    }
    // reject ".." (no parent escape from fs root)
    let mut i = 0;
    while i + 1 < p.len() {
        if p[i] == b'.' && p[i + 1] == b'.' {
            return false;
        }
        i += 1;
    }
    true
}

fn send_status(fd: isize, code: &[u8]) -> bool {
    // minimal headers; Connection: close (we always close after reply)
    if !send_all(fd, b"HTTP/1.0 ") {
        return false;
    }
    if !send_all(fd, code) {
        return false;
    }
    send_all(fd, b"\r\nContent-Type: text/plain\r\nConnection: close\r\n")
}

fn utoa(mut v: usize, out: &mut [u8]) -> usize {
    if v == 0 {
        out[0] = b'0';
        return 1;
    }
    let mut tmp = [0u8; 20];
    let mut n = 0;
    while v > 0 && n < 20 {
        tmp[n] = b'0' + (v % 10) as u8;
        v /= 10;
        n += 1;
    }
    let mut i = 0;
    while i < n {
        out[i] = tmp[n - 1 - i];
        i += 1;
    }
    n
}

fn serve_one(fd: isize, passed: &mut bool) {
    let mut head = [0u8; 2048];
    let n = recv_head(fd, &mut head);
    let path = if n > 0 { get_path(&head[..n]) } else { None };
    let (ok_path, fpath): (bool, &[u8]) = match path {
        Some(p) if valid_path(p) => {
            // "/" serves /README (friendly root)
            if p.len() == 1 {
                (true, b"/README")
            } else {
                (true, p)
            }
        }
        _ => (false, b"/"),
    };
    if !ok_path {
        send_status(fd, b"400 Bad Request");
        send_all(fd, b"Content-Length: 0\r\n\r\n");
        return;
    }
    // NUL-terminate into a small buf for open()
    let mut pb = [0u8; 128];
    let k = fpath.len().min(127);
    pb[..k].copy_from_slice(&fpath[..k]);
    pb[k] = 0;
    let f = user_lib::open(pb.as_ptr(), 0);
    if f < 0 {
        send_status(fd, b"404 Not Found");
        send_all(fd, b"Content-Length: 0\r\n\r\n");
        return;
    }
    // stream the file in 4K chunks (64K cap), counting first
    let mut total = 0usize;
    let mut chunk = [0u8; 4096];
    let mut chunks: usize = 0;
    loop {
        let r = user_lib::read(f, chunk.as_mut_ptr(), chunk.len());
        if r <= 0 {
            break;
        }
        total += r as usize;
        chunks += 1;
        if chunks * 4096 > 65536 {
            break;
        }
    }
    // NOTE: read() has no rewind; re-open to stream the body. (Two-pass
    // keeps Content-Length exact without a 64K buffer on the 32K stack.)
    user_lib::close(f);
    send_status(fd, b"200 OK");
    let mut nb = [0u8; 20];
    let nl = utoa(total.min(65536), &mut nb);
    send_all(fd, b"Content-Length: ");
    send_all(fd, &nb[..nl]);
    send_all(fd, b"\r\n\r\n");
    let f2 = user_lib::open(pb.as_ptr(), 0);
    if f2 >= 0 {
        let mut left = total.min(65536);
        while left > 0 {
            let want = left.min(chunk.len());
            let r = user_lib::read(f2, chunk.as_mut_ptr(), want);
            if r <= 0 {
                break;
            }
            if !send_all(fd, &chunk[..r as usize]) {
                break;
            }
            left -= r as usize;
        }
        user_lib::close(f2);
    }
    if !*passed {
        *passed = true;
        user_lib::print("[TEST] web PASS\n");
    }
}

#[no_mangle]
pub extern "C" fn main(_argc: usize, _argv: *const *const u8) {
    let fd = user_lib::socket(1);
    if fd < 0 {
        user_lib::print("webserver: socket failed\n");
        user_lib::exit(1);
    }
    if user_lib::bind(fd, 80) != 0 {
        user_lib::print("webserver: bind failed\n");
        user_lib::exit(1);
    }
    if user_lib::listen(fd) != 0 {
        user_lib::print("webserver: listen failed\n");
        user_lib::exit(1);
    }
    // readiness marker (test.sh waits for this before fetching)
    user_lib::print("[NET] webserver listening on :80\n");
    let mut passed = false;
    loop {
        let c = user_lib::accept(fd);
        if c == -2 {
            user_lib::sleep(2); // 20ms
            continue;
        }
        if c < 0 {
            user_lib::sleep(2);
            continue;
        }
        serve_one(c, &mut passed);
        user_lib::close(c);
    }
}
