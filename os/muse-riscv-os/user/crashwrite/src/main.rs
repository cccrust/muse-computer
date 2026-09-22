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

// v1.6 crash-consistency workload (with test.sh §11 power-loss stage).
// Sector-granular on purpose: the journal makes every 512B sector write
// atomic (old-or-new), but NOT multi-sector spans (standard ordered-mode
// semantics; cross-block atomicity needs TX batching, v1.7+). So each page
// here is exactly one sector: [ver:u32][idx:u32][fill 504].
// write: loop versions over /CRASHDAT. check: every page is untouched-zero
// or one complete version (torn = FAIL). The property holds for ANY kill
// point, so the test.sh kill timing is not load-bearing.
const NPAGE: usize = 64;
const PATH: &[u8] = b"/CRASHDAT\0";

fn page_pat(ver: u32, idx: u32, out: &mut [u8; 512]) {
    let mut x = ver.wrapping_mul(0x9e37_79b9).wrapping_add(idx.wrapping_mul(0x85eb_ca6b));
    let mut i = 0;
    while i < 512 {
        x = x.wrapping_mul(0x27d4_eb2f).wrapping_add(0x1656_63b5);
        out[i] = (x >> 24) as u8;
        i += 1;
    }
    out[0] = (ver & 0xff) as u8;
    out[1] = ((ver >> 8) & 0xff) as u8;
    out[2] = ((ver >> 16) & 0xff) as u8;
    out[3] = ((ver >> 24) & 0xff) as u8;
    out[4] = (idx & 0xff) as u8;
    out[5] = ((idx >> 8) & 0xff) as u8;
    out[6] = ((idx >> 16) & 0xff) as u8;
    out[7] = ((idx >> 24) & 0xff) as u8;
}

// 0 = untouched-zero, 1 = one complete version, 2 = torn.
fn verify_page(p: &[u8; 512], idx: u32) -> u8 {
    let mut zero = true;
    for &b in p.iter() {
        if b != 0 {
            zero = false;
            break;
        }
    }
    if zero {
        return 0;
    }
    let ver = (p[0] as u32) | ((p[1] as u32) << 8) | ((p[2] as u32) << 16) | ((p[3] as u32) << 24);
    let pi = (p[4] as u32) | ((p[5] as u32) << 8) | ((p[6] as u32) << 16) | ((p[7] as u32) << 24);
    if ver == 0 || pi != idx {
        return 2;
    }
    let mut exp = [0u8; 512];
    page_pat(ver, idx, &mut exp);
    if exp[..] == p[..] {
        1
    } else {
        2
    }
}

#[no_mangle]
pub extern "C" fn main(argc: usize, argv: *const *const u8) -> ! {
    let mode_write = if argc >= 2 {
        match unsafe { user_lib::argv_str(argv, 1, argc) } {
            Some(s) => s.len() == 5 && s[0] == b'w',
            None => false,
        }
    } else {
        false
    };
    if mode_write {
        do_write();
    } else {
        do_check();
    }
}

fn do_write() -> ! {
    // readiness marker (test.sh §11 polls for this, then sleeps for
    // versions to accumulate before SIGKILL).
    user_lib::print("[TEST] crashwrite running\n");
    let mut page = [0u8; 512];
    let mut ver: u32 = 1;
    loop {
        let fd = user_lib::open(PATH.as_ptr(), user_lib::O_CREATE);
        if fd < 0 {
            user_lib::exit(1);
        }
        let mut i = 0u32;
        while i < NPAGE as u32 {
            page_pat(ver, i, &mut page);
            if user_lib::lseek(fd, (i as usize * 512) as isize, 0) < 0 {
                user_lib::close(fd);
                user_lib::exit(1);
            }
            // single 512B write = single journaled sector = atomic
            let mut k = 0;
            while k < 512 {
                let r = user_lib::write(fd, unsafe { page.as_ptr().add(k) }, 512 - k);
                if r <= 0 {
                    user_lib::close(fd);
                    user_lib::exit(1);
                }
                k += r as usize;
            }
            i += 1;
        }
        user_lib::close(fd);
        ver = ver.wrapping_add(1);
        if ver == 0 {
            ver = 1;
        }
    }
}

fn do_check() -> ! {
    let fd = user_lib::open(PATH.as_ptr(), 0);
    if fd < 0 {
        // never written (killed before first version completed): loudly FAIL
        // so stage setup errors don't pass silently.
        user_lib::print("[TEST] crash FAIL (no file)\n");
        user_lib::exit(1);
    }
    let mut page = [0u8; 512];
    let mut i = 0u32;
    let mut ok = true;
    let mut touched = 0;
    while i < NPAGE as u32 {
        if user_lib::lseek(fd, (i as usize * 512) as isize, 0) < 0 {
            ok = false;
            break;
        }
        let mut k = 0;
        while k < 512 {
            let r = user_lib::read(fd, unsafe { page.as_mut_ptr().add(k) }, 512 - k);
            if r <= 0 {
                break;
            }
            k += r as usize;
        }
        if k != 512 {
            ok = false;
            break;
        }
        let v = verify_page(&page, i);
        if v == 2 {
            ok = false;
            break;
        }
        if v == 1 {
            touched += 1;
        }
        i += 1;
    }
    user_lib::close(fd);
    // require real progress (killed after at least one full version) so a
    // too-early kill can't vacuous-pass; test.sh sleeps before killing.
    if ok && touched > 0 {
        user_lib::print("[TEST] crash PASS\n");
    } else {
        user_lib::print("[TEST] crash FAIL\n");
    }
    user_lib::exit(0);
}
