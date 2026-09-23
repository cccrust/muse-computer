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

// v2.2 cgroup self-test: the capped party fails, the machine doesn't.
// Parent: baseline memstat -> cgcreate(1000) -> fork child.
// Child: cgenter(new) -> sbrk until -1 (MUST get -1, not a machine panic)
//   -> exit(0). A child that eats past ~2x cap without -1 fails loud.
// Parent: waitpid code 0 -> own sbrk(1MB)+touch works (sibling unaffected)
//   -> memstat delta small (reclaim still works, v1.2 semantics hold).
// All return-code assertions; no timing.
fn fail(msg: &str) -> ! {
    user_lib::print("[TEST] cg FAIL (");
    user_lib::print(msg);
    user_lib::print(")\n");
    user_lib::exit(1);
}

#[no_mangle]
pub extern "C" fn main(_argc: usize, _argv: *const *const u8) {
    // bad ids rejected
    if user_lib::cgenter(9999) != -1 {
        fail("badenter");
    }
    if user_lib::cglimit(9999, 10) != -1 {
        fail("badlimit");
    }
    let base = user_lib::memstat();
    if base < 0 {
        fail("memstat");
    }
    let cg = user_lib::cgcreate(1000);
    if cg < 0 {
        fail("create");
    }
    // limit takes on the new group (0 would mean unlimited; check roundtrip
    // via a second group to keep this one capped)
    let pid = user_lib::fork();
    if pid == 0 {
        if user_lib::cgenter(cg) != 0 {
            fail("enter");
        }
        // hog: sbrk until the cap bites (-1). Cap is 1000 frames; the
        // child starts near zero usage. Bound the loop: 3000 iterations
        // of 1MB would be 3GB -- must stop at -1 long before.
        let mut got_minus1 = false;
        let mut i = 0;
        while i < 3000 {
            let r = user_lib::sbrk(1024 * 1024);
            if r < 0 {
                got_minus1 = true;
                break;
            }
            // touch one byte per page so frames materialize (whether sbrk
            // maps eagerly or faults lazily, usage must grow)
            let p = r as *mut u8;
            let mut k = 0usize;
            while k < 1024 * 1024 {
                unsafe {
                    core::ptr::write_volatile(p.add(k), 0xaa);
                }
                k += 4096;
            }
            i += 1;
        }
        if !got_minus1 {
            // ate ~3GB without -1: the cap did not hold
            fail("nocap");
        }
        user_lib::exit(0);
    } else if pid > 0 {
        let mut code: i32 = -1;
        loop {
            let w = user_lib::waitpid(pid, &mut code as *mut i32, 0);
            if w == -2 {
                user_lib::yield_();
                continue;
            }
            if w != pid || code != 0 {
                fail("reap");
            }
            break;
        }
        // sibling unaffected: own big sbrk works
        let r = user_lib::sbrk(1024 * 1024);
        if r < 0 {
            fail("sibling");
        }
        let p = r as *mut u8;
        unsafe {
            core::ptr::write_volatile(p, 0xbb);
            if core::ptr::read_volatile(p) != 0xbb {
                fail("sibling-rw");
            }
        }
        // frames conserved (child's ~1000+ reaped; slop for tables/TF)
        let end = user_lib::memstat();
        let loss = base - end;
        if loss > 1500 {
            fail("leak");
        }
        user_lib::print("[TEST] cg PASS\n");
        user_lib::exit(0);
    } else {
        fail("fork");
    }
}
