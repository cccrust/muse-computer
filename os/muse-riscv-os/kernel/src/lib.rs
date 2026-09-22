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

// v1.6: journal pure-logic mirror (mirrored in fs/jnl.rs for no_std; the
// crc binds (seq, lba, data) so torn slot mixes can never validate).
pub fn jnl_crc(seq: u32, lba: u32, data: &[u8; 512]) -> u32 {
    let mut crc: u32 = 0xffff_ffff;
    for &b in seq.to_le_bytes().iter().chain(lba.to_le_bytes().iter()).chain(data.iter()) {
        crc ^= b as u32;
        for _ in 0..8 {
            let m = if crc & 1 != 0 { 0xedb8_8320 } else { 0 };
            crc = (crc >> 1) ^ m;
        }
    }
    !crc
}
pub fn jnl_r32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}
pub fn jnl_w32(b: &mut [u8], o: usize, v: u32) {
    b[o..o + 4].copy_from_slice(&v.to_le_bytes());
}

#[derive(Default)]
pub struct RunQueue {
    q: Vec<usize>,
}impl RunQueue {
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
    #[test]
    fn jnl_crc_binds() {
        let d = [0xabu8; 512];
        let c0 = jnl_crc(7, 100, &d);
        let mut d2 = d;
        d2[0] ^= 1;
        assert_ne!(jnl_crc(7, 100, &d2), c0);
        assert_ne!(jnl_crc(8, 100, &d), c0);
        assert_ne!(jnl_crc(7, 101, &d), c0);
        assert_eq!(jnl_crc(7, 100, &d), c0);
    }
    #[test]
    fn jnl_record_roundtrip() {
        let mut a = [0u8; 512];
        let d = [0x42u8; 512];
        jnl_w32(&mut a, 0, 0x4a52_4e41);
        jnl_w32(&mut a, 8, 1234);
        jnl_w32(&mut a, 12, 777);
        jnl_w32(&mut a, 16, jnl_crc(1234, 777, &d));
        assert_eq!(jnl_r32(&a, 8), 1234);
        assert_eq!(jnl_crc(jnl_r32(&a, 8), jnl_r32(&a, 12), &d), jnl_r32(&a, 16));
        let mut torn = d;
        torn[511] ^= 0xff;
        assert_ne!(jnl_crc(1234, 777, &torn), jnl_r32(&a, 16));
    }
}
