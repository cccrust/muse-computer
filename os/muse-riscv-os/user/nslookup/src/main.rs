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

// v1.7 nslookup: resolve a name via the given (or default) DNS server.
// Usage: nslookup <name> [server_ip] [server_port]
// Defaults: server 10.0.2.3:53 (SLIRP DNS); the test suite passes
// 10.0.2.2 5353 (tools/dns_stub.py).
// Prints "name -> a.b.c.d" and self-checks [TEST] nslookup PASS/FAIL
// (PASS = got an answer at all; correctness of the VALUE is the stub's
// contract, asserted host-side too).
const DEF_SRV: u32 = 0x0a00_0203; // 10.0.2.3 BE

fn print_ip(ip: u32) {
    let mut b = [0u8; 16];
    let mut n = 0;
    for i in 0..4 {
        let v = ((ip >> (24 - i * 8)) & 0xff) as usize;
        if i > 0 {
            b[n] = b'.';
            n += 1;
        }
        let mut t = [0u8; 3];
        let mut tn = 0;
        let mut x = v;
        if x == 0 {
            t[0] = b'0';
            tn = 1;
        } else {
            let mut rev = [0u8; 3];
            let mut rn = 0;
            while x > 0 {
                rev[rn] = b'0' + (x % 10) as u8;
                x /= 10;
                rn += 1;
            }
            while rn > 0 {
                rn -= 1;
                t[tn] = rev[rn];
                tn += 1;
            }
        }
        b[n..n + tn].copy_from_slice(&t[..tn]);
        n += tn;
    }
    let _ = user_lib::write(1, b.as_ptr(), n);
}

#[no_mangle]
pub extern "C" fn main(argc: usize, argv: *const *const u8) {
    if argc < 2 {
        user_lib::print("usage: nslookup <name> [server_ip] [server_port]\n");
        user_lib::exit(1);
    }
    let name = match unsafe { user_lib::argv_str(argv, 1, argc) } {
        Some(s) if !s.is_empty() => s,
        _ => {
            user_lib::print("nslookup: bad name\n");
            user_lib::exit(1);
        }
    };
    let mut srv = DEF_SRV;
    if argc >= 3 {
        if let Some(s) = unsafe { user_lib::argv_str(argv, 2, argc) } {
            if let Some(ip) = user_lib::parse_ip(s) {
                srv = ip;
            }
        }
    }
    let mut port: u16 = 53;
    if argc >= 4 {
        if let Some(s) = unsafe { user_lib::argv_str(argv, 3, argc) } {
            let mut v: u16 = 0;
            for &c in s.iter() {
                if c < b'0' || c > b'9' {
                    v = 0;
                    break;
                }
                v = v.saturating_mul(10).saturating_add((c - b'0') as u16);
            }
            if v != 0 {
                port = v;
            }
        }
    }
    match user_lib::dns_resolve(name, srv, port) {
        Some(ip) => {
            user_lib::print("resolved -> ");
            print_ip(ip);
            user_lib::print("\n[TEST] nslookup PASS\n");
            user_lib::exit(0);
        }
        None => {
            user_lib::print("[TEST] nslookup FAIL\n");
            user_lib::exit(1);
        }
    }
}
