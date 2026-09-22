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

// v1.7 curl: like wget but dumps headers + body to stdout (-i semantics)
// instead of saving. Usage: curl <host> <port> <path>
// Self-checks 200 + marker bytes in body, prints [TEST] curl PASS/FAIL.
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

fn write_all(fd: isize, buf: &[u8]) -> bool {
    let mut off = 0;
    while off < buf.len() {
        let r = user_lib::write(fd, unsafe { buf.as_ptr().add(off) }, buf.len() - off);
        if r <= 0 {
            return false;
        }
        off += r as usize;
    }
    true
}

#[no_mangle]
pub extern "C" fn main(argc: usize, argv: *const *const u8) {
    if argc < 4 {
        user_lib::print("usage: curl <host> <port> <path>\n");
        user_lib::exit(1);
    }
    let host = unsafe { user_lib::argv_str(argv, 1, argc).unwrap_or(b"") };
    let mut port: u16 = 0;
    if let Some(s) = unsafe { user_lib::argv_str(argv, 2, argc) } {
        for &c in s.iter() {
            if c < b'0' || c > b'9' {
                port = 0;
                break;
            }
            port = port.saturating_mul(10).saturating_add((c - b'0') as u16);
        }
    }
    let path = unsafe { user_lib::argv_str(argv, 3, argc).unwrap_or(b"/") };
    if port == 0 {
        user_lib::print("[TEST] curl FAIL (args)\n");
        user_lib::exit(1);
    }
    let ip = match user_lib::parse_ip(host) {
        Some(v) => v,
        None => match user_lib::dns_resolve(host, 0x0a00_0203, 53) {
            Some(v) => v,
            None => {
                user_lib::print("[TEST] curl FAIL (dns)\n");
                user_lib::exit(1);
            }
        },
    };
    let fd = user_lib::socket(1);
    if fd < 0 {
        user_lib::print("[TEST] curl FAIL (socket)\n");
        user_lib::exit(1);
    }
    let mut spins = 0;
    loop {
        let r = user_lib::connect(fd, ip, port);
        if r == 0 {
            break;
        }
        if r != -2 {
            user_lib::print("[TEST] curl FAIL (connect)\n");
            user_lib::exit(1);
        }
        user_lib::sleep(1);
        spins += 1;
        if spins > 500 {
            user_lib::print("[TEST] curl FAIL (connect-timeout)\n");
            user_lib::exit(1);
        }
    }
    let mut req = [0u8; 256];
    let rn = user_lib::http_req(&mut req, host, path);
    if rn == 0 || !send_all(fd, &req[..rn]) {
        user_lib::print("[TEST] curl FAIL (send)\n");
        user_lib::exit(1);
    }
    let mut resp = [0u8; 4096];
    let mut n = 0usize;
    spins = 0;
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
        user_lib::print("[TEST] curl FAIL (status)\n");
        user_lib::exit(1);
    }
    // dump headers + body to stdout, then self-check the marker
    if !write_all(1, &resp[..n]) {
        user_lib::print("[TEST] curl FAIL (stdout)\n");
        user_lib::exit(1);
    }
    let body = &resp[off..n];
    let mut found = false;
    let mark = b"muse-riscv-os";
    if body.len() >= mark.len() {
        let mut i = 0;
        while i + mark.len() <= body.len() {
            if &body[i..i + mark.len()] == mark {
                found = true;
                break;
            }
            i += 1;
        }
    }
    if found {
        user_lib::print("[TEST] curl PASS\n");
    } else {
        user_lib::print("[TEST] curl FAIL (marker)\n");
    }
    user_lib::exit(0);
}
