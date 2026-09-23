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

// v2.0 chroot self-test (no stdin timing: all return-code assertions).
// Builds /ctrtest + links sh+echo into /ctrtest/bin, jails itself, then:
// - getcwd()=="/" (container view, no prefix leak)
// - open("/bin/sh") works (inside)
// - open("/TESTDATA") fails (outside; only exists at real /)
// - open("/../bin/sh") fails (.. escape clamped... note: /../x normalizes
//   INSIDE the jail to /x, which also fails -- either way unreachable;
//   the test asserts failure, and separately that getcwd has no prefix)
// - chroot("/nope") returns -1, chroot("/bin/sh") (a file) returns -1
// - child forked after jailing stays jailed (open outside fails there too)
fn fail(msg: &str) -> ! {
    user_lib::print("[TEST] chroot FAIL (");
    user_lib::print(msg);
    user_lib::print(")\n");
    user_lib::exit(1);
}

fn open_ro(path: &[u8]) -> isize {
    // path must already be NUL-terminated by caller
    user_lib::open(path.as_ptr(), 0)
}

#[no_mangle]
pub extern "C" fn main(_argc: usize, _argv: *const *const u8) {
    user_lib::mkdir(b"/ctrtest\0".as_ptr());
    user_lib::mkdir(b"/ctrtest/bin\0".as_ptr());
    if user_lib::link(b"/bin/sh\0".as_ptr(), b"/ctrtest/bin/sh\0".as_ptr()) != 0 {
        // may exist from a previous run; verify instead of failing
        if open_ro(b"/ctrtest/bin/sh\0") < 0 {
            fail("setup-link");
        }
    }
    if user_lib::link(b"/bin/echo\0".as_ptr(), b"/ctrtest/bin/echo\0".as_ptr()) != 0 {
        if open_ro(b"/ctrtest/bin/echo\0") < 0 {
            fail("setup-link2");
        }
    }
    // bad targets rejected, jail unchanged (still /)
    if user_lib::chroot(b"/nope\0".as_ptr()) != -1 {
        fail("badpath");
    }
    if user_lib::chroot(b"/bin/sh\0".as_ptr()) != -1 {
        fail("notdir");
    }
    if user_lib::chroot(b"/ctrtest\0".as_ptr()) != 0 {
        fail("chroot");
    }
    // container view of cwd (getcwd returns bytes incl. NUL)
    let mut cb = [0u8; 128];
    let n = user_lib::getcwd(cb.as_mut_ptr(), 128);
    if n != 2 || cb[0] != b'/' || cb[1] != 0 {
        fail("getcwd");
    }
    // inside works
    let f = open_ro(b"/bin/echo\0");
    if f < 0 {
        fail("inside");
    }
    user_lib::close(f);
    // outside unreachable (only exists at the real root)
    if open_ro(b"/TESTDATA\0") >= 0 {
        fail("outside-visible");
    }
    // ".." escape clamped: /../bin/echo normalizes inside the jail and
    // must NOT resolve to the real /bin/echo... note it normalizes to
    // /bin/echo which EXISTS inside -> open succeeds; assert instead that
    // getcwd-equivalent confinement holds via a definitely-outside file:
    if open_ro(b"/../../../TESTDATA\0") >= 0 {
        fail("dotdot-escape");
    }
    // relative escape attempt from a subdir
    if user_lib::chdir(b"/bin\0".as_ptr()) != 0 {
        fail("chdir-bin");
    }
    if open_ro(b"../../../../TESTDATA\0") >= 0 {
        fail("rel-escape");
    }
    // forked child inherits the jail
    let pid = user_lib::fork();
    if pid == 0 {
        if open_ro(b"/TESTDATA\0") >= 0 {
            user_lib::exit(11);
        }
        let mut c2 = [0u8; 128];
        let m = user_lib::getcwd(c2.as_mut_ptr(), 128);
        // child cwd is /bin (inherited, 4 bytes + NUL); must be exactly
        // the container view with no real prefix leaked
        if m != 5 || c2[0] != b'/' || c2[1] != b'b' || c2[2] != b'i' || c2[3] != b'n' || c2[4] != 0 {
            user_lib::exit(12);
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
                fail("child-jailed");
            }
            break;
        }
    } else {
        fail("fork");
    }
    user_lib::print("[TEST] chroot PASS\n");
    user_lib::exit(0);
}
