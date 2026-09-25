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

// v2.3 CPU cap race: NHOG_FREE uncapped hogs vs NHOG_CAP hogs sharing
// one 5%-capped cgroup, identical work. Every pick system-wide prefers
// uncapped tasks (capped run only via the work-conserving second pass).
// Hogs far outnumber harts (4x oversubscribed) so every hart always has
// queued alternatives -- without this, a capped hog alone on an idle
// hart runs full speed via the pass-2 fallback and the order is a coin
// flip (observed: 2+6 fails as cpu-order). Capped fork first (head start
// to the throttled side: a false PASS on broken quota code is ~impossible).
// The FIRST reaped child must be uncapped. 15s budget; timeout or wrong
// order (or any wrong exit code) is FAIL. Tests ceiling, not floor.
const NHOG_CAP: usize = 12;
const NHOG_FREE: usize = 4;
const HOG_N: u64 = 6_000_000;
const FREE_CODE: i32 = 77;

fn hog(n: u64) {
    let mut k = 0u64;
    loop {
        k = k.wrapping_add(1);
        if k >= n {
            break;
        }
        // yield often: keeps the hogs queued (not just running) so the
        // quota preference actually steers picks instead of pass-2 fallback.
        if k & 0x3ff == 0 {
            user_lib::yield_();
        }
    }
}

fn cpu_phase() {
    if user_lib::cgsetcpu(9999, 5) != -1 {
        fail("badcpu");
    }
    let gcap = user_lib::cgcreate(8000);
    if gcap < 0 {
        fail("cpu-create-cap");
    }
    let gfree = user_lib::cgcreate(8000);
    if gfree < 0 {
        fail("cpu-create-free");
    }
    if user_lib::cgsetcpu(gcap, 5) != 0 {
        fail("cpu-setcap");
    }
    // gfree stays uncapped (default 100%).
    let mut i = 0;
    while i < NHOG_CAP {
        let pid = user_lib::fork();
        if pid == 0 {
            if user_lib::cgenter(gcap) != 0 {
                fail("cpu-enter-cap");
            }
            hog(HOG_N);
            user_lib::exit(10 + i as i32);
        } else if pid < 0 {
            fail("cpu-fork-cap");
        }
        i += 1;
    }
    let mut j = 0;
    while j < NHOG_FREE {
        let pid = user_lib::fork();
        if pid == 0 {
            if user_lib::cgenter(gfree) != 0 {
                fail("cpu-enter-free");
            }
            hog(HOG_N);
            user_lib::exit(FREE_CODE);
        } else if pid < 0 {
            fail("cpu-fork-free");
        }
        j += 1;
    }
    let total = NHOG_CAP + NHOG_FREE;
    let t0 = user_lib::time();
    let mut code: i32 = -1;
    let mut first = true;
    let mut n = 0;
    let mut seen_free = 0;
    let mut seen_cap = 0;
    while n < total {
        if user_lib::time() - t0 > 15000 {
            fail("cpu-timeout");
        }
        let w = user_lib::waitpid(-1, &mut code as *mut i32, 0);
        if w == -2 {
            user_lib::yield_();
            continue;
        }
        if w < 0 {
            fail("cpu-reap");
        }
        if first {
            first = false;
            if code != FREE_CODE {
                fail("cpu-order");
            }
            seen_free += 1;
        } else if code == FREE_CODE {
            seen_free += 1;
        } else if code >= 10 && code < 10 + NHOG_CAP as i32 {
            seen_cap += 1;
        } else {
            fail("cpu-code");
        }
        n += 1;
    }
    if seen_free != NHOG_FREE || seen_cap != NHOG_CAP {
        fail("cpu-count");
    }
}

// v2.7 share race: NHOG_HI hogs in a weight-8 group vs NHOG_LO hogs in
// a weight-1 group, identical work, all uncapped (ceiling sits out --
// pure weight contest). Hogs outnumber harts so every pick has
// alternatives (same oversubscription discipline as cpu_phase).
//
// WHAT IS (AND IS NOT) ASSERTED -- read _doc/v2.7.md §4 first:
// order is deliberately NOT asserted. On this scheduler each hart pins
// its local queue (steals only happen on empty queues), so a lo hog
// with a dedicated hart finishes at full speed no matter the weights --
// first-finish would test queue luck, not shares. What IS asserted:
// (a) API edges (bad id/weight), (b) COMPLETION: every hog finishes,
// with the right codes and counts, inside 15s. Completion catches the
// dangerous direction bug (pick-MAX instead of pick-min starves the
// minimum forever -> timeout) and any hang/crash in the rewritten pick
// paths; it cannot see weights (first-fit passes too -- documented).
// The weights formula itself is pinned by host-tests (exact arithmetic).
//
// Hog discipline DIFFERS from cpu_phase on purpose (no yields here):
// yields would schedule at ~MHz while vruntime charges land at 100Hz,
// so the signal would sit frozen between ticks and the same task would
// win every pick within a 10ms slice (pinning). Pure spin aligns picks
// to timer ticks (fresh counters every pick). See _doc/v2.7.md.
const NHOG_HI: usize = 4;
const NHOG_LO: usize = 4;
const HI_CODE: i32 = 55;
const SPIN_N: u64 = 60_000_000;

fn spinkes(n: u64) {
    let mut k = 0u64;
    while k < n {
        k = k.wrapping_add(1);
        // optimization barrier (DCE would otherwise delete a side-
        // effect-free spin): fences cannot be proven dead, so the loop
        // survives; cadence matches hog()'s yield rhythm, cost ~nil.
        // No yield_: preemption comes from timer ticks only.
        if k & 0x3ff == 0 {
            core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::SeqCst);
        }
    }
}

fn share_phase() {
    if user_lib::cgsetshare(9999, 8) != -1 {
        fail("share-badid");
    }
    let ghi = user_lib::cgcreate(0);
    if ghi < 0 {
        fail("share-create-hi");
    }
    let glo = user_lib::cgcreate(0);
    if glo < 0 {
        fail("share-create-lo");
    }
    // 0 weight rejected, >1000 rejected (API edges, no timing).
    if user_lib::cgsetshare(ghi, 0) != -1 {
        fail("share-badw0");
    }
    if user_lib::cgsetshare(ghi, 1001) != -1 {
        fail("share-badwbig");
    }
    if user_lib::cgsetshare(ghi, 8) != 0 {
        fail("share-sethi");
    }
    if user_lib::cgsetshare(glo, 1) != 0 {
        fail("share-setlo");
    }
    // lo hogs fork first (head start to the light side, mirroring the
    // cpu_phase anti-flake discipline: a false PASS on broken shares
    // must be ~impossible).
    let mut i = 0;
    while i < NHOG_LO {
        let pid = user_lib::fork();
        if pid == 0 {
            if user_lib::cgenter(glo) != 0 {
                fail("share-enter-lo");
            }
            spinkes(SPIN_N);
            user_lib::exit(30 + i as i32);
        } else if pid < 0 {
            fail("share-fork-lo");
        }
        i += 1;
    }
    let mut j = 0;
    while j < NHOG_HI {
        let pid = user_lib::fork();
        if pid == 0 {
            if user_lib::cgenter(ghi) != 0 {
                fail("share-enter-hi");
            }
            spinkes(SPIN_N);
            user_lib::exit(HI_CODE);
        } else if pid < 0 {
            fail("share-fork-hi");
        }
        j += 1;
    }
    let total = NHOG_HI + NHOG_LO;
    let t0 = user_lib::time();
    let mut code: i32 = -1;
    let mut n = 0;
    let mut seen_hi = 0;
    let mut seen_lo = 0;
    while n < total {
        if user_lib::time() - t0 > 15000 {
            fail("share-timeout");
        }
        let w = user_lib::waitpid(-1, &mut code as *mut i32, 0);
        if w == -2 {
            user_lib::yield_();
            continue;
        }
        if w < 0 {
            fail("share-reap");
        }
        // completion only (no order assertion -- see header comment).
        if code == HI_CODE {
            seen_hi += 1;
        } else if code >= 30 && code < 30 + NHOG_LO as i32 {
            seen_lo += 1;
        } else {
            fail("share-code");
        }
        n += 1;
    }
    if seen_hi != NHOG_HI || seen_lo != NHOG_LO {
        fail("share-count");
    }
}

/// v2.9: slot-reuse cycle (see _doc/v2.9.md §1.3/§2). 300x
/// { cgcreate -> fork child cgenter+exit -> waitpid reap } must ALL
/// succeed: without reuse the 256-slot bound fails closed (~id 256,
/// cgcreate returns -1). The child must really ENTER (occupancy --
/// a never-entered slot is never reusable, or create-then-enter
/// patterns would merge) and the parent must really REAP (a zombie
/// still occupies its slot). Prints its own PASS for the suite.
fn reuse_phase() {
    let mut i = 0;
    while i < 300 {
        let g = user_lib::cgcreate(0);
        if g < 0 {
            fail("reuse-create");
        }
        let pid = user_lib::fork();
        if pid == 0 {
            if user_lib::cgenter(g) != 0 {
                fail("reuse-enter");
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
                    fail("reuse-reap");
                }
                break;
            }
        } else {
            fail("reuse-fork");
        }
        i += 1;
    }
    user_lib::print("[TEST] cgreuse PASS\n");
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
        // v2.3: CPU cap ordering (cap-as-ceiling, order assertion only).
        cpu_phase();
        // v2.7: CPU share ordering (vruntime fairness, order assertion
        // only -- ratios are never asserted, see _doc/v2.7.md §4).
        share_phase();
        // v2.9: cgroup slot reuse (300 create/enter/exit cycles).
        reuse_phase();
        user_lib::print("[TEST] cg PASS\n");
        user_lib::exit(0);
    } else {
        fail("fork");
    }
}
