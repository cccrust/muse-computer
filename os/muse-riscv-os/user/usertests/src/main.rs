#![no_std]
#![no_main]
use core::arch::global_asm;
global_asm!(r#".section .text.entry
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

fn run(path: &[u8]) {
    let pid = user_lib::fork();
    if pid == 0 {
        let _ = user_lib::exec(path.as_ptr(), 0);
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

#[no_mangle]
pub extern "C" fn main(_argc: usize, _argv: *const *const u8) {
    user_lib::print("[USER] usertests: start\n");
    run(b"/bin/fork_test\0");
    run(b"/bin/pipe_test\0");
    run(b"/bin/ls\0");
    run(b"/bin/cat\0");
    run(b"/bin/echo\0");
    mmap_test();
    user_lib::print("[TEST] usertests PASS\n");
    user_lib::exit(0);
}

// v0.8: anonymous mmap write/read, munmap, remap-is-zero, sbrk shrink.
fn mmap_test() {
    let a = user_lib::mmap(0, 8192, 3);
    if a < 0 {
        user_lib::print("[TEST] mmap FAIL (map)\n");
        return;
    }
    let p = a as *mut u8;
    unsafe {
        for i in 0..8192 {
            *p.add(i) = (i ^ 0x5a) as u8;
        }
        let mut ok = true;
        for i in 0..8192 {
            if *p.add(i) != (i ^ 0x5a) as u8 {
                ok = false;
                break;
            }
        }
        if !ok {
            user_lib::print("[TEST] mmap FAIL (rw)\n");
            return;
        }
    }
    if user_lib::munmap(a as usize, 8192) != 0 {
        user_lib::print("[TEST] mmap FAIL (unmap)\n");
        return;
    }
    // remap: recycled frames are zeroed by the frame allocator
    let b = user_lib::mmap(0, 8192, 3);
    if b < 0 {
        user_lib::print("[TEST] mmap FAIL (remap)\n");
        return;
    }
    let q = b as *const u8;
    unsafe {
        let mut ok = true;
        for i in 0..8192 {
            if *q.add(i) != 0 {
                ok = false;
                break;
            }
        }
        if !ok {
            user_lib::print("[TEST] mmap FAIL (zero)\n");
            return;
        }
        // sbrk grow + shrink round-trip
        let old = user_lib::sbrk(0);
        if old < 0 {
            user_lib::print("[TEST] mmap FAIL (brk0)\n");
            return;
        }
        let r = user_lib::sbrk(4096);
        if r != old {
            user_lib::print("[TEST] mmap FAIL (grow)\n");
            return;
        }
        *(old as *mut u8) = 0xab;
        if *(old as *mut u8) != 0xab {
            user_lib::print("[TEST] mmap FAIL (brk-rw)\n");
            return;
        }
        if user_lib::sbrk(-4096) != old + 4096 {
            user_lib::print("[TEST] mmap FAIL (shrink)\n");
            return;
        }
    }
    user_lib::print("[TEST] mmap PASS\n");
}
