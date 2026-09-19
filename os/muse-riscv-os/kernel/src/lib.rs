//! Host-testable pure logic (also mirrored in main.rs for no_std).
//! SV39 PTE encode/decode + VPN split + simple scheduler queue.

#![cfg_attr(target_os = "none", no_std)]

#[cfg(target_os = "none")]
extern crate alloc;

#[cfg(target_os = "none")]
use alloc::vec::Vec;

pub const PTE_V: u64 = 1 << 0;
pub const PTE_R: u64 = 1 << 1;
pub const PTE_W: u64 = 1 << 2;
pub const PTE_X: u64 = 1 << 3;
pub const PTE_U: u64 = 1 << 4;

pub fn pte_new(ppn: u64, flags: u64) -> u64 {
    (ppn << 10) | (flags & 0x3ff)
}
pub fn pte_ppn(pte: u64) -> u64 {
    (pte >> 10) & 0xfff_ffff_ffff
}
pub fn pte_flags(pte: u64) -> u64 {
    pte & 0x3ff
}
pub fn vpn_split(va: u64) -> [u64; 3] {
    [(va >> 12) & 0x1ff, (va >> 21) & 0x1ff, (va >> 30) & 0x1ff]
}
pub fn satp_token(mode: u64, asid: u64, ppn: u64) -> u64 {
    (mode << 60) | (asid << 44) | ppn
}

#[derive(Default)]
pub struct RunQueue {
    q: Vec<usize>,
}
impl RunQueue {
    pub fn new() -> Self {
        Self { q: Vec::new() }
    }
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
    pub fn len(&self) -> usize {
        self.q.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pte_roundtrip() {
        let pte = pte_new(0x80000, PTE_V | PTE_R | PTE_W);
        assert_eq!(pte_ppn(pte), 0x80000);
        assert!(pte_flags(pte) & PTE_V != 0);
    }
    #[test]
    fn vpn_levels() {
        // 0x4000_1000 -> vpn0=1, vpn1=0, vpn2=1
        let v = vpn_split(0x4000_1000);
        assert_eq!(v[0], 1);
        assert_eq!(v[2], 1);
    }
    #[test]
    fn queue_rr() {
        let mut q = RunQueue::new();
        q.push(1);
        q.push(2);
        assert_eq!(q.pop(), Some(1));
        assert_eq!(q.pop(), Some(2));
        assert_eq!(q.pop(), None);
    }
    #[test]
    fn satp_sv39() {
        let s = satp_token(8, 0, 0x80000);
        assert_eq!(s >> 60, 8);
    }
}
