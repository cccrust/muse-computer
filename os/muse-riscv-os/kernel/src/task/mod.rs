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
    pub fd_cloexec: [bool; 16],
    pub mmap_base: usize, // v0.8: top-down anonymous mmap frontier
    pub brk_min: usize,   // v0.8: sbrk may not shrink below this
    pub traced: bool,     // v0.9: strace-lite prints this proc's syscalls
}

struct Sched {
    procs: Vec<Option<Proc>>,
    queue: VecDeque<usize>,
    current: usize,
    next_pid: usize,
    yield_flag: bool,
}

static mut SCHED: Option<crate::sync::SpinMutex<Sched>> = None;

fn sched() -> &'static crate::sync::SpinMutex<Sched> {
    unsafe { SCHED.as_ref().unwrap() }
}

pub fn set_yield_flag() {
    sched().lock().yield_flag = true;
}
pub fn take_yield_flag() -> bool {
    let mut s = sched().lock();
    let v = s.yield_flag;
    s.yield_flag = false;
    v
}

pub fn current_pid() -> usize {
    sched().lock().current
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
            queue: VecDeque::new(),
            current: 0,
            next_pid: 1,
            yield_flag: false,
        }));
    }
    // create init from embedded ELF
    let elf = crate::embed::INIT_ELF;
    let pid = spawn_from_elf("init", elf, 0);
    crate::println!("[PROC] init pid={}", pid);
    // pre-spawn shell test tasks? init will exec sh
}

fn new_proc(name: &str, parent: usize) -> (usize, usize, usize) {
    // returns (pid, root, tf_pa)
    let mut s = sched().lock();
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
    // init TF (via PA, identity); empty argv on stack
    let sp = push_args(root, &[]);
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
    let mut s = sched().lock();
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
        fd_cloexec: [false; 16],
        // v0.8: mmap grows down from below the user stack; brk floor =
        // initial brk (brk is finish_spawn's param, in scope here)
        mmap_base: USER_STACK_TOP - USER_STACK_PAGES * 4096,
        brk_min: brk,
        traced: false,
    };
    s.procs[pid] = Some(proc);
    if parent != 0 {
        // inherit cwd from parent (lock already held, direct index)
        let (cc, cl) = match s.procs.get(parent).and_then(|o| o.as_ref()) {
            Some(p) => (p.cwd, p.cwd_len),
            None => {
                let mut c = [0u8; 128];
                c[0] = b'/';
                (c, 1)
            }
        };
        if let Some(Some(me)) = s.procs.get_mut(pid) {
            me.cwd = cc;
            me.cwd_len = cl;
        }
        if let Some(Some(pp)) = s.procs.get_mut(parent) {
            pp.children.push(pid);
        }
    }
    s.queue.push_back(pid);
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

/// Lay out argc/argv on the user stack (inactive root, via page walk).
/// [sp]=argc u64, [sp+8..]=argv ptr array (NULL-terminated), strings above.
/// Returns new sp (16B aligned). Stack pages must already be mapped.
fn push_args(root: usize, args: &[Vec<u8>]) -> usize {
    let argc = args.len().min(8);
    let mut addrs = [0usize; 8];
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
    p &= !7usize;
    // Reserve argv array + argc slot so the final sp is 16B aligned.
    // (Old code masked sp with !15 AFTER layout, which could slide sp up
    // to 8B below the argc slot whenever the string bytes totalled 0..7
    // mod 16 -- entry then read argc from the wrong address. v0.7 #1.)
    if p.wrapping_sub(8 * (argc + 1) + 8) & 15 != 0 {
        p -= 8; // pad between strings and argv (inside mapped stack pages)
    }
    let argv_base = p - 8 * (argc + 1);
    for i in 0..argc {
        write_u64_to(root, argv_base + i * 8, addrs[i] as u64);
    }
    write_u64_to(root, argv_base + argc * 8, 0);
    let sp = argv_base - 8;
    write_u64_to(root, sp, argc as u64);
    debug_assert!(sp & 15 == 0);
    sp
}

/// Fork current process. Returns child pid. Caller sets child a0=0.
pub fn fork(parent_pid: usize) -> usize {
    // gather parent info without holding lock across allocs
    let (p_root, p_tf, p_brk, p_kind, p_fds, p_off, p_path, pname, p_cwd, p_cwd_len, p_cloexec, p_mmap_base, p_brk_min) = {
        let s = sched().lock();
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
    let mut s = sched().lock();
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
    s.queue.push_back(child);
    // set parent return = child pid (caller does)
    child
}

/// Exec path in current process with argv. Args must already be copied out
/// of user memory (address space is replaced here).
pub fn exec(pid: usize, path: &str, args: &[Vec<u8>]) -> bool {
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
        let s = sched().lock();
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
    // reset TF with argv on stack
    let sp = push_args(new_root, args);
    unsafe {
        let tf = tf_pa as *mut TrapFrame;
        *tf = TrapFrame::empty();
        (*tf).x[2] = sp;
        (*tf).sepc = info.entry;
        (*tf).sstatus = (1 << 5) | (1 << 18);
        (*tf).kernel_sp = crate::trap::trap_stack_top();
    }
    let mut s = sched().lock();
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
    // if this is current, activate immediately (trap exit will use new root only
    // if we activate now; since TF_VA same, just switch satp)
    if s.current == pid {
        pt::activate(new_root);
    }
    true
}

pub fn exit(pid: usize, code: i32) {
    let next = {
        let mut s = sched().lock();
        if let Some(Some(p)) = s.procs.get_mut(pid) {
            p.state = State::Zombie;
            p.exit_code = code;
        }
        // wake parent if waiting? parent polls.
        // pick next runnable
        find_next(&mut s, pid)
    };
    crate::println!("[PROC] pid={} exited({})", pid, code);
    switch_to(next);
}

fn find_next(s: &mut Sched, exclude: usize) -> Option<usize> {
    // RR: pop front until runnable (queue = waiting only, current separate)
    let n = s.queue.len();
    for _ in 0..n {
        if let Some(pid) = s.queue.pop_front() {
            if pid == exclude {
                continue;
            }
            if let Some(Some(p)) = s.procs.get(pid) {
                if p.state == State::Runnable || p.state == State::Running {
                    // do NOT push back: becomes current
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
    let mut s = sched().lock();
    match s.procs.get_mut(pid) {
        Some(Some(p)) => {
            if p.state == State::Zombie {
                return false;
            }
            p.killed = true;
            p.kill_code = code;
            if p.state == State::Blocked {
                p.state = State::Runnable;
                p.blocked_on = 0;
                s.queue.push_back(pid);
            }
            true
        }
        _ => false,
    }
}

pub fn is_killed(pid: usize) -> bool {
    let s = sched().lock();
    matches!(s.procs.get(pid), Some(Some(p)) if p.killed)
}

pub fn kill_code(pid: usize) -> i32 {
    let s = sched().lock();
    s.procs
        .get(pid)
        .and_then(|o| o.as_ref())
        .map(|p| p.kill_code)
        .unwrap_or(-9)
}

// ---- foreground pid for Ctrl-C ----
static mut FG_PID: usize = 0;

pub fn set_fg(pid: usize) {
    unsafe {
        FG_PID = pid;
    }
}

/// Ctrl-C target: kill foreground (exit 130). Returns false if none.
pub fn kill_fg() -> bool {
    let fg = unsafe { FG_PID };
    if fg == 0 {
        return false;
    }
    kill_with(fg, 130)
}

// ---- blocking ----
/// Block current task (trap context only); caller must yield.
pub fn block_current(reason: u8, wake_at: u64) {
    let mut s = sched().lock();
    let cur = s.current;
    if let Some(Some(p)) = s.procs.get_mut(cur) {
        if p.state == State::Running {
            p.state = State::Blocked;
            p.blocked_on = reason;
            p.wake_at = wake_at;
        }
    }
}

fn wake_locked(s: &mut Sched, pid: usize) {
    if let Some(Some(p)) = s.procs.get_mut(pid) {
        if p.state == State::Blocked {
            p.state = State::Runnable;
            p.blocked_on = 0;
            s.queue.push_back(pid);
        }
    }
}

/// Wake stdin sleepers (UART ISR).
pub fn wake_stdin() {
    let mut s = sched().lock();
    let ids: Vec<usize> = s
        .procs
        .iter()
        .enumerate()
        .filter_map(|(i, o)| match o {
            Some(p) if p.state == State::Blocked && p.blocked_on == BLOCK_STDIN => Some(i),
            _ => None,
        })
        .collect();
    for pid in ids {
        wake_locked(&mut s, pid);
    }
}

/// Wake virtio-block sleepers (virtio completion ISR).
pub fn wake_virtio() {
    let mut s = sched().lock();
    let ids: Vec<usize> = s
        .procs
        .iter()
        .enumerate()
        .filter_map(|(i, o)| match o {
            Some(p) if p.state == State::Blocked && p.blocked_on == BLOCK_VIRTIO => Some(i),
            _ => None,
        })
        .collect();
    for pid in ids {
        wake_locked(&mut s, pid);
    }
}

/// v0.6: true once the scheduler has started (task::run). Drivers use it
/// to pick polling (boot, kernel context) vs interrupt-blocked wait.
pub fn scheduler_active() -> bool {
    unsafe { SCHED_ACTIVE }
}

static mut SCHED_ACTIVE: bool = false;

/// Wake expired sleepers (timer tick).
pub fn wake_sleepers(now: u64) {
    let mut s = sched().lock();
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
    for pid in ids {
        wake_locked(&mut s, pid);
    }
}

/// Called at end of trap handler with old TF ptr (VA).
/// Switches satp to next task. TF_VA stays same.
/// Invariant: queue holds waiting pids, current is separate.
pub fn schedule_point(_old_tf: *mut TrapFrame) {
    let next = {
        let mut s = sched().lock();
        let cur = s.current;
        // requeue current if still runnable
        if let Some(Some(p)) = s.procs.get_mut(cur) {
            if p.state == State::Running {
                p.state = State::Runnable;
                s.queue.push_back(cur);
            }
        }
        // pop next runnable
        let n = s.queue.len();
        let mut pick: Option<usize> = None;
        for _ in 0..n {
            if let Some(pid) = s.queue.pop_front() {
                let ok = match s.procs.get(pid) {
                    Some(Some(p)) => p.state == State::Runnable || p.state == State::Running,
                    _ => false,
                };
                if ok {
                    pick = Some(pid);
                    break;
                }
                // else drop zombie / dead
            }
        }
        match pick {
            Some(pid) => {
                if let Some(Some(p)) = s.procs.get_mut(pid) {
                    p.state = State::Running;
                }
                s.current = pid;
                Some(pid)
            }
            None => {
                // no other runnable; keep current if still alive (re-popped above?)
                // current was requeued above, but we popped it as pick if it was only one.
                // If pick is None, current must be dead/zombie.
                None
            }
        }
    };
    if let Some(pid) = next {
        let root = {
            let s = sched().lock();
            s.procs[pid].as_ref().unwrap().root
        };
        pt::activate(root);
    }
}

fn switch_to(next: Option<usize>) {
    match next {
        Some(pid) => {
            {
                let mut s = sched().lock();
                s.current = pid;
                if let Some(Some(p)) = s.procs.get_mut(pid) {
                    p.state = State::Running;
                }
            }
            let root = sched().lock().procs[pid].as_ref().unwrap().root;
            pt::activate(root);
            enter_user(pid);
        }
        None => {
            crate::println!("[PROC] no runnable task, shutdown");
            crate::println!("[TEST] ALL DONE");
            crate::sbi::shutdown();
            loop {}
        }
    }
}

pub fn run() -> ! {
    let first = {
        let mut s = sched().lock();
        let pid = s.queue.pop_front().expect("no task");
        s.current = pid;
        if let Some(Some(p)) = s.procs.get_mut(pid) {
            p.state = State::Running;
        }
        // do NOT push back: queue holds only waiting tasks, current is separate
        pid
    };
    let (root, tf_pa) = {
        let s = sched().lock();
        let p = s.procs[first].as_ref().unwrap();
        (p.root, p.tf_pa)
    };
    pt::activate(root);
    unsafe {
        SCHED_ACTIVE = true;
    }
    enter_user(first);
    unreachable!();
}

fn enter_user(_pid: usize) -> ! {
    unsafe {
        let cur = current_pid();
        let (tf_pa, sepc, sstatus) = {
            let s = sched().lock();
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
    ld x1, 1*8(t0)
    ld x3, 3*8(t0)
    ld x4, 4*8(t0)
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

// helpers for syscall layer
pub fn with_current<R>(f: impl FnOnce(&Proc) -> R) -> R {
    let s = sched().lock();
    let pid = s.current;
    let p: &Proc = s.procs[pid].as_ref().unwrap();
    f(p)
}
pub fn with_current_mut<R>(f: impl FnOnce(&mut Proc) -> R) -> R {
    let mut s = sched().lock();
    let pid = s.current;
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
        let mut s = sched().lock();
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
            s.procs[c] = None;
            if let Some(Some(pp)) = s.procs.get_mut(pid) {
                pp.children.retain(|&x| x != c);
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
    let s = sched().lock();
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
    let mut s = sched().lock();
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

/// Resolve user path against cwd: absolute stays, relative joins cwd;
/// normalizes `.`/`..`/`//`/trailing `/` (root stays `/`).
/// Output is always absolute; over-long (>127) returns root-relative clamp.
pub fn resolve_path(cwd: &str, path: &str) -> alloc::string::String {
    use alloc::string::String;
    use alloc::vec::Vec;
    let joined: String = if path.starts_with('/') {
        String::from(path)
    } else if cwd.ends_with('/') {
        alloc::format!("{}{}", cwd, path)
    } else {
        alloc::format!("{}/{}", cwd, path)
    };
    let mut parts: Vec<&str> = Vec::new();
    for comp in joined.split('/') {
        match comp {
            "" | "." => {}
            ".." => {
                parts.pop();
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
    let cwd = get_cwd(pid);
    resolve_path(&cwd, path)
}

// ---- v0.9: strace flag + ps snapshot ----
pub fn set_traced(pid: usize, on: bool) -> bool {
    let mut s = sched().lock();
    match s.procs.get_mut(pid).and_then(|o| o.as_mut()) {
        Some(p) => {
            p.traced = on;
            true
        }
        None => false,
    }
}

pub fn is_traced(pid: usize) -> bool {
    let s = sched().lock();
    matches!(s.procs.get(pid), Some(Some(p)) if p.traced)
}

/// One line per live proc: "pid ppid state brk cwd\n". state: R/B/Z.
pub fn ps_snapshot() -> alloc::string::String {
    let s = sched().lock();
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
