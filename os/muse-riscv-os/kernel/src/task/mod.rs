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
    Zombie,
}

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
        name: nb,
    };
    s.procs[pid] = Some(proc);
    if parent != 0 {
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
    p -= 8 * (argc + 1);
    let argv_base = p;
    for i in 0..argc {
        write_u64_to(root, argv_base + i * 8, addrs[i] as u64);
    }
    write_u64_to(root, argv_base + argc * 8, 0);
    p -= 8;
    write_u64_to(root, p, argc as u64);
    p & !15usize
}

/// Fork current process. Returns child pid. Caller sets child a0=0.
pub fn fork(parent_pid: usize) -> usize {
    // gather parent info without holding lock across allocs
    let (p_root, p_tf, p_brk, p_kind, p_fds, p_off, p_path, pname) = {
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
        name: nb,
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
        p.brk = brk.max(0x20000);
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

/// Mark target as killed; it exits with -9 on next trap.
pub fn kill(pid: usize) -> bool {
    let mut s = sched().lock();
    match s.procs.get_mut(pid) {
        Some(Some(p)) => {
            if p.state == State::Zombie {
                return false;
            }
            p.killed = true;
            true
        }
        _ => false,
    }
}

pub fn is_killed(pid: usize) -> bool {
    let s = sched().lock();
    matches!(s.procs.get(pid), Some(Some(p)) if p.killed)
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
    fn clone_proc_info(&self) -> (usize, usize, usize, [u8; 16], [i32; 16], [usize; 16], [u64; 16], [u8; 32]);
}
impl CloneInfo for Proc {
    fn clone_proc_info(&self) -> (usize, usize, usize, [u8; 16], [i32; 16], [usize; 16], [u64; 16], [u8; 32]) {
        (self.root, self.tf_pa, self.brk, self.fd_kind, self.fds, self.fd_off, self.fd_path, self.name)
    }
}

pub fn wait(pid: usize) -> (i32, usize) {
    // returns (found, child_pid/code). Blocking poll with yield.
    loop {
        let child = {
            let mut s = sched().lock();
            let mut found: Option<(usize, i32)> = None;
            let children = s.procs[pid].as_ref().unwrap().children.clone();
            for c in children {
                if let Some(Some(p)) = s.procs.get(c) {
                    if p.state == State::Zombie {
                        found = Some((c, p.exit_code));
                        break;
                    }
                }
            }
            if let Some((c, code)) = found {
                s.procs[c] = None;
                // remove from children list
                if let Some(Some(pp)) = s.procs.get_mut(pid) {
                    pp.children.retain(|&x| x != c);
                }
                Some((c, code))
            } else {
                // any non-zombie children?
                let any = s.procs[pid]
                    .as_ref()
                    .unwrap()
                    .children
                    .iter()
                    .any(|&c| s.procs.get(c).and_then(|o| o.as_ref()).is_some());
                if !any {
                    Some((0, -1))
                } else {
                    None
                }
            }
        };
        if let Some((c, code)) = child {
            if c == 0 {
                return (-1, 0);
            }
            return (c as i32, code as usize);
        }
        // no zombie yet: yield and return WouldBlock; user retries.
        // Do NOT call schedule_point here (would switch satp and corrupt TF).
        // Outer trap handler will switch after we return.
        yield_now();
        return (-2, 0);
    }
}
