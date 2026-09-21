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

// v1.3 UDP smoke test: socket + connect to the SLIRP host
// (10.0.2.2:7777, served by tools/udp_echo.py), send an 8-byte magic,
// wait up to ~5s for the identical echo. First packet also covers ARP
// (kernel resolves the gateway on demand).
const GW_IP: u32 = 0x0a00_0202; // 10.0.2.2 BE
const ECHO_PORT: u16 = 7777;

#[no_mangle]
pub extern "C" fn main(_argc: usize, _argv: *const *const u8) {
    let fd = user_lib::socket();
    if fd < 0 {
        user_lib::print("[TEST] net FAIL (socket)\n");
        user_lib::exit(1);
    }
    if user_lib::connect(fd, GW_IP, ECHO_PORT) != 0 {
        user_lib::print("[TEST] net FAIL (connect)\n");
        user_lib::exit(1);
    }
    let magic = b"muse-net";
    // send until accepted (ARP resolution returns WouldBlock)
    let mut sent = false;
    let mut i = 0;
    while i < 500 {
        let r = user_lib::send(fd, magic.as_ptr(), magic.len());
        if r == magic.len() as isize {
            sent = true;
            break;
        }
        if r >= 0 {
            break; // short send: shouldn't happen, fail fast below
        }
        if r != -2 {
            break; // hard error
        }
        user_lib::sleep(1); // 10ms
        i += 1;
    }
    let mut ok = false;
    if sent {
        // wait up to ~5s for the echo
        let mut buf = [0u8; 16];
        let mut j = 0;
        while j < 500 {
            let r = user_lib::recv(fd, buf.as_mut_ptr(), buf.len());
            if r == magic.len() as isize && buf[..8] == *magic {
                ok = true;
                break;
            }
            if r >= 0 {
                break; // wrong packet: fail
            }
            if r != -2 {
                break; // hard error
            }
            user_lib::sleep(1);
            j += 1;
        }
    }
    user_lib::close(fd);
    if ok {
        user_lib::print("[TEST] net PASS\n");
    } else {
        user_lib::print("[TEST] net FAIL\n");
    }
    user_lib::exit(0);
}
