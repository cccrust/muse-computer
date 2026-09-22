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

// v1.6 ping: ICMP echo to the SLIRP gateway (10.0.2.2), 4 packets.
// Each round: send (ARP resolution returns WouldBlock; retry), then wait
// up to ~5s for the matching reply. RTT measured coarsely in 10ms sleeps.
// PASS requires 4/4 replies (gateway echo is SLIRP-internal, reliable).
const GW_IP: u32 = 0x0a00_0202; // 10.0.2.2 BE

#[no_mangle]
pub extern "C" fn main(_argc: usize, _argv: *const *const u8) {
    let fd = user_lib::socket(2);
    if fd < 0 {
        user_lib::print("[TEST] ping FAIL (socket)\n");
        user_lib::exit(1);
    }
    if user_lib::connect(fd, GW_IP, 0) != 0 {
        user_lib::print("[TEST] ping FAIL (connect)\n");
        user_lib::exit(1);
    }
    let mut got = 0;
    let mut i = 0;
    while i < 4 {
        let msg = b"muse-ping-00";
        let mut m = *msg;
        m[10] = b'0' + i as u8;
        // send (ARP may need a few retries)
        let mut sent = false;
        let mut s = 0;
        while s < 500 {
            let r = user_lib::send(fd, m.as_ptr(), m.len());
            if r == m.len() as isize {
                sent = true;
                break;
            }
            if r >= 0 || r != -2 {
                break;
            }
            user_lib::sleep(1);
            s += 1;
        }
        if !sent {
            break;
        }
        // wait reply (match by payload echo)
        let mut buf = [0u8; 32];
        let mut ok_pkt = false;
        let mut w = 0;
        while w < 500 {
            let r = user_lib::recv(fd, buf.as_mut_ptr(), buf.len());
            if r == m.len() as isize && buf[..m.len()] == m[..] {
                ok_pkt = true;
                break;
            }
            if r >= 0 || r != -2 {
                break;
            }
            user_lib::sleep(1);
            w += 1;
        }
        if !ok_pkt {
            break;
        }
        got += 1;
        i += 1;
    }
    user_lib::close(fd);
    if got == 4 {
        user_lib::print("[TEST] ping PASS\n");
    } else {
        user_lib::print("[TEST] ping FAIL\n");
    }
    user_lib::exit(0);
}
