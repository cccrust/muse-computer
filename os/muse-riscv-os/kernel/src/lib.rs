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
// v1.8: tx added to the bind; grouping rule mirrored in tests below.
pub fn jnl_crc(seq: u32, lba: u32, tx: u32, data: &[u8; 512]) -> u32 {
    let mut crc: u32 = 0xffff_ffff;
    for &b in seq
        .to_le_bytes()
        .iter()
        .chain(lba.to_le_bytes().iter())
        .chain(tx.to_le_bytes().iter())
        .chain(data.iter())
    {
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

// v1.7: DNS reply parsing mirror (mirrored in user-lib for no_std; answers
// use compression pointers, questions don't). Returns first A/IN rdata.
pub fn dns_skip_name(p: &[u8], mut o: usize) -> Option<usize> {
    let mut jumps = 0;
    loop {
        if o >= p.len() {
            return None;
        }
        let b = p[o];
        if b & 0xc0 == 0xc0 {
            if o + 1 >= p.len() {
                return None;
            }
            return Some(o + 2);
        }
        if b == 0 {
            return Some(o + 1);
        }
        if (b as usize) > 63 || o + 1 + (b as usize) > p.len() {
            return None;
        }
        o += 1 + (b as usize);
        jumps += 1;
        if jumps > 16 {
            return None;
        }
    }
}
pub fn dns_find_a(p: &[u8]) -> Option<u32> {
    if p.len() < 12 {
        return None;
    }
    if p[2] & 0x80 == 0 || p[3] & 0x0f != 0 {
        return None;
    }
    let qd = ((p[4] as usize) << 8) | p[5] as usize;
    let an = ((p[6] as usize) << 8) | p[7] as usize;
    if qd != 1 || an == 0 {
        return None;
    }
    let mut o = dns_skip_name(p, 12)? + 4; // question + QTYPE/QCLASS
    let mut ai = 0;
    while ai < an {
        o = dns_skip_name(p, o)?;
        if o + 10 > p.len() {
            return None;
        }
        let typ = ((p[o] as usize) << 8) | p[o + 1] as usize;
        let cls = ((p[o + 2] as usize) << 8) | p[o + 3] as usize;
        let rdlen = ((p[o + 8] as usize) << 8) | p[o + 9] as usize;
        o += 10;
        if o + rdlen > p.len() {
            return None;
        }
        if typ == 1 && cls == 1 && rdlen == 4 {
            return Some(
                ((p[o] as u32) << 24) | ((p[o + 1] as u32) << 16) | ((p[o + 2] as u32) << 8) | p[o + 3] as u32,
            );
        }
        o += rdlen;
        ai += 1;
        if ai > 16 {
            return None;
        }
    }
    None
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
        let c0 = jnl_crc(7, 100, 5, &d);
        let mut d2 = d;
        d2[0] ^= 1;
        assert_ne!(jnl_crc(7, 100, 5, &d2), c0);
        assert_ne!(jnl_crc(8, 100, 5, &d), c0);
        assert_ne!(jnl_crc(7, 101, 5, &d), c0);
        assert_ne!(jnl_crc(7, 100, 6, &d), c0);
        assert_eq!(jnl_crc(7, 100, 5, &d), c0);
    }
    #[test]
    fn jnl_record_roundtrip() {
        let mut a = [0u8; 512];
        let d = [0x42u8; 512];
        jnl_w32(&mut a, 0, 0x4a52_4e41);
        jnl_w32(&mut a, 8, 1234);
        jnl_w32(&mut a, 12, 777);
        jnl_w32(&mut a, 20, 5);
        jnl_w32(&mut a, 16, jnl_crc(1234, 777, 5, &d));
        assert_eq!(jnl_r32(&a, 8), 1234);
        assert_eq!(jnl_crc(jnl_r32(&a, 8), jnl_r32(&a, 12), jnl_r32(&a, 20), &d), jnl_r32(&a, 16));
        let mut torn = d;
        torn[511] ^= 0xff;
        assert_ne!(jnl_crc(1234, 777, 5, &torn), jnl_r32(&a, 16));
    }
    #[test]
    fn jnl_tx_grouping() {
        // grouping rule mirror: legacy (tx=0) applies; a TX applies whole
        // (commit + exact member count) or not at all.
        struct Rec {
            tx: u32,
            committed: bool,
            count: u32,
            members: u32,
        }
        fn applies(r: &Rec) -> bool {
            if r.tx == 0 {
                return true;
            }
            r.committed && r.members == r.count
        }
        assert!(applies(&Rec { tx: 0, committed: false, count: 0, members: 0 }));
        assert!(applies(&Rec { tx: 9, committed: true, count: 3, members: 3 }));
        assert!(!applies(&Rec { tx: 9, committed: false, count: 0, members: 3 }));
        assert!(!applies(&Rec { tx: 9, committed: true, count: 3, members: 2 }));
        assert!(!applies(&Rec { tx: 9, committed: true, count: 2, members: 3 }));
    }
    #[test]
    fn dns_compressed_answer() {
        // response for "h.example": question + answer with pointer to qname
        let mut p = [0u8; 64];
        // header: response, NOERROR, qd=1, an=1
        p[2] = 0x80;
        p[5] = 1;
        p[7] = 1;
        // question: 1[h]7[example]0, QTYPE=A, QCLASS=IN
        let mut o = 12;
        p[o] = 1;
        o += 1;
        p[o] = b'h';
        o += 1;
        p[o] = 7;
        o += 1;
        p[o..o + 7].copy_from_slice(b"example");
        o += 7;
        p[o] = 0;
        o += 1;
        p[o] = 0;
        p[o + 1] = 1;
        p[o + 2] = 0;
        p[o + 3] = 1;
        o += 4;
        // answer: pointer to offset 12, A IN, ttl, rdlen=4, 10.9.8.7
        p[o] = 0xc0;
        p[o + 1] = 12;
        p[o + 2] = 0;
        p[o + 3] = 1;
        p[o + 4] = 0;
        p[o + 5] = 1;
        p[o + 10] = 0;
        p[o + 11] = 4;
        p[o + 12] = 10;
        p[o + 13] = 9;
        p[o + 14] = 8;
        p[o + 15] = 7;
        o += 16;
        assert_eq!(dns_find_a(&p[..o]), Some(0x0a09_0807));
    }
    #[test]
    fn dns_cname_then_a() {
        // CNAME first (skipped), A second (taken)
        let mut p = [0u8; 96];
        p[2] = 0x80;
        p[5] = 1;
        p[7] = 2;
        // question: 1[x]0 + QTYPE/QCLASS
        let mut o = 12;
        p[o] = 1;
        o += 1;
        p[o] = b'x';
        o += 1;
        p[o] = 0;
        o += 1;
        p[o] = 0;
        p[o + 1] = 1;
        p[o + 2] = 0;
        p[o + 3] = 1;
        o += 4;
        // answer 1: pointer, CNAME, rdlen=2 -> pointer (alias, skipped)
        p[o] = 0xc0;
        p[o + 1] = 12;
        p[o + 2] = 0;
        p[o + 3] = 5;
        p[o + 4] = 0;
        p[o + 5] = 1;
        p[o + 10] = 0;
        p[o + 11] = 2;
        p[o + 12] = 0xc0;
        p[o + 13] = 12;
        o += 14;
        // answer 2: pointer, A, 1.2.3.4
        p[o] = 0xc0;
        p[o + 1] = 12;
        p[o + 2] = 0;
        p[o + 3] = 1;
        p[o + 4] = 0;
        p[o + 5] = 1;
        p[o + 10] = 0;
        p[o + 11] = 4;
        p[o + 12] = 1;
        p[o + 13] = 2;
        p[o + 14] = 3;
        p[o + 15] = 4;
        o += 16;
        assert_eq!(dns_find_a(&p[..o]), Some(0x0102_0304));
        // garbage is rejected
        assert_eq!(dns_find_a(&p[..10]), None);
        let mut bad = [0u8; 64];
        bad[2] = 0x80;
        bad[3] = 0x03; // NXDOMAIN
        bad[5] = 1;
        assert_eq!(dns_find_a(&bad), None);
    }
}
