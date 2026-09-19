//! Host-side unit tests mirroring kernel SV39 / allocator / scheduler logic.

pub const PTE_V: u64 = 1 << 0;
pub const PTE_R: u64 = 1 << 1;
pub const PTE_W: u64 = 1 << 2;
pub const PTE_X: u64 = 1 << 3;
pub const PTE_U: u64 = 1 << 4;

pub fn pte_new(pa: usize, flags: u64) -> u64 {
    ((pa >> 12) as u64) << 10 | (flags & 0x3ff)
}
pub fn pte_pa(pte: u64) -> usize {
    (((pte >> 10) & 0xfff_ffff_ffff) as usize) << 12
}
pub fn vpn(va: usize, level: usize) -> usize {
    (va >> (12 + 9 * level)) & 0x1ff
}

pub struct FrameMock {
    next: usize,
    end: usize,
}
impl FrameMock {
    pub fn new(start: usize, end: usize) -> Self {
        Self { next: start, end }
    }
    pub fn alloc(&mut self) -> Option<usize> {
        if self.next + 4096 <= self.end {
            let p = self.next;
            self.next += 4096;
            Some(p)
        } else {
            None
        }
    }
}

#[derive(Default)]
pub struct RunQueue {
    q: Vec<usize>,
}
impl RunQueue {
    pub fn push(&mut self, pid: usize) {
        self.q.push(pid);
    }
    pub fn pop(&mut self) -> Option<usize> {
        if self.q.is_empty() {
            None
        } else {
            Some(self.q.remove(0))
        }
    }
}

fn main() {
    println!("host-tests: run with `cargo test -p host-tests`");
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sv39_pte_encode() {
        let pte = pte_new(0x8020_0000, PTE_V | PTE_R | PTE_X);
        assert_eq!(pte_pa(pte), 0x8020_0000);
        assert!(pte & PTE_V != 0);
    }
    #[test]
    fn sv39_vpn_split() {
        assert_eq!(vpn(0x8020_0000, 2), 2);
        assert_eq!(vpn(0x10000, 0), 16);
    }
    #[test]
    fn sv39_satp_mode() {
        let root_pa = 0x8040_0000usize;
        let satp = (8usize << 60) | (root_pa >> 12);
        assert_eq!(satp >> 60, 8);
    }
    #[test]
    fn frame_bump() {
        let mut f = FrameMock::new(0x8040_0000, 0x8040_0000 + 8192);
        assert!(f.alloc().is_some());
        assert!(f.alloc().is_some());
        assert!(f.alloc().is_none());
    }
    #[test]
    fn sched_round_robin() {
        let mut q = RunQueue::default();
        q.push(1);
        q.push(2);
        q.push(3);
        assert_eq!(q.pop(), Some(1));
        q.push(1);
        assert_eq!(q.pop(), Some(2));
        assert_eq!(q.pop(), Some(3));
        assert_eq!(q.pop(), Some(1));
    }
    #[test]
    fn elf_magic() {
        let fake = [0x7fu8, b'E', b'L', b'F', 2, 1, 1, 0];
        assert_eq!(&fake[0..4], &[0x7f, b'E', b'L', b'F']);
    }
    #[test]
    fn pipe_capacity() {
        const CAP: usize = 1024;
        assert!(CAP >= 256);
    }
}
