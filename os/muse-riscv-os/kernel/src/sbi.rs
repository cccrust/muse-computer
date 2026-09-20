use core::arch::asm;

#[inline(always)]
fn ecall(eid: usize, fid: usize, a0: usize, a1: usize, a2: usize, a3: usize) -> usize {
    let ret: usize;
    unsafe {
        asm!(
            "ecall",
            inlateout("a0") a0 => ret,
            in("a1") a1,
            in("a2") a2,
            in("a3") a3,
            in("a6") fid,
            in("a7") eid,
        );
    }
    ret
}

pub fn console_putchar(c: u8) {
    // SBI v0.1 legacy: eid=1 putchar
    ecall(1, 0, c as usize, 0, 0, 0);
}

pub fn set_timer(stime: u64) {
    // SBI legacy set_timer: eid=0, a0=stime (per-hart: programs the
    // calling hart's timer; v1.0 calls it on every hart)
    ecall(0, 0, stime as usize, 0, 0, 0);
}

pub fn shutdown() -> ! {
    // eid=8 shutdown (any hart may call it)
    ecall(8, 0, 0, 0, 0, 0);
    loop {}
}

/// v1.0: SBI HSM hart_start (EID 0x48534D "HSM", fid 0). Starts the given
/// hart in S-mode at start_addr. Returns the SBI error code (0 = success).
/// opaque is currently unused (APs read mhartid directly).
pub fn hart_start(hartid: usize, start_addr: usize, opaque: usize) -> isize {
    ecall(0x48534D, 0, hartid, start_addr, opaque, 0) as isize
}

/// v1.1: SBI IPI (EID 0x735049 "sPI", fid 0: sbi_send_ipi). Pends a
/// supervisor software interrupt on every hart in hart_mask. The target
/// takes it once SIE=1 (userspace or the idle wfi loop); a hart spinning
/// in kernel (SIE=0) holds it pending. Returns the SBI error code.
pub fn send_ipi(hart_mask: usize) -> isize {
    ecall(0x735049, 0, hart_mask, 0, 0, 0) as isize
}

/// v1.1: SBI RFENCE remote_sfence_vma (EID 0x52464E43 "RFNC", fid 1:
/// sbi_remote_sfence_vma(hart_mask, hart_mask_base, start, size)).
/// Flushes the TLB range [start, start+size) on every hart in hart_mask
/// (the caller is responsible for its own local sfence). start=size=0
/// means the whole address space (SBI idiom). Fire-and-forget.
pub fn remote_sfence_vma(hart_mask: usize, start: usize, size: usize) {
    ecall(0x52464E43, 1, hart_mask, 0, start, size);
}
