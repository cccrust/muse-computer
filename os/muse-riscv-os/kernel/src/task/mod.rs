pub mod context;
pub mod elf;

use crate::mem::{self, pagetable as pt};
use crate::trap::TrapFrame;
use alloc::collections::VecDeque;
use alloc::vec::Vec;

pub const USER_STACK_TOP: usize = 0x7000_0000;
pub const USER_STACK_PAGES: usize = 8;
pub const TRAPFRAME_VA: usize = 0x7fff_e000;

extern "C" {
    fn boot_stack_top();
}

fn trap_stack_top() -> usize {
    boot_stack_top as usize
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum State {
    Runnable,
    Running,
    Blocked,
    Zombie,
}

/// block reasons for State::Blocked
pub const BLOCK_STDIN: u8 = 1;
pub const BLOCK_SLEEP: u8 = 2;
pub const BLOCK_VIRTIO: u8 = 3;
// v1.3: net sleepers (UDP recv / ARP wait). Woken by the net RX ISR via
// wake_net; the timer tick does NOT auto-wake these (unlike virtio-blk's
// watchdog) -- callers use bounded userspace retry instead.
pub const BLOCK_NET: u8 = 4;
// v1.4: waitpid sleepers. Woken by exit() of a child (only when
// blocked_on == WAIT -- a stdin-blocked sh is never disturbed).
pub const BLOCK_WAIT: u8 = 5;

pub struct Proc {
    pub pid: usize,
    pub parent: usize,
    pub state: State,
    pub exit_code: i32,
    pub root: usize,
    pub tf_pa: usize,
    pub brk: usize,
    pub fds: [i32; 16], // index into global FD table? simplified: file handle ids
    pub fd_off: [usize; 16],
    pub fd_kind: [u8; 16], // 0 empty,1 stdin,2 stdout,3 stderr,4 file,5 pipe-r,6 pipe-w
    pub fd_path: [u64; 16], // inode id for files
    pub children: Vec<usize>,
    pub name: [u8; 32],
    pub killed: bool,
    pub kill_code: i32,
    pub blocked_on: u8,
    pub wake_at: u64,
    pub cwd: [u8; 128],
    pub cwd_len: usize,
    // v2.0: filesystem root jail (container groundwork). Absolute paths
    // anchor here, ".." clamps here. NB: `root: usize` above is the page
    // table root -- different thing, hence the longer name.
    pub fsroot: [u8; 128],
    pub fsroot_len: usize,
    pub fd_cloexec: [bool; 16],
    pub mmap_base: usize, // v0.8: top-down anonymous mmap frontier
    pub brk_min: usize,   // v0.8: sbrk may not shrink below this
    pub traced: bool,     // v0.9: strace-lite prints this proc's syscalls
}

struct Sched {
    procs: Vec<Option<Proc>>,
    // v1.1: per-hart runqueues (placement + stealing). Big lock retained
    // (lock split is v1.2); all queue ops are short critical sections.
    queues: [VecDeque<usize>; crate::MAX_HART],
    // v1.0: per-hart state for SMP (global procs under one big lock)
    current: [usize; crate::MAX_HART],
    next_pid: usize,
    yield_flag: [bool; crate::MAX_HART],
    // v1.1: true while the hart sleeps in the idle loop (wfi, SIE=1) --
    // an IPI then wakes it promptly (see kick_idle).
    idle: [bool; crate::MAX_HART],
}

// v1.1: successful cross-hart steals (informational; printed at halt).
static STEALS: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

/// v1.1: scheduler balance stats line (called on the halt path).
/// v1.2: plus the SCHED-lock contention verdict line (v1.4 decision data).
pub fn print_stats() {
    crate::println!(
        "[SMP] steals={}",
        STEALS.load(core::sync::atomic::Ordering::SeqCst)
    );
    crate::println!(
        "[SMP] contention sched={}/{}",
        SCHED_MISS.load(core::sync::atomic::Ordering::SeqCst),
        SCHED_ACQ.load(core::sync::atomic::Ordering::SeqCst)
    );
    // v1.6: heap watermark (fragmentation watch after dealloc coalescing).
    let (hfree, hlarge) = crate::mem::heap::stats();
    crate::println!("[MM] heap free={} largest={}", hfree, hlarge);
}

static mut SCHED: Option<crate::sync::SpinMutex<Sched>> = None;

// TEMP DBG v1.0-2: exclusion detectors (run_on spin + schedule_point).
static mut DEBUG_IN_CRIT: bool = false;
static mut DEBUG_SCHED_IN: bool = false;

// TEMP DBG v1.0-2: push provenance ring + dup detect.
static PUSH_RING: crate::sync::SpinMutex<[(usize, usize, usize); 128]> =
    crate::sync::SpinMutex::new([(0, 0, 0); 128]);
static mut PUSH_IDX: usize = 0;

fn dbg_push(_pid: usize, _reason: usize) {
    // v1.0: silent (a println per schedule drowns the UART under SMP).
    // Re-enable for bring-up forensics if DUP detectors ever fire.
}

fn dbg_dump_pushes() {
    let r = PUSH_RING.lock();
    unsafe {
        crate::println!("[DBG] push ring:");
        let n = PUSH_IDX.min(128);
        let mut k = 0;
        while k < n {
            let (p, h, rr) = r[k];
            crate::println!("[DBG]   pid={} hart={} rsn={}", p, h, rr);
            k += 1;
        }
    }
}

fn sched() -> &'static crate::sync::SpinMutex<Sched> {
    unsafe { SCHED.as_ref().unwrap() }
}

// v1.2: contention verdict data (v1.4 lock decision). Every SCHED
// acquisition goes through here; failed CAS spins are counted.
static SCHED_MISS: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
static SCHED_ACQ: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

fn sched_lock() -> crate::sync::Guard<'static, Sched> {
    SCHED_ACQ.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    sched().lock_counted(&SCHED_MISS)
}

/// v1.3: zero contention counters (called once at boot-markers: boot-time
/// run_on spinning would otherwise dominate the v1.4 verdict numbers).
pub fn contention_reset() {
    SCHED_MISS.store(0, core::sync::atomic::Ordering::SeqCst);
    SCHED_ACQ.store(0, core::sync::atomic::Ordering::SeqCst);
}

/// v1.0: current hart id, from tp (set at entry for every hart).
#[inline(always)]
pub fn hartid() -> usize {
    let tp: usize;
    unsafe {
        core::arch::asm!("mv {}, tp", out(reg) tp);
    }
    tp
}

pub fn set_yield_flag() {
    sched_lock().yield_flag[hartid() % crate::MAX_HART] = true;
}
pub fn take_yield_flag() -> bool {
    let h = hartid() % crate::MAX_HART;
    let mut s = sched_lock();
    let v = s.yield_flag[h];
    s.yield_flag[h] = false;
    v
}

pub fn current_pid() -> usize {
    sched_lock().current[hartid() % crate::MAX_HART]
}

/// v1.1: enqueue pid on hart hq's runqueue. Caller holds the sched lock.
/// Exactly-once (v1.0 rule, per-hart): if pid is still current[] on any
/// hart, that hart owns it and its trap epilogue will enqueue it -- do
/// not push a second copy (two harts would run one task).
/// Returns true if the pid was queued (caller may kick_idle after unlock).
fn enqueue_locked(s: &mut Sched, pid: usize, hq: usize) -> bool {
    let hq = hq % crate::MAX_HART;
    for hh in 0..crate::MAX_HART {
        if s.current[hh] == pid {
            return false;
        }
    }
    s.queues[hq].push_back(pid);
    dbg_push(pid, 4);
    true
}

/// v1.1: scan queues[v] (bounded), drop stale entries (reaped / zombie /
/// blocked / owned by another hart), return the first takeable pid for
/// hart h. Caller validates state was Runnable-or-orphan-Running.
fn pop_valid_locked(s: &mut Sched, h: usize, v: usize) -> Option<usize> {
    let v = v % crate::MAX_HART;
    let n = s.queues[v].len();
    for _ in 0..n {
        let pid = match s.queues[v].pop_front() {
            Some(p) => p,
            None => break,
        };
        let mut owned = false;
        for hh in 0..crate::MAX_HART {
            if hh != h && s.current[hh] == pid {
                owned = true;
                break;
            }
        }
        let live = match s.procs.get(pid) {
            Some(Some(p))
                if p.state == State::Runnable || p.state == State::Running =>
            {
                !owned
            }
            _ => false,
        };
        if live {
            return Some(pid);
        }
        // else: drop stale entry
    }
    None
}

/// v1.1: pick next task for hart h: local queue first, then steal one
/// task per pass from other harts (round-robin). A successful steal bumps
/// STEALS. Caller marks Running + current[h] + idle[h]=false.
fn pick_locked(s: &mut Sched, h: usize) -> Option<usize> {
    let h = h % crate::MAX_HART;
    if let Some(pid) = pop_valid_locked(s, h, h) {
        return Some(pid);
    }
    for off in 1..crate::MAX_HART {
        let v = (h + off) % crate::MAX_HART;
        if let Some(pid) = pop_valid_locked(s, h, v) {
            STEALS.fetch_add(1, core::sync::atomic::Ordering::SeqCst);
            return Some(pid);
        }
    }
    None
}

/// v1.1: IPI-kick every hart that is idle with a non-empty queue.
/// Computed under lock, sent after unlock (ecall from trap/ISR context
/// is legal). Spurious kicks are harmless (soft irq only sets the local
/// yield flag). Without this, an idle hart in wfi waits up to one timer
/// tick (~10ms) to notice newly queued work.
fn kick_idle() {
    let mut mask = 0usize;
    {
        let s = sched_lock();
        for h in 0..crate::MAX_HART {
            if s.idle[h] && !s.queues[h].is_empty() {
                mask |= 1 << h;
            }
        }
    }
    for h in 0..crate::MAX_HART {
        if mask & (1 << h) != 0 {
            crate::sbi::send_ipi(1 << h);
        }
    }
}

fn alloc_tf() -> usize {
    crate::mem::frame::alloc_frame().expect("oom tf")
}

fn tf_of(proc: &Proc) -> *mut TrapFrame {
    // kernel accesses TF via identity PA
    proc.tf_pa as *mut TrapFrame
}

fn tf_va_ptr() -> *mut TrapFrame {
    TRAPFRAME_VA as *mut TrapFrame
}

pub fn init() {
    unsafe {
        SCHED = Some(crate::sync::SpinMutex::new(Sched {
            procs: Vec::new(),
            queues: core::array::from_fn(|_| VecDeque::new()),
            current: [0; crate::MAX_HART],
            next_pid: 1,
            yield_flag: [false; crate::MAX_HART],
            idle: [false; crate::MAX_HART],
        }));
    }
    // create init from embedded ELF
    let elf = crate::embed::INIT_ELF;
    let pid = spawn_from_elf("init", elf, 0);
    crate::println!("[PROC] init pid={}", pid);
    // v1.0: per-hart idle TFs (boot hart, before APs start)
    idle_init();
    // v1.0: scheduler data ready (drivers use this to pick poll vs block).
    // Set here (not in run) so APs see it as soon as they are started.
    SCHED_ACTIVE.store(true, core::sync::atomic::Ordering::SeqCst);
    // pre-spawn shell test tasks? init will exec sh
}

fn new_proc(name: &str, parent: usize) -> (usize, usize, usize) {
    // returns (pid, root, tf_pa)
    let mut s = sched_lock();
    let pid = s.next_pid;
    s.next_pid += 1;
    let root = mem::new_user_space();
    let tf_pa = alloc_tf();
    // map TF VA -> tf_pa (S-only RW, no U)
    pt::map_one(root, TRAPFRAME_VA, tf_pa, pt::PTE_R | pt::PTE_W);
    // reserve slot
    while s.procs.len() <= pid {
        s.procs.push(None);
    }
    (pid, root, tf_pa)
}

fn finish_spawn(
    pid: usize,
    parent: usize,
    root: usize,
    tf_pa: usize,
    name: &str,
    entry: usize,
    brk: usize,
) {
    // map user stack
    mem::alloc_map_user(
        root,
        USER_STACK_TOP - USER_STACK_PAGES * 4096,
        USER_STACK_PAGES * 4096,
        pt::PTE_R | pt::PTE_W,
    );
    // init TF (via PA, identity); empty argv+env on stack
    let sp = push_args(root, &[], &[]);
    unsafe {
        let tf = tf_pa as *mut TrapFrame;
        *tf = TrapFrame::empty();
        (*tf).x[2] = sp;
        (*tf).sepc = entry;
        // SPP=0, SPIE=1, SUM=1
        (*tf).sstatus = (1 << 5) | (1 << 18);
        (*tf).kernel_sp = crate::trap::trap_stack_top();
        // a0 return 0 for child fork handled by caller
    }
    let mut nb = [0u8; 32];
    let bs = name.as_bytes();
    let n = bs.len().min(31);
    nb[..n].copy_from_slice(&bs[..n]);
    let mut s = sched_lock();
    let mut fds = [-1i32; 16];
    let mut kind = [0u8; 16];
    if parent == 0 {
        // init: stdin/out/err
        kind[0] = 1;
        kind[1] = 2;
        kind[2] = 2;
        fds[0] = 0;
        fds[1] = 1;
        fds[2] = 2;
    } else {
        // inherit from parent
        if let Some(Some(p)) = s.procs.get(parent) {
            kind = p.fd_kind;
            fds = p.fds;
        } else {
            kind[0] = 1;
            kind[1] = 2;
            kind[2] = 2;
        }
    }
    let proc = Proc {
        pid,
        parent,
        state: State::Runnable,
        exit_code: 0,
        root,
        tf_pa,
        brk,
        fds,
        fd_off: [0; 16],
        fd_kind: kind,
        fd_path: [0; 16],
        children: Vec::new(),
        killed: false,
        kill_code: -9,
        blocked_on: 0,
        wake_at: 0,
        name: nb,
        cwd: {
            let mut c = [0u8; 128];
            if parent == 0 {
                c[0] = b'/';
                c
            } else {
                // inherit parent cwd (read before taking &mut below would
                // deadlock; parent==0 only at boot so copy after insert)
                c[0] = b'/';
                c
            }
        },
        cwd_len: 1,
        // v2.0: fresh procs start unjailed; parent==0 only at boot.
        fsroot: {
            let mut c = [0u8; 128];
            c[0] = b'/';
            c
        },
        fsroot_len: 1,
        fd_cloexec: [false; 16],
        // v0.8: mmap grows down from below the user stack; brk floor =
        // initial brk (brk is finish_spawn's param, in scope here)
        mmap_base: USER_STACK_TOP - USER_STACK_PAGES * 4096,
        brk_min: brk,
        traced: false,
    };
    s.procs[pid] = Some(proc);
    if parent != 0 {
        // inherit cwd + fsroot from parent (lock already held, direct index)
        let (cc, cl) = match s.procs.get(parent).and_then(|o| o.as_ref()) {
            Some(p) => (p.cwd, p.cwd_len),
            None => {
                let mut c = [0u8; 128];
                c[0] = b'/';
                (c, 1)
            }
        };
        let (rc, rl) = match s.procs.get(parent).and_then(|o| o.as_ref()) {
            Some(p) => (p.fsroot, p.fsroot_len),
            None => {
                let mut c = [0u8; 128];
                c[0] = b'/';
                (c, 1)
            }
        };
        if let Some(Some(me)) = s.procs.get_mut(pid) {
            me.cwd = cc;
            me.cwd_len = cl;
            me.fsroot = rc;
            me.fsroot_len = rl;
        }
        if let Some(Some(pp)) = s.procs.get_mut(parent) {
            pp.children.push(pid);
        }
    }
    // v1.1: spawn onto the current (spawning) hart's queue; idle harts
    // steal from there. Boot: init lands on the boot hart.
    enqueue_locked(&mut s, pid, hartid() % crate::MAX_HART);
    drop(s);
    kick_idle();
}

pub fn spawn_from_elf(name: &str, elf_bytes: &[u8], parent: usize) -> usize {
    let (pid, root, tf_pa) = new_proc(name, parent);
    let info = elf::parse(elf_bytes).expect("bad elf");
    let mut brk = 0usize;
    for i in 0..info.nprog {
        let p = &info.progs[i];
        let flags = elf::pte_flags_for(p.flags);
        // map + copy
        mem::map_user(root, p.vaddr, &elf_bytes[p.offset..p.offset + p.filesz], flags);
        // zero bss part
        if p.memsz > p.filesz {
            let zb = p.vaddr + p.filesz;
            let zl = p.memsz - p.filesz;
            // ensure mapped
            mem::alloc_map_user(root, zb, zl, flags);
            // zero via translate loop
            for k in 0..zl {
                let va = zb + k;
                // temporarily activate? we are in kernel table now, not target root!
                // Need to write via direct frame walk without activation.
                // Use helper: write_byte_to(root, va, 0)
                write_byte_to(root, va, 0);
            }
        }
        // v0.11: enforce this segment's permissions over its whole range.
        // BSS tails (or zero-filesz segments) can share pages with RX
        // text/rodata; first-mapper-wins would leave them non-writable and
        // any .bss store (e.g. user _start) faults with cause=15.
        pt::protect(root, p.vaddr, p.memsz, flags);
        let end = (p.vaddr + p.memsz + 0xfff) & !0xfff;
        if end > brk {
            brk = end;
        }
    }
    // copy for parent==0? need to handle write_byte_to for data too?
    // map_user above used translate on inactive root -> BUG.
    // Fix: map_user must work without activation. Our mem::map_user uses translate
    // on the target root directly (page table walk, no satp switch), so it works
    // even when satp=kernel. translate walks target root tables. Good.
    // But copy in map_user also uses translate on target root -> works. OK.
    finish_spawn(pid, parent, root, tf_pa, name, info.entry, brk.max(0x20000));
    pid
}

fn write_byte_to(root: usize, va: usize, v: u8) {
    if let Some(pa) = pt::translate(root, va) {
        unsafe {
            *(pa as *mut u8) = v;
        }
    }
}

fn write_u64_to(root: usize, va: usize, v: u64) {
    for k in 0..8 {
        write_byte_to(root, va + k, (v >> (k * 8)) as u8);
    }
}

/// Lay out argc/argv + envc/envp on the user stack (inactive root).
/// [sp]=argc u64, [sp+8..]=argv ptr array (NULL-terminated),
/// then envc u64, envp ptr array (NULL-terminated), strings above.
/// Entry (all user _start): a0=[sp], a1=sp+8, a2=a1+8*(a0+1)+8.
/// Returns new sp (16B aligned). Stack pages must already be mapped.
/// NOTE (v0.11): this layout is a locked pair with user _start's env
/// capture -- never revert one without the other (a v0.11 _start on the
/// old layout reads envc from unmapped 0x70000000 and faults at entry).
fn push_args(root: usize, args: &[Vec<u8>], env: &[Vec<u8>]) -> usize {
    let argc = args.len().min(8);
    let envc = env.len().min(16);
    let mut addrs = [0usize; 8];
    let mut envs = [0usize; 16];
    let mut p = USER_STACK_TOP;
    for i in 0..argc {
        let n = args[i].len().min(127);
        p -= n + 1;
        for (k, &ch) in args[i].iter().take(n).enumerate() {
            write_byte_to(root, p + k, ch);
        }
        write_byte_to(root, p + n, 0);
        addrs[i] = p;
    }
    for i in 0..envc {
        let n = env[i].len().min(127);
        p -= n + 1;
        for (k, &ch) in env[i].iter().take(n).enumerate() {
            write_byte_to(root, p + k, ch);
        }
        write_byte_to(root, p + n, 0);
        envs[i] = p;
    }
    p &= !7usize;
    // Tables below the strings, low->high: [argc@sp][argv+NULL][envc]
    // [envp+NULL]. The entry contract requires argv immediately after argc
    // (a1=sp+8) and envp at a1+8*(argc+1)+8, so align sp BEFORE writing
    // (v0.7 #1: never mask an address after placing data at it; v0.11: an
    // earlier revision put envp between argc and argv, breaking argv).
    let need = 8 + 8 * (argc + 1) + 8 + 8 * (envc + 1);
    let sp = p.wrapping_sub(need) & !15usize;
    let argv_base = sp + 8;
    for i in 0..argc {
        write_u64_to(root, argv_base + i * 8, addrs[i] as u64);
    }
    write_u64_to(root, argv_base + argc * 8, 0);
    let envc_slot = argv_base + 8 * (argc + 1);
    write_u64_to(root, envc_slot, envc as u64);
    let envp_base = envc_slot + 8;
    for i in 0..envc {
        write_u64_to(root, envp_base + i * 8, envs[i] as u64);
    }
    write_u64_to(root, envp_base + envc * 8, 0);
    write_u64_to(root, sp, argc as u64);
    debug_assert!(sp & 15 == 0);
    sp
}

/// Fork current process. Returns child pid. Caller sets child a0=0.
pub fn fork(parent_pid: usize) -> usize {
    // gather parent info without holding lock across allocs
    let (p_root, p_tf, p_brk, p_kind, p_fds, p_off, p_path, pname, p_cwd, p_cwd_len, p_cloexec, p_mmap_base, p_brk_min, p_fsroot, p_fsroot_len) = {
        let s = sched_lock();
        let p = s.procs[parent_pid].as_ref().expect("no parent").clone_proc_info();
        p
    };
    let (child, root, tf_pa) = new_proc("fork-child", parent_pid);
    // clone user mappings (U leaves)
    pt::clone_user(p_root, root);
    // map TF
    // (new_proc already mapped TF VA)
    // copy TF content, child return 0
    unsafe {
        core::ptr::copy_nonoverlapping(p_tf as *const u8, tf_pa as *mut u8, 4096);
        let ctf = tf_pa as *mut TrapFrame;
        (*ctf).x[10] = 0;
    }
    // user stack already cloned via clone_user; ensure stack mapping exists
    // brk etc
    let mut s = sched_lock();
    // re-fetch name
    let mut nb = [0u8; 32];
    nb.copy_from_slice(&pname);
    let proc = Proc {
        pid: child,
        parent: parent_pid,
        state: State::Runnable,
        exit_code: 0,
        root,
        tf_pa,
        brk: p_brk,
        fds: p_fds,
        fd_off: p_off,
        fd_kind: p_kind,
        fd_path: p_path,
        children: Vec::new(),
        killed: false,
        kill_code: -9,
        blocked_on: 0,
        wake_at: 0,
        name: nb,
        cwd: p_cwd,
        cwd_len: p_cwd_len,
        // v2.0: jail follows the parent (a child cannot escape by forking)
        fsroot: p_fsroot,
        fsroot_len: p_fsroot_len,
        fd_cloexec: p_cloexec,
        // v0.8: child shares copies of all user pages (clone_user); the
        // mmap frontier must match or parent/child would map the same area
        mmap_base: p_mmap_base,
        brk_min: p_brk_min,
        // v0.9: never inherit trace (avoid log explosion)
        traced: false,
    };
    s.procs[child] = Some(proc);
    if let Some(Some(pp)) = s.procs.get_mut(parent_pid) {
        pp.children.push(child);
    }
    // v1.1: child inherits the parent hart's queue (locality); other
    // harts steal if they idle. Kick after unlock (non-reentrant lock).
    enqueue_locked(&mut s, child, hartid() % crate::MAX_HART);
    drop(s);
    kick_idle();
    // set parent return = child pid (caller does)
    child
}

/// Exec path in current process with argv+env. Both must already be copied
/// out of user memory (address space is replaced here).
pub fn exec(pid: usize, path: &str, args: &[Vec<u8>], env: &[Vec<u8>]) -> bool {
    let data = match crate::fs::read_file(path) {
        Some(d) => d,
        None => return false,
    };
    let info = match elf::parse(&data) {
        Some(i) => i,
        None => return false,
    };
    // new address space
    let new_root = mem::new_user_space();
    let tf_pa = {
        let s = sched_lock();
        s.procs[pid].as_ref().unwrap().tf_pa
    };
    // remap TF into new root
    pt::map_one(new_root, TRAPFRAME_VA, tf_pa, pt::PTE_R | pt::PTE_W);
    let mut brk = 0usize;
    for i in 0..info.nprog {
        let p = &info.progs[i];
        let flags = elf::pte_flags_for(p.flags);
        mem::map_user(new_root, p.vaddr, &data[p.offset..p.offset + p.filesz], flags);
        if p.memsz > p.filesz {
            let zb = p.vaddr + p.filesz;
            let zl = p.memsz - p.filesz;
            mem::alloc_map_user(new_root, zb, zl, flags);
            for k in 0..zl {
                write_byte_to(new_root, zb + k, 0);
            }
        }
        // v0.11: enforce segment permissions (see spawn path above).
        pt::protect(new_root, p.vaddr, p.memsz, flags);
        let end = (p.vaddr + p.memsz + 0xfff) & !0xfff;
        if end > brk {
            brk = end;
        }
    }
    mem::alloc_map_user(
        new_root,
        USER_STACK_TOP - USER_STACK_PAGES * 4096,
        USER_STACK_PAGES * 4096,
        pt::PTE_R | pt::PTE_W,
    );
    // reset TF with argv+env on stack
    let sp = push_args(new_root, args, env);
    unsafe {
        let tf = tf_pa as *mut TrapFrame;
        *tf = TrapFrame::empty();
        (*tf).x[2] = sp;
        (*tf).sepc = info.entry;
        (*tf).sstatus = (1 << 5) | (1 << 18);
        (*tf).kernel_sp = crate::trap::trap_stack_top();
    }
    let mut s = sched_lock();
    if let Some(Some(p)) = s.procs.get_mut(pid) {
        p.root = new_root;
        let new_brk = brk.max(0x20000);
        p.brk = new_brk;
        // v0.8: fresh address space => reset mmap frontier + brk floor
        p.mmap_base = USER_STACK_TOP - USER_STACK_PAGES * 4096;
        p.brk_min = new_brk;
        // close CLOEXEC fds (keep cwd, keep other fds incl. redirections)
        for i in 0..16 {
            if p.fd_cloexec[i] {
                p.fd_kind[i] = 0;
                p.fds[i] = -1;
                p.fd_off[i] = 0;
                p.fd_path[i] = 0;
                p.fd_cloexec[i] = false;
            }
        }
        // reset fds? keep 0,1,2
    }
    // if this is current on our hart, activate immediately (trap exit will
    // use new root only if we activate now; since TF_VA same, just switch)
    let h = hartid() % crate::MAX_HART;
    if s.current[h] == pid {
        pt::activate(new_root);
    }
    drop(s);
    // v1.1: the old root's VAs may sit in other harts' TLBs, and ASIDs are
    // all 0 -- flush remotes so stale entries cannot alias the new root.
    pt::remote_flush_all();
    true
}

pub fn exit(pid: usize, code: i32) {
    let h = hartid() % crate::MAX_HART;
    let next = {
        let mut s = sched_lock();
        match s.procs.get(pid).and_then(|o| o.as_ref()) {
            Some(p) if p.state == State::Zombie => {
                // v1.4 DBG: corpse re-entry (double-run symptom). Dump
                // ownership and idle out instead of switching blindly.
                crate::println!(
                    "[DBG] REEXIT pid={} hart={} current=[{},{},{},{}] idle=[{},{},{},{}] qlen=[{},{},{},{}]",
                    pid,
                    h,
                    s.current[0],
                    s.current[1],
                    s.current[2],
                    s.current[3],
                    s.idle[0] as u8,
                    s.idle[1] as u8,
                    s.idle[2] as u8,
                    s.idle[3] as u8,
                    s.queues[0].len(),
                    s.queues[1].len(),
                    s.queues[2].len(),
                    s.queues[3].len()
                );
                drop(s);
                idle_on_hart(h);
            }
            _ => {}
        }
        if let Some(Some(p)) = s.procs.get_mut(pid) {
            p.state = State::Zombie;
            p.exit_code = code;
        }
        // v1.2: reparent live children to init (pid 1) so orphan zombies
        // are reaped by init's wait loop instead of leaking forever.
        // (If init itself is dying there is no one left to reap; the
        // children stay until the machine halts -- acceptable.)
        if pid != 1 {
            let orphans: Vec<usize> = match s.procs.get(pid).and_then(|o| o.as_ref()) {
                Some(p) => p.children.clone(),
                None => Vec::new(),
            };
            for c in orphans {
                // Move live AND zombie children: init's wait(-1) loop
                // reaps the zombies; a zombie left here would leak both
                // its slot and (unreaped) address space.
                let mut moved = false;
                if let Some(Some(ch)) = s.procs.get_mut(c) {
                    ch.parent = 1;
                    moved = true;
                }
                if moved {
                    if let Some(Some(init)) = s.procs.get_mut(1) {
                        if !init.children.contains(&c) {
                            init.children.push(c);
                        }
                    }
                }
            }
            if let Some(Some(me)) = s.procs.get_mut(pid) {
                me.children.clear();
            }
        }
        // v1.4: wake the parent if it truly sleeps in waitpid
        // (Blocked/WAIT). Only that reason qualifies -- a parent blocked
        // on stdin/pipe/sleep must not be disturbed. The woken parent
        // re-checks (target may be another child) and re-blocks if needed.
        let h = hartid() % crate::MAX_HART;
        let par = match s.procs.get(pid).and_then(|o| o.as_ref()) {
            Some(p) => p.parent,
            None => 0,
        };
        if par != 0 {
            let blocked_wait = match s.procs.get(par).and_then(|o| o.as_ref()) {
                Some(pp) => pp.state == State::Blocked && pp.blocked_on == BLOCK_WAIT,
                None => false,
            };
            if blocked_wait {
                if let Some(Some(pp)) = s.procs.get_mut(par) {
                    pp.state = State::Runnable;
                    pp.blocked_on = 0;
                }
                // borrow ended; enqueue under the same lock (exactly-once:
                // par is not current[] anywhere -- it was Blocked).
                enqueue_locked(&mut s, par, h);
            }
        }
        // pick next runnable
        find_next(&mut s, pid)
    };
    crate::println!("[PROC] pid={} exited({})", pid, code);
    switch_to(next);
}

fn find_next(s: &mut Sched, exclude: usize) -> Option<usize> {
    // v1.1: exiting hart takes local work first, then steals (one task).
    // RR within a queue is preserved (pop front, stale dropped). The
    // exiting pid is dropped wherever met (it just went Zombie).
    let h = hartid() % crate::MAX_HART;
    for off in 0..crate::MAX_HART {
        let v = (h + off) % crate::MAX_HART;
        let n = s.queues[v].len();
        for _ in 0..n {
            let pid = match s.queues[v].pop_front() {
                Some(p) => p,
                None => break,
            };
            if pid == exclude {
                continue;
            }
            let mut owned = false;
            for hh in 0..crate::MAX_HART {
                if hh != h && s.current[hh] == pid {
                    owned = true;
                    break;
                }
            }
            if owned {
                continue; // owned elsewhere: drop this copy, keep scanning
            }
            if let Some(Some(p)) = s.procs.get(pid) {
                if p.state == State::Runnable || p.state == State::Running {
                    // do NOT push back: becomes current
                    if off > 0 {
                        STEALS.fetch_add(1, core::sync::atomic::Ordering::SeqCst);
                    }
                    return Some(pid);
                }
                // zombie skipped
            }
        }
    }
    None
}

pub fn yield_now() {
    // called from trap context only (timer/syscall). Just set flag;
    // actual switch happens in schedule_point.
    set_yield_flag();
}

/// Mark target as killed; it exits on next trap entry. Wakes if blocked.
pub fn kill(pid: usize) -> bool {
    kill_with(pid, -9)
}

fn kill_with(pid: usize, code: i32) -> bool {
    let h = hartid() % crate::MAX_HART;
    let queued = {
        let mut s = sched_lock();
        let blocked = match s.procs.get_mut(pid) {
            Some(Some(p)) => {
                if p.state == State::Zombie {
                    return false;
                }
                p.killed = true;
                p.kill_code = code;
                if p.state == State::Blocked {
                    p.state = State::Runnable;
                    p.blocked_on = 0;
                    true
                } else {
                    false
                }
            }
            _ => return false,
        };
        // borrow of p ended; fresh &mut s for the enqueue
        if blocked {
            enqueue_locked(&mut s, pid, h)
        } else {
            false
        }
    };
    if queued {
        kick_idle();
    }
    true
}

pub fn is_killed(pid: usize) -> bool {
    let s = sched_lock();
    matches!(s.procs.get(pid), Some(Some(p)) if p.killed)
}

/// v2.0: is this pid currently Running (i.e., genuinely executing, not a
/// stale current[] claim on a Blocked/Zombie task)? Gates the trap-entry
/// kill check: a stale killed-zombie lingering in current[] (see switch_to
/// None-arm) must be vacated by the scheduler, not re-executed to death.
pub fn is_running(pid: usize) -> bool {
    let s = sched_lock();
    matches!(s.procs.get(pid), Some(Some(p)) if p.state == State::Running)
}

pub fn kill_code(pid: usize) -> i32 {
    let s = sched_lock();
    s.procs
        .get(pid)
        .and_then(|o| o.as_ref())
        .map(|p| p.kill_code)
        .unwrap_or(-9)
}

// ---- foreground pid for Ctrl-C (v1.0: atomic, shared by all harts) ----
static FG_PID: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

pub fn set_fg(pid: usize) {
    FG_PID.store(pid, core::sync::atomic::Ordering::SeqCst);
}

/// Ctrl-C target: kill foreground (exit 130). Returns false if none.
pub fn kill_fg() -> bool {
    let fg = FG_PID.load(core::sync::atomic::Ordering::SeqCst);
    if fg == 0 {
        return false;
    }
    kill_with(fg, 130)
}

// ---- blocking ----
/// Block current task (trap context only); caller must yield.
pub fn block_current(reason: u8, wake_at: u64) {
    let mut s = sched_lock();
    let cur = s.current[hartid() % crate::MAX_HART];
    if let Some(Some(p)) = s.procs.get_mut(cur) {
        if p.state == State::Running {
            p.state = State::Blocked;
            p.blocked_on = reason;
            p.wake_at = wake_at;
        }
    }
}

/// v1.1: wake one task (caller holds the sched lock). Transitions
/// Blocked->Runnable and enqueues onto the WAKING hart's queue (locality;
/// stealers rebalance). Returns true if queued (caller kicks idle harts
/// after unlock).
fn wake_locked(s: &mut Sched, pid: usize, hq: usize) -> bool {
    let blocked = match s.procs.get_mut(pid) {
        Some(Some(p)) if p.state == State::Blocked => {
            p.state = State::Runnable;
            p.blocked_on = 0;
            true
        }
        _ => false,
    };
    // borrow of p ended; fresh &mut s for the enqueue
    if blocked {
        enqueue_locked(s, pid, hq)
    } else {
        false
    }
}

/// Wake stdin sleepers (UART ISR).
pub fn wake_stdin() {
    let h = hartid() % crate::MAX_HART;
    let queued = {
        let mut s = sched_lock();
        let ids: Vec<usize> = s
            .procs
            .iter()
            .enumerate()
            .filter_map(|(i, o)| match o {
                Some(p) if p.state == State::Blocked && p.blocked_on == BLOCK_STDIN => Some(i),
                _ => None,
            })
            .collect();
        let mut q = false;
        for pid in ids {
            q |= wake_locked(&mut s, pid, h);
        }
        q
    };
    if queued {
        kick_idle();
    }
}

/// Wake virtio-block sleepers (virtio completion ISR).
pub fn wake_virtio() {
    let h = hartid() % crate::MAX_HART;
    let queued = {
        let mut s = sched_lock();
        let ids: Vec<usize> = s
            .procs
            .iter()
            .enumerate()
            .filter_map(|(i, o)| match o {
                Some(p) if p.state == State::Blocked && p.blocked_on == BLOCK_VIRTIO => Some(i),
                _ => None,
            })
            .collect();
        let mut q = false;
        for pid in ids {
            q |= wake_locked(&mut s, pid, h);
        }
        q
    };
    if queued {
        kick_idle();
    }
}

/// Wake net sleepers (net RX ISR: UDP packet arrived or ARP resolved).
/// v1.3: same shape as wake_virtio (enqueue + kick after unlock).
pub fn wake_net() {
    let h = hartid() % crate::MAX_HART;
    let queued = {
        let mut s = sched_lock();
        let ids: Vec<usize> = s
            .procs
            .iter()
            .enumerate()
            .filter_map(|(i, o)| match o {
                Some(p) if p.state == State::Blocked && p.blocked_on == BLOCK_NET => Some(i),
                _ => None,
            })
            .collect();
        let mut q = false;
        for pid in ids {
            q |= wake_locked(&mut s, pid, h);
        }
        q
    };
    if queued {
        kick_idle();
    }
}

/// v0.6: true once the scheduler has started (task::run). Drivers use it
/// to pick polling (boot, kernel context) vs interrupt-blocked wait.
/// v1.0: atomic -- read from all harts (virtio completion path).
pub fn scheduler_active() -> bool {
    SCHED_ACTIVE.load(core::sync::atomic::Ordering::SeqCst)
}

static SCHED_ACTIVE: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

/// Wake expired sleepers (timer tick).
pub fn wake_sleepers(now: u64) {
    let h = hartid() % crate::MAX_HART;
    let queued = {
        let mut s = sched_lock();
        let ids: Vec<usize> = s
            .procs
            .iter()
            .enumerate()
            .filter_map(|(i, o)| match o {
                Some(p)
                    if p.state == State::Blocked
                        && p.blocked_on == BLOCK_SLEEP
                        && now >= p.wake_at =>
                {
                    Some(i)
                }
                _ => None,
            })
            .collect();
        let mut q = false;
        for pid in ids {
            q |= wake_locked(&mut s, pid, h);
        }
        q
    };
    if queued {
        kick_idle();
    }
}

/// Called at end of trap handler with old TF ptr (VA).
/// Switches satp to next task. TF_VA stays same.
/// Invariant (v1.1, per hart): a task is EITHER current[h] (Running) OR
/// queued in some queues[q] (Runnable) OR neither (Blocked/Zombie) --
/// never both. current[h] is vacated the moment its task stops running,
/// so no stale claims.
pub fn schedule_point(_old_tf: *mut TrapFrame) {
    // v1.1: local runqueue first, steal on empty (big lock retained).
    let h = hartid() % crate::MAX_HART;
    let next = {
        let mut s = sched_lock();
        // TEMP DBG v1.0-2: exclusion check on the hottest path
        unsafe {
            if DEBUG_SCHED_IN {
                crate::println!("[DBG] LOCK BROKEN2 on hart{}", h);
            }
            DEBUG_SCHED_IN = true;
            core::arch::asm!("nop; nop; nop; nop; nop; nop; nop; nop");
            DEBUG_SCHED_IN = false;
        }
        let cur = s.current[h];
        // Requeue current if it is still ours. Two cases push:
        // - Running -> Runnable (timeslice / explicit yield);
        // - Runnable (woken between block_current and this epilogue: the
        //   waker set Runnable but left the push to us -- exactly-once).
        // Blocked/Zombie vacates the slot. The push is DIRECT, not via
        // enqueue_locked: we own current[h], and enqueue_locked's
        // ownership check would (correctly for wakers, wrongly for us)
        // skip a self-owned pid and strand the task.
        let mut repush = false;
        if let Some(Some(p)) = s.procs.get_mut(cur) {
            if p.state == State::Running {
                p.state = State::Runnable;
                repush = true;
            } else if p.state == State::Runnable {
                repush = true;
            } else {
                s.current[h] = 0;
            }
        } else {
            s.current[h] = 0;
        }
        if repush {
            s.queues[h].push_back(cur);
            dbg_push(cur, 3);
        }
        // pop next runnable (local-first, then steal)
        match pick_locked(&mut s, h) {
            Some(pid) => {
                if let Some(Some(p)) = s.procs.get_mut(pid) {
                    p.state = State::Running;
                }
                // TEMP DBG v1.0-2: duplicate-run detector. On violation,
                // dump full scheduler state and FREEZE this hart while
                // still holding the lock (other harts spin on it, so the
                // log ends here with everything inspectable).
                {
                    let mut dup = false;
                    for hh in 0..crate::MAX_HART {
                        if hh != h && s.current[hh] == pid {
                            dup = true;
                        }
                    }
                    if dup {
                        crate::println!("[DBG] DUP-SCHED pid={} hart={}", pid, h);
                        crate::println!(
                            "[DBG] current=[{},{},{},{}]",
                            s.current[0], s.current[1], s.current[2], s.current[3]
                        );
                        for (qh, q) in s.queues.iter().enumerate() {
                            crate::println!("[DBG] queue[{}] len={}", qh, q.len());
                        }
                        crate::println!("[DBG] freezing hart{}", h);
                        loop {
                            unsafe { core::arch::asm!("wfi") };
                        }
                    }
                }
                s.current[h] = pid;
                s.idle[h] = false;
                // v1.0: a task may migrate harts; its trap stack must be
                // this hart's (trap.S loads sp from TF on trap entry)
                let tfpa = s.procs[pid].as_ref().unwrap().tf_pa;
                unsafe {
                    (*(tfpa as *mut TrapFrame)).kernel_sp =
                        crate::trap::trap_stack_top_hart(h);
                }
                Some(pid)
            }
            None => {
                // nothing runnable for this hart (current already vacated
                // above); mark idle and sleep below instead of sret into
                // a dead TF. kick_idle() will IPI us when work lands.
                // v2.0: vacate current[] as well (a stale claim combined
                // with a lingering kill flag re-enters do_exit forever).
                s.current[h] = 0;
                s.idle[h] = true;
                None
            }
        }
    };
    if let Some(pid) = next {
        let root = {
            let s = sched_lock();
            s.procs[pid].as_ref().unwrap().root
        };
        pt::activate(root);
        // v1.0: point the trap epilogue at the picked task's TF. After
        // satp switch, TRAPFRAME_VA maps the new task -- but when coming
        // from the idle loop, sscratch still points at the idle TF.
        unsafe {
            core::arch::asm!("csrw sscratch, {0}", in(reg) TRAPFRAME_VA);
        }
    } else {
        idle_on_hart(h);
    }
}

// ---- v1.0 idle loop (SMP liveness) ----
//
// Why this exists: with several harts, a hart can reach schedule_point
// with nothing runnable (its task blocked, queue momentarily empty).
// Returning to trap.S and sret-ing there would resume a dead/Blocked
// task's TF (stale sret), letting one task run on two harts at once.
// Spinning in kernel (SIE=0) is worse: no timer ticks fire, so sleepers
// never wake -- total deadlock once ALL harts idle, even for UART input.
// So a taskless hart srets into a tiny kernel idle loop (wfi, SIE=1):
// traps land in rust_trap_handler with sscratch pointing at a dedicated
// per-hart idle TF, which redrives schedule_point. Full circle, no hangs.
//
// Guest state for idle: sepc=idle_loop, sstatus=SPP|SIE(to-be)|SPIE,
// sscratch=idle TF (identity PA, kernel tables map all RAM), sp left on
// the hart's trap stack (trap.S swaps it with the idle TF anyway).

/// sstatus for idle sret: SPP (stay S-mode) + SPIE (SIE=1 after sret).
const IDLE_SSTATUS: usize = (1 << 8) | (1 << 5);

static mut IDLE_TF_PA: [usize; crate::MAX_HART] = [0; crate::MAX_HART];

/// Allocate per-hart idle TF frames. Boot hart only, before APs start.
pub fn idle_init() {
    for h in 0..crate::MAX_HART {
        let pa = crate::mem::frame::alloc_frame().expect("oom idle tf");
        let tf = pa as *mut TrapFrame;
        unsafe {
            *tf = TrapFrame::empty();
            (*tf).sepc = idle_loop_addr();
            (*tf).sstatus = IDLE_SSTATUS;
            (*tf).kernel_sp = crate::trap::trap_stack_top_hart(h);
            (*tf).x[2] = crate::trap::trap_stack_top_hart(h);
        }
        unsafe {
            IDLE_TF_PA[h] = pa;
        }
    }
}

fn idle_tf(h: usize) -> usize {
    unsafe { IDLE_TF_PA[h % crate::MAX_HART] }
}

extern "C" {
    fn idle_loop();
}

fn idle_loop_addr() -> usize {
    idle_loop as usize
}

core::arch::global_asm!(
    r#"
    .section .text
    .globl idle_loop
    .globl _idle_loop_end
    .align 2
idle_loop:
    wfi
    j idle_loop
_idle_loop_end:
"#
);

/// v2.0: idle loop address range (for the nested-kernel-trap detector:
/// traps with sepc in here are legitimate idle wakeups, not bugs).
pub fn idle_range() -> (usize, usize) {
    extern "C" {
        fn _idle_loop_end();
    }
    (idle_loop_addr(), _idle_loop_end as usize)
}

/// Enter the idle loop on this hart. Diverges (sret to kernel wfi loop);
/// a later trap redrives schedule_point, which may pick a real task.
/// v1.4: parks on the immortal KERNEL_ROOT, never on a task root. An idle
/// hart keeps its satp across wfi; if it parked on a task root that later
/// gets reaped+recycled (v1.2 teardown), the next timer trap would walk
/// kernel mappings through garbage tables (observed: kernel STORE faults
/// inside __trap_entry, then nested-trap TF clobbering with kernel sepc).
/// The kernel root is never freed and maps everything idle/traps need
/// (text, stacks, heap, idle TFs via identity, MMIO).
fn idle_on_hart(h: usize) -> ! {
    let tf = idle_tf(h);
    crate::mem::pagetable::activate(crate::mem::kernel_root());
    unsafe {
        core::arch::asm!(
            "csrw sscratch, {tf}",
            "csrw sepc, {sepc}",
            "csrw sstatus, {sst}",
            "sret",
            tf = in(reg) tf,
            sepc = in(reg) idle_loop_addr(),
            sst = in(reg) IDLE_SSTATUS,
            options(noreturn),
        );
    }
}

fn switch_to(next: Option<usize>) {    match next {
        Some(pid) => {
            // v1.0: exiting task's hart takes the next task
            let h = hartid() % crate::MAX_HART;
            let root = {
                let mut s = sched_lock();
                // re-validate under lock: the pick was made under lock in
                // find_next, but the lock dropped before we got here; a
                // reaped pid must never be entered (paused here as Running
                // avoids the reap path, but check anyway -- never panic).
                let ok = matches!(s.procs.get(pid), Some(Some(p)) if p.state == State::Runnable || p.state == State::Running);
                if !ok {
                    drop(s);
                    // fall back to scheduling: re-enter trap epilogue path
                    // by idling; a later trap will pick a live task
                    idle_on_hart(h);
                }
                // v1.4 DBG+HARDEN: never enter a task owned elsewhere
                // (schedule_point's DUP detector only covers its own picks;
                // switch_to had no check). Idle out instead of double-run.
                let mut dup = false;
                for hh in 0..crate::MAX_HART {
                    if hh != h && s.current[hh] == pid {
                        dup = true;
                        break;
                    }
                }
                if dup {
                    crate::println!("[DBG] DUP-SWITCH pid={} hart={}", pid, h);
                    drop(s);
                    idle_on_hart(h);
                }
                s.current[h] = pid;
                s.idle[h] = false;
                if let Some(Some(p)) = s.procs.get_mut(pid) {
                    p.state = State::Running;
                    unsafe {
                        (*(p.tf_pa as *mut TrapFrame)).kernel_sp =
                            crate::trap::trap_stack_top_hart(h);
                    }
                    p.root
                } else {
                    drop(s);
                    idle_on_hart(h);
                }
            };
            pt::activate(root);
            enter_user(pid);
        }
        None => {
            // v1.0: never shut down here. "Queue empty" only means THIS
            // hart has nothing to run -- other harts may own Running
            // tasks (in userspace between traps) or Blocked tasks that a
            // later IRQ/tick will wake. Idling (wfi, SIE=1) lets a later
            // trap redrive schedule_point. Global power-off is only via
            // the explicit SYS_SHUTDOWN (halt) path.
            // v1.1: mark idle under lock so kick_idle() can target us.
            // v2.0: vacate current[] too -- a stale claim here makes the
            // trap-entry kill check re-execute a dead task forever.
            let h = hartid() % crate::MAX_HART;
            {
                let mut s = sched_lock();
                s.idle[h] = true;
                s.current[h] = 0;
            }
            idle_on_hart(h);
        }
    }
}

/// v1.0: first entry to userspace on a hart. APs call this from
/// rust_secondary_main after task::init (done by the boot hart).
/// Spins (no wfi: SIE is off in kernel context) until a task appears;
/// init's forks feed the queues, shutdown powers the machine off.
/// v1.1: local runqueue first, then steal (same take rules as
/// schedule_point); never panics on stale entries.
pub fn run_on(hart: usize) -> ! {
    let h = hart % crate::MAX_HART;
    loop {
        let mut s = sched_lock();
        // TEMP DBG: exclusion check
        unsafe {
            if DEBUG_IN_CRIT {
                crate::println!("[DBG] LOCK BROKEN run_on hart{}", h);
            }
            DEBUG_IN_CRIT = true;
            core::arch::asm!("nop; nop; nop; nop; nop; nop; nop; nop");
            DEBUG_IN_CRIT = false;
        }
        if let Some(pid) = pick_locked(&mut s, h) {
            s.current[h] = pid;
            s.idle[h] = false;
            if let Some(Some(p)) = s.procs.get_mut(pid) {
                p.state = State::Running;
                unsafe {
                    (*(p.tf_pa as *mut TrapFrame)).kernel_sp =
                        crate::trap::trap_stack_top_hart(h);
                }
                // capture root under the same lock: no reap window
                let root = p.root;
                drop(s);
                pt::activate(root);
                enter_user(pid);
                unreachable!();
            }
            // vanished under us (impossible: we own it as Running);
            // loop and pick again rather than panic
        }
        // v1.1: SIE=0 here so a pending IPI never traps; poll-ack it so
        // the boot self-test observes delivery even if this hart never
        // reaches userspace (boot hart may do all early work itself).
        crate::trap::poll_soft_ack();
        core::hint::spin_loop();
    }
}

pub fn run() -> ! {
    run_on(hartid() % crate::MAX_HART)
}

fn enter_user(_pid: usize) -> ! {
    unsafe {
        let cur = current_pid();
        let (tf_pa, sepc, sstatus) = {
            let s = sched_lock();
            let p = s.procs[cur].as_ref().unwrap();
            (p.tf_pa, (p.tf_pa as *const TrapFrame).as_ref().unwrap().sepc, (p.tf_pa as *const TrapFrame).as_ref().unwrap().sstatus)
        };
        // set sepc/sstatus/sscratch then load regs via inline asm trampoline
        core::arch::asm!(
            "csrw sepc, {sepc}",
            "csrw sstatus, {sst}",
            "csrw sscratch, {tfva}",
            // a0 = tf_pa for loader
            "mv a0, {tfpa}",
            "call __enter_user_asm",
            sepc = in(reg) sepc,
            sst = in(reg) sstatus,
            tfva = in(reg) TRAPFRAME_VA,
            tfpa = in(reg) tf_pa,
        );
        unreachable!()
    }
}

// Assembly trampoline: a0 = TF pa, sscratch already = TF_VA
core::arch::global_asm!(
    r#"
    .section .text
    .globl __enter_user_asm
    .align 2
__enter_user_asm:
    mv t0, a0
    ld t1, 32*8(t0)
    csrw sstatus, t1
    ld t1, 33*8(t0)
    csrw sepc, t1
    mv t0, a0
    # sscratch already TF_VA
    # NOTE (v1.0): x4/tp is NOT restored -- it holds the hart id (kernel
    # state set at entry); fresh TrapFrames zero it, which would wipe the
    # hartid on first entry and misroute every per-hart lookup after.
    ld x1, 1*8(t0)
    ld x3, 3*8(t0)
    ld x6, 6*8(t0)
    ld x7, 7*8(t0)
    ld x8, 8*8(t0)
    ld x9, 9*8(t0)
    ld x11, 11*8(t0)
    ld x12, 12*8(t0)
    ld x13, 13*8(t0)
    ld x14, 14*8(t0)
    ld x15, 15*8(t0)
    ld x16, 16*8(t0)
    ld x17, 17*8(t0)
    ld x18, 18*8(t0)
    ld x19, 19*8(t0)
    ld x20, 20*8(t0)
    ld x21, 21*8(t0)
    ld x22, 22*8(t0)
    ld x23, 23*8(t0)
    ld x24, 24*8(t0)
    ld x25, 25*8(t0)
    ld x26, 26*8(t0)
    ld x27, 27*8(t0)
    ld x28, 28*8(t0)
    ld x29, 29*8(t0)
    ld x30, 30*8(t0)
    ld x31, 31*8(t0)
    ld x10, 10*8(t0)
    ld sp, 2*8(t0)
    ld t0, 5*8(t0)
    sret
"#
);

// helpers for syscall layer (v1.0: per-hart current)
pub fn with_current<R>(f: impl FnOnce(&Proc) -> R) -> R {
    let s = sched_lock();
    let pid = s.current[hartid() % crate::MAX_HART];
    let p: &Proc = s.procs[pid].as_ref().unwrap();
    f(p)
}
pub fn with_current_mut<R>(f: impl FnOnce(&mut Proc) -> R) -> R {
    let mut s = sched_lock();
    let pid = s.current[hartid() % crate::MAX_HART];
    let p: &mut Proc = s.procs[pid].as_mut().unwrap();
    f(p)
}

trait CloneInfo {
    #[allow(clippy::type_complexity)]
    fn clone_proc_info(
        &self,
    ) -> (
        usize,
        usize,
        usize,
        [u8; 16],
        [i32; 16],
        [usize; 16],
        [u64; 16],
        [u8; 32],
        [u8; 128],
        usize,
        [bool; 16],
        usize,
        usize,
        [u8; 128],
        usize,
    );
}
impl CloneInfo for Proc {
    fn clone_proc_info(
        &self,
    ) -> (
        usize,
        usize,
        usize,
        [u8; 16],
        [i32; 16],
        [usize; 16],
        [u64; 16],
        [u8; 32],
        [u8; 128],
        usize,
        [bool; 16],
        usize,
        usize,
        [u8; 128],
        usize,
    ) {
        (
            self.root,
            self.tf_pa,
            self.brk,
            self.fd_kind,
            self.fds,
            self.fd_off,
            self.fd_path,
            self.name,
            self.cwd,
            self.cwd_len,
            self.fd_cloexec,
            self.mmap_base,
            self.brk_min,
            self.fsroot,
            self.fsroot_len,
        )
    }
}

pub fn wait(pid: usize) -> (i32, usize) {
    waitpid(pid, -1, 0)
}

/// WNOHANG option bit for waitpid.
pub const WNOHANG: usize = 1;

/// Wait for a child: target>0 waits that pid, else any child.
/// Returns (child_pid_or_status, code):
/// - (c, code) zombie reaped; (-1, 0) no children; (-2, 0) would block
///   (caller yields and retries); (0, 0) WNOHANG no zombie yet.
pub fn waitpid(pid: usize, target: isize, options: usize) -> (i32, usize) {
    let nohang = options & WNOHANG != 0;
    let child = {
        let mut s = sched_lock();
        let children = match s.procs.get(pid).and_then(|o| o.as_ref()) {
            Some(p) => p.children.clone(),
            None => return (-1, 0),
        };
        let mut found: Option<(usize, i32)> = None;
        for c in &children {
            if target > 0 && *c as isize != target {
                continue;
            }
            if let Some(Some(p)) = s.procs.get(*c) {
                if p.state == State::Zombie {
                    found = Some((*c, p.exit_code));
                    break;
                }
            }
        }
        if let Some((c, code)) = found {
            // v1.2: capture the dead address space, THEN release everything
            // without the sched lock (slot is gone; nobody can touch it).
            let dead = match s.procs[c].take() {
                Some(p) => Some((p.root, p.tf_pa)),
                None => None,
            };
            // v1.1: purge stale entries from ALL runqueues (pick paths
            // filter strays, but keep the queues clean anyway).
            for q in s.queues.iter_mut() {
                q.retain(|&x| x != c);
            }
            if let Some(Some(pp)) = s.procs.get_mut(pid) {
                pp.children.retain(|&x| x != c);
            }
            if let Some((root, tf_pa)) = dead {
                drop(s);
                crate::mem::free_user_space(root);
                crate::mem::frame::dealloc_frame(tf_pa);
                // freed frames are reused under ASID 0: flush remotes so
                // stale TLB entries cannot alias the next mappings.
                crate::mem::pagetable::remote_flush_all();
            }
            Some((c as i32, code as usize))
        } else {
            // any live (non-zombie, slot present) matching children?
            let mut any = false;
            for c in &children {
                if target > 0 && *c as isize != target {
                    continue;
                }
                if s.procs.get(*c).and_then(|o| o.as_ref()).is_some() {
                    any = true;
                    break;
                }
            }
            if !any {
                Some((-1, 0))
            } else if nohang {
                Some((0, 0))
            } else {
                // v1.4: true blocking (was yield-spin). Mark Blocked/WAIT
                // in THIS critical section -- atomic with the zombie check
                // above, so an exit()+wake cannot slip between (no lost
                // wakeup). The trap epilogue deschedules us; exit() of any
                // child wakes us. Userspace protocol unchanged (-2 retry).
                let h = hartid() % crate::MAX_HART;
                let cur = s.current[h];
                if let Some(Some(p)) = s.procs.get_mut(cur) {
                    if p.state == State::Running {
                        p.state = State::Blocked;
                        p.blocked_on = BLOCK_WAIT;
                        p.wake_at = 0;
                    }
                }
                None
            }
        }
    };
    if let Some(v) = child {
        return (v.0, v.1);
    }
    // no zombie yet: yield and return WouldBlock; user retries.
    // Do NOT call schedule_point here (would switch satp and corrupt TF).
    // Outer trap handler will switch after we return.
    yield_now();
    (-2, 0)
}

// ---- cwd + path resolution (v0.5) ----
pub fn get_cwd(pid: usize) -> alloc::string::String {
    let s = sched_lock();
    match s.procs.get(pid).and_then(|o| o.as_ref()) {
        Some(p) => {
            let n = p.cwd_len.min(128);
            alloc::string::String::from_utf8_lossy(&p.cwd[..n]).into_owned()
        }
        None => alloc::string::String::from("/"),
    }
}

pub fn set_cwd(pid: usize, cwd: &str) -> bool {
    if !cwd.starts_with('/') || cwd.len() > 127 {
        return false;
    }
    let mut s = sched_lock();
    match s.procs.get_mut(pid).and_then(|o| o.as_mut()) {
        Some(p) => {
            let b = cwd.as_bytes();
            p.cwd[..b.len()].copy_from_slice(b);
            for i in b.len()..128 {
                p.cwd[i] = 0;
            }
            p.cwd_len = b.len();
            true
        }
        None => false,
    }
}

/// Resolve user path inside a jail: absolute paths anchor at `root`
/// (`/bin/sh` -> `<root>/bin/sh`), relative ones at `cwd`; `..` clamps at
/// the root boundary (the stack never shrinks below the root components).
/// Output is always absolute and jail-contained BY CONSTRUCTION -- callers
/// cannot bypass it, so every syscall going through resolve_for() is
/// automatically confined. Over-long (>127) returns a clamped path.
/// root=cwd="/" reproduces the pre-v2.0 behavior bit-for-bit.
pub fn resolve_path(root: &str, cwd: &str, path: &str) -> alloc::string::String {
    use alloc::string::String;
    use alloc::vec::Vec;
    // root components form the immovable base of the stack.
    // NOTE (borrowck): `parts` borrows `root` (param) and `joined` (local);
    // both outlive it, so keep `joined` alive to the end of the function.
    let mut parts: Vec<&str> = Vec::new();
    for comp in root.split('/') {
        match comp {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            c => parts.push(c),
        }
    }
    let base_len = parts.len();
    // anchor: absolute paths start from root (strip the leading '/'),
    // relative ones from cwd-inside-root (cwd is the jail-absolute view).
    let joined: String = if path.starts_with('/') {
        String::from(&path[1..])
    } else {
        alloc::format!("{}/{}", cwd.trim_start_matches('/'), path)
    };
    for comp in joined.split('/') {
        match comp {
            "" | "." => {}
            ".." => {
                if parts.len() > base_len {
                    parts.pop();
                }
            }
            c => parts.push(c),
        }
    }
    let mut out = String::from("/");
    out.push_str(&parts.join("/"));
    if out.len() > 127 {
        out.truncate(127);
    }
    out
}

pub fn resolve_for(pid: usize, path: &str) -> alloc::string::String {
    let s = sched_lock();
    let (root, cwd) = match s.procs.get(pid).and_then(|o| o.as_ref()) {
        Some(p) => {
            let rn = p.fsroot_len.min(128);
            let cn = p.cwd_len.min(128);
            (
                alloc::string::String::from_utf8_lossy(&p.fsroot[..rn]).into_owned(),
                alloc::string::String::from_utf8_lossy(&p.cwd[..cn]).into_owned(),
            )
        }
        None => (alloc::string::String::from("/"), alloc::string::String::from("/")),
    };
    drop(s);
    resolve_path(&root, &cwd, path)
}

/// v2.0: read/write the fs jail root. set_root takes an ALREADY-RESOLVED
/// absolute host path (sys_chroot resolves first under the OLD root, so
/// the new root is a descendant by construction -- tightening only).
pub fn get_root(pid: usize) -> alloc::string::String {
    let s = sched_lock();
    match s.procs.get(pid).and_then(|o| o.as_ref()) {
        Some(p) => {
            let n = p.fsroot_len.min(128);
            alloc::string::String::from_utf8_lossy(&p.fsroot[..n]).into_owned()
        }
        None => alloc::string::String::from("/"),
    }
}

pub fn set_root(pid: usize, root: &str) -> bool {
    if !root.starts_with('/') || root.len() > 127 || root.is_empty() {
        return false;
    }
    let mut s = sched_lock();
    match s.procs.get_mut(pid).and_then(|o| o.as_mut()) {
        Some(p) => {
            let b = root.as_bytes();
            p.fsroot[..b.len()].copy_from_slice(b);
            for i in b.len()..128 {
                p.fsroot[i] = 0;
            }
            p.fsroot_len = b.len();
            // cwd is container-view; re-anchor it at the new root to keep
            // (root, cwd) consistent (old cwd may point outside the jail).
            p.cwd[0] = b'/';
            for i in 1..128 {
                p.cwd[i] = 0;
            }
            p.cwd_len = 1;
            true
        }
        None => false,
    }
}

// ---- v0.9: strace flag + ps snapshot ----
pub fn set_traced(pid: usize, on: bool) -> bool {
    let mut s = sched_lock();
    match s.procs.get_mut(pid).and_then(|o| o.as_mut()) {
        Some(p) => {
            p.traced = on;
            true
        }
        None => false,
    }
}

pub fn is_traced(pid: usize) -> bool {
    let s = sched_lock();
    matches!(s.procs.get(pid), Some(Some(p)) if p.traced)
}

/// One line per live proc: "pid ppid state brk cwd\n". state: R/B/Z.
pub fn ps_snapshot() -> alloc::string::String {
    let s = sched_lock();
    let mut out = alloc::string::String::new();
    for slot in s.procs.iter() {
        if let Some(p) = slot {
            let st = match p.state {
                State::Running | State::Runnable => "R",
                State::Blocked => "B",
                State::Zombie => "Z",
            };
            let n = p.cwd_len.min(128);
            let cwd = alloc::string::String::from_utf8_lossy(&p.cwd[..n]);
            out.push_str(&alloc::format!(
                "{} {} {} {} {}\n",
                p.pid, p.parent, st, p.brk, cwd
            ));
        }
    }
    out
}
