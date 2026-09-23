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

// v2.0 container rootfs assembler: `ctr <name>` creates /ctr/<name>/bin
// and hardlinks every /bin entry into it (zero-copy: links share inodes).
// Idempotent (existing dirs/files are skipped, never fails on rerun).
fn mkdir_p(path: &[u8]) {
    // path is a NUL-terminated stack buffer built by caller
    if user_lib::mkdir(path.as_ptr()) != 0 {
        // exists already is fine; other errors surface at link time
    }
}

#[no_mangle]
pub extern "C" fn main(argc: usize, argv: *const *const u8) {
    if argc < 2 {
        user_lib::print("usage: ctr <name>\n");
        user_lib::exit(1);
    }
    let name = match unsafe { user_lib::argv_str(argv, 1, argc) } {
        Some(s) if !s.is_empty() && s.len() < 24 => s,
        _ => {
            user_lib::print("ctr: bad name\n");
            user_lib::exit(1);
        }
    };
    // reject path tricks in the name itself (no /, no ..)
    for &c in name.iter() {
        if c == b'/' || c == 0 {
            user_lib::print("ctr: bad name\n");
            user_lib::exit(1);
        }
    }
    if name.len() >= 2 {
        let mut i = 0;
        while i + 1 < name.len() {
            if name[i] == b'.' && name[i + 1] == b'.' {
                user_lib::print("ctr: bad name\n");
                user_lib::exit(1);
            }
            i += 1;
        }
    }
    // /ctr/<name>/bin
    let mut base = [0u8; 64];
    base[..5].copy_from_slice(b"/ctr/");
    base[5..5 + name.len()].copy_from_slice(name);
    let basen = 5 + name.len();
    let mut root = [0u8; 64];
    root[..basen].copy_from_slice(&base[..basen]);
    mkdir_p(b"/ctr\0");
    mkdir_p(&root);
    let mut bindir = [0u8; 64];
    bindir[..basen].copy_from_slice(&base[..basen]);
    bindir[basen..basen + 4].copy_from_slice(b"/bin");
    mkdir_p(&bindir);
    // link every /bin/<f> -> <root>/bin/<f>
    let mut nb = [0u8; 512];
    let r = user_lib::getdents(b"/bin\0".as_ptr(), nb.as_mut_ptr(), 512);
    if r <= 0 {
        user_lib::print("ctr: empty /bin\n");
        user_lib::exit(1);
    }
    let mut off = 0usize;
    let mut n = 0;
    while off < r as usize && off < 511 {
        // NUL-terminated name at nb[off..]
        let mut len = 0;
        while off + len < 511 && nb[off + len] != 0 {
            len += 1;
        }
        if len == 0 || len > 28 {
            break;
        }
        let mut src = [0u8; 64];
        src[..5].copy_from_slice(b"/bin/");
        src[5..5 + len].copy_from_slice(&nb[off..off + len]);
        let mut dst = [0u8; 64];
        dst[..basen + 4].copy_from_slice(&bindir[..basen + 4]);
        dst[basen + 4] = b'/';
        dst[basen + 5..basen + 5 + len].copy_from_slice(&nb[off..off + len]);
        // exists already -> skip (idempotent reruns)
        user_lib::link(src.as_ptr(), dst.as_ptr());
        n += 1;
        off += len + 1;
        if n >= 32 {
            break;
        }
    }
    user_lib::print("ready ");
    let _ = user_lib::write(1, base.as_ptr(), basen);
    user_lib::print("\n");
    user_lib::exit(0);
}
