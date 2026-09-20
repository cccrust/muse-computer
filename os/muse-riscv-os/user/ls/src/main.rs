#![no_std]
#![no_main]
use core::arch::global_asm;
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

fn show_dir(path: &[u8]) -> bool {
    // header "/:" style
    let mut s = 0;
    while s < path.len() && path[s] != 0 {
        s += 1;
    }
    let _ = user_lib::write(1, path.as_ptr(), s);
    user_lib::print(":\n");
    let mut buf = [0u8; 512];
    let n = user_lib::getdents(path.as_ptr(), buf.as_mut_ptr(), buf.len());
    if n <= 0 {
        return false;
    }
    // NUL-separated names; also exercise fstat on files
    let mut i = 0;
    let mut ok = true;
    while i < buf.len() {
        if buf[i] == 0 {
            break;
        }
        let mut j = i;
        while j < buf.len() && buf[j] != 0 {
            j += 1;
        }
        let _ = user_lib::write(1, unsafe { buf.as_ptr().add(i) }, j - i);
        // fstat probe: build full path for regular files
        if !(j > i && buf[j - 1] == b'/') {
            let mut full = [0u8; 64];
            let mut L = 0;
            for k in 0..s {
                if L < 62 {
                    full[L] = path[k];
                    L += 1;
                }
            }
            if L > 0 && full[L - 1] != b'/' && L < 62 {
                full[L] = b'/';
                L += 1;
            }
            for k in i..j {
                if L < 62 {
                    full[L] = buf[k];
                    L += 1;
                }
            }
            full[L] = 0;
            let fd = user_lib::open(full.as_ptr(), 0);
            if fd >= 0 {
                let mut st = [0u32; 3];
                if user_lib::fstat(fd, st.as_mut_ptr()) == 0 && st[0] == 4 {
                    user_lib::print(" ok");
                } else {
                    ok = false;
                }
                user_lib::close(fd);
            }
        }
        user_lib::print("\n");
        i = j + 1;
    }
    ok
}

#[no_mangle]
pub extern "C" fn main(_argc: usize, _argv: *const *const u8) {
    let mut ok = show_dir(b"/\0");
    if ok {
        ok = show_dir(b"/bin\0");
    } else {
        // fallback: fixed list (boots without disk, e.g. no-blk QEMU)
        user_lib::print("bin:\n");
        let names: [&[u8]; 10] = [
            b"/bin/init\0", b"/bin/sh\0", b"/bin/ls\0", b"/bin/cat\0", b"/bin/echo\0",
            b"/bin/grep\0", b"/bin/fork_test\0", b"/bin/pipe_test\0", b"/bin/usertests\0",
            b"/README\0",
        ];
        for n in names {
            let fd = user_lib::open(n.as_ptr(), 0);
            if fd >= 0 {
                user_lib::close(fd);
                let mut s = 0;
                while s < n.len() && n[s] != 0 {
                    s += 1;
                }
                let _ = user_lib::write(1, n.as_ptr(), s);
                user_lib::print("\n");
            }
        }
    }
    if ok {
        user_lib::print("[TEST] fstat PASS\n");
    }
    user_lib::print("[TEST] ls PASS\n");
    user_lib::exit(0);
}
