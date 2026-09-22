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

// v1.7 wget: TCP GET to a file. Usage: wget <host> <port> <path> <outfile>
// host: literal IPv4 ("10.0.2.2") or DNS name (resolved via 10.0.2.3).
// Blocks with sleeps (WouldBlock protocol); verifies HTTP 200 + exact
// Content-Length bytes received, then [TEST] wget PASS/FAIL.
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
            user_lib::sleep(1);
            spins += 1;
            if spins > 1000 {
                return false;
            }
            continue;
        }
        return false;
    }
    true
}

fn connect_tcp(ip: u32, port: u16) -> isize {
    let fd = user_lib::socket(1);
    if fd < 0 {
        return -1;
    }
    // non-blocking connect: -2 until SYN sent (ARP), then EST wait below
    let mut spins = 0;
    loop {
        let r = user_lib::connect(fd, ip, port);
        if r == 0 {
            break;
        }
        if r != -2 {
            user_lib::close(fd);
            return -1;
        }
        user_lib::sleep(1);
        spins += 1;
        if spins > 500 {
            user_lib::close(fd);
            return -1;
        }
    }
    // wait ESTABLISHED: probe with zero-length... no zero-send; poll via
    // recv readiness is wrong too. Instead: send() returns -2 until EST.
    // (SYN_SENT -> -2 per kernel.) Just proceed; send_all retries.
    fd
}

#[no_mangle]
pub extern "C" fn main(argc: usize, argv: *const *const u8) {
    if argc < 5 {
        user_lib::print("usage: wget <host> <port> <path> <outfile>\n");
        user_lib::exit(1);
    }
    let host = unsafe { user_lib::argv_str(argv, 1, argc).unwrap_or(b"") };
    let port = match unsafe { user_lib::argv_str(argv, 2, argc) } {
        Some(s) => {
            let mut v: u16 = 0;
            for &c in s.iter() {
                if c < b'0' || c > b'9' {
                    v = 0;
                    break;
                }
                v = v.saturating_mul(10).saturating_add((c - b'0') as u16);
            }
            v
        }
        None => 0,
    };
    let path = unsafe { user_lib::argv_str(argv, 3, argc).unwrap_or(b"/") };
    let outpath = unsafe { user_lib::argv_str(argv, 4, argc).unwrap_or(b"/dl") };
    if port == 0 {
        user_lib::print("[TEST] wget FAIL (args)\n");
        user_lib::exit(1);
    }
    // host: literal IP or DNS name
    let ip = match user_lib::parse_ip(host) {
        Some(v) => v,
        None => match user_lib::dns_resolve(host, 0x0a00_0203, 53) {
            Some(v) => v,
            None => {
                user_lib::print("[TEST] wget FAIL (dns)\n");
                user_lib::exit(1);
            }
        },
    };
    let fd = connect_tcp(ip, port);
    if fd < 0 {
        user_lib::print("[TEST] wget FAIL (connect)\n");
        user_lib::exit(1);
    }
    let mut req = [0u8; 256];
    let rn = user_lib::http_req(&mut req, host, path);
    if rn == 0 || !send_all(fd, &req[..rn]) {
        user_lib::print("[TEST] wget FAIL (send)\n");
        user_lib::exit(1);
    }
    // receive until EOF (server closes) or cap; find header first
    let mut resp = [0u8; 4096];
    let mut n = 0usize;
    let mut spins = 0;
    let mut eof = false;
    while n < resp.len() {
        let r = user_lib::recv(fd, unsafe { resp.as_mut_ptr().add(n) }, resp.len() - n);
        if r > 0 {
            n += r as usize;
            spins = 0;
            continue;
        }
        if r == 0 {
            eof = true;
            break;
        }
        if r == -2 {
            user_lib::sleep(1);
            spins += 1;
            if spins > 1000 {
                break;
            }
            continue;
        }
        break;
    }
    user_lib::close(fd);
    let (code, off) = user_lib::http_split(&resp[..n]);
    if code != 200 || !eof {
        user_lib::print("[TEST] wget FAIL (status)\n");
        user_lib::exit(1);
    }
    // verify Content-Length matches received body bytes
    let want = content_length(&resp[..off]);
    if want == 0 || n - off != want {
        user_lib::print("[TEST] wget FAIL (length)\n");
        user_lib::exit(1);
    }
    // save to file
    let mut pb = [0u8; 128];
    let k = outpath.len().min(127);
    pb[..k].copy_from_slice(&outpath[..k]);
    pb[k] = 0;
    let f = user_lib::open(pb.as_ptr(), user_lib::O_CREATE | user_lib::O_TRUNC);
    if f < 0 {
        user_lib::print("[TEST] wget FAIL (open)\n");
        user_lib::exit(1);
    }
    let body = &resp[off..n];
    let mut w = 0;
    while w < body.len() {
        let r = user_lib::write(f, unsafe { body.as_ptr().add(w) }, body.len() - w);
        if r <= 0 {
            user_lib::close(f);
            user_lib::print("[TEST] wget FAIL (write)\n");
            user_lib::exit(1);
        }
        w += r as usize;
    }
    user_lib::close(f);
    user_lib::print("[TEST] wget PASS\n");
    user_lib::exit(0);
}

fn content_length(head: &[u8]) -> usize {
    // scan header lines for "Content-Length:" (case exact, our server's form)
    let needle = b"Content-Length:";
    let mut i = 0;
    while i + needle.len() <= head.len() {
        if &head[i..i + needle.len()] == needle {
            let mut j = i + needle.len();
            while j < head.len() && (head[j] == b' ' || head[j] == b'\t') {
                j += 1;
            }
            let mut v = 0usize;
            while j < head.len() && head[j] >= b'0' && head[j] <= b'9' {
                v = v * 10 + (head[j] - b'0') as usize;
                j += 1;
            }
            return v;
        }
        i += 1;
    }
    0
}
