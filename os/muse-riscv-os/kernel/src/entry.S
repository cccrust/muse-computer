    .section .text.entry
    .globl _start
    .align 2
    # v1.0: amoswap needs the A extension; the stock target leaves it off
    # for asm, so enable locally (QEMU virt rv64imafdch has it).
    .option push
    .option arch, +a
_start:
    # Boot protocol (cold boot and SBI HSM hart_start alike): a0 = hartid.
    # (mhartid is M-mode only -- reading it here would trap.)
    # tp = hartid is the kernel-wide invariant (see task::hartid).
    mv tp, a0
    # v1.0: first-hart-wins boot. OpenSBI does not guarantee the boot hart
    # is hart 0 (observed: cold boot on hart 1 with -smp 4). Exactly one
    # hart wins and runs rust_main; the rest park until BOOT_DONE, then
    # join as APs.
    la t0, BOOT_TAKEN
    li t1, 1
    amoswap.w t1, t1, 0(t0)
    bnez t1, _ap_park
    .option pop
    # winner: per-hart boot stack slice, then rust_main
    la sp, boot_stack_top
    slli t2, tp, 14
    sub sp, sp, t2
    call rust_main
1:
    wfi
    j 1b

_ap_park:
    # spin (no wfi: SIE is off this early, nothing would wake us) until
    # the boot hart finishes shared init, then join as an AP.
    la t0, BOOT_DONE
1:
    lw t1, 0(t0)
    beqz t1, 1b
    la sp, boot_stack_top
    slli t2, tp, 14
    sub sp, sp, t2
    mv a0, tp
    call rust_secondary_main
1:
    wfi
    j 1b

    # v1.0: HSM-started AP entry. Same state as _ap_park's tail
    # (a0 = hartid per the HSM start register state).
    .globl _start_secondary
_start_secondary:
    mv tp, a0
    la sp, boot_stack_top
    slli t2, tp, 14
    sub sp, sp, t2
    mv a0, tp
    call rust_secondary_main
1:
    wfi
    j 1b

    .section .bss.stack
    .globl boot_stack
    .align 12
boot_stack:
    # v1.0: 16 KiB per hart (see MAX_HART); hart h uses
    # [top-(h+1)*16K, top-h*16K).
    # NOTE: matched by .bss.* in linker.ld, so clear_bss zeroes it.
    .space 65536
    .globl boot_stack_top
boot_stack_top:

    # v1.0: boot arbitration flags (plain words; MMU off at entry,
    # so linked == physical addresses here).
    .section .bss
    .align 2
    .globl BOOT_TAKEN
BOOT_TAKEN:
    .word 0
    .globl BOOT_DONE
BOOT_DONE:
    .word 0
