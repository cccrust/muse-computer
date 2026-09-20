use core::arch::asm;

#[inline(always)]
fn ecall(eid: usize, fid: usize, a0: usize, a1: usize, a2: usize) -> usize {
    let ret: usize;
    unsafe {
        asm!(
            "ecall",
            inlateout("a0") a0 => ret,
            in("a1") a1,
            in("a2") a2,
            in("a6") fid,
            in("a7") eid,
        );
    }
    ret
}

pub fn console_putchar(c: u8) {
    // SBI v0.1 legacy: eid=1 putchar
    ecall(1, 0, c as usize, 0, 0);
}

pub fn set_timer(stime: u64) {
    // SBI legacy set_timer: eid=0, a0=stime (per-hart: programs the
    // calling hart's timer; v1.0 calls it on every hart)
    ecall(0, 0, stime as usize, 0, 0);
}

pub fn shutdown() -> ! {
    // eid=8 shutdown (any hart may call it)
    ecall(8, 0, 0, 0, 0);
    loop {}
}

/// v1.0: SBI HSM hart_start (EID 0x48534D "HSM", fid 0). Starts the given
/// hart in S-mode at start_addr. Returns the SBI error code (0 = success).
/// opaque is currently unused (APs read mhartid directly).
pub fn hart_start(hartid: usize, start_addr: usize, opaque: usize) -> isize {
    ecall(0x48534D, 0, hartid, start_addr, opaque) as isize
}
