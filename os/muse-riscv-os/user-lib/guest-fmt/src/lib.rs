//! Allocation-free number formatting for muse-riscv-os guest programs.
//!
//! `core`-only, zero dependencies: usable from `#![no_std]` guest
//! binaries and from host tests alike. Collected from patterns
//! hand-rolled across `ctr`/`sh` (`dbg_num`, `push_dec`) so future
//! guest programs stop reinventing them.
//!
//! Every function writes into the caller's buffer and returns the new
//! length (append-style) or the written length (fresh-style), saturating
//! at the buffer end: they never panic and never wrap.
//!
//! (`cfg_attr` keeps `std` for the host test harness only; guest and
//! non-test builds stay pure `core`, so `std` creep fails fast.)

#![cfg_attr(not(test), no_std)]

/// decimal digits of `v` appended at `buf[n..]`; returns new length
/// (saturating at `buf.len()`).
pub fn push_dec(buf: &mut [u8], mut n: usize, mut v: usize) -> usize {
    let mut tmp = [0u8; 20];
    let mut m = 0usize;
    if v == 0 {
        tmp[0] = b'0';
        m = 1;
    } else {
        while v > 0 && m < 20 {
            tmp[m] = b'0' + (v % 10) as u8;
            v /= 10;
            m += 1;
        }
    }
    let mut i = m;
    while i > 0 && n < buf.len() {
        i -= 1;
        buf[n] = tmp[i];
        n += 1;
    }
    n
}

/// decimal digits of `v` written at `buf[..]`; returns length written
/// (saturating; 20 bytes hold any 64-bit value).
pub fn fmt_dec(buf: &mut [u8], v: usize) -> usize {
    push_dec(buf, 0, v)
}

/// lowercase hex of `v` appended at `buf[n..]`; returns new length.
pub fn push_hex(buf: &mut [u8], mut n: usize, mut v: usize) -> usize {
    let hb = b"0123456789abcdef";
    let mut tmp = [0u8; 16];
    let mut m = 0usize;
    if v == 0 {
        tmp[0] = b'0';
        m = 1;
    } else {
        while v > 0 && m < 16 {
            tmp[m] = hb[(v & 15) as usize];
            v >>= 4;
            m += 1;
        }
    }
    let mut i = m;
    while i > 0 && n < buf.len() {
        i -= 1;
        buf[n] = tmp[i];
        n += 1;
    }
    n
}

/// lowercase hex of `v` written at `buf[..]`; returns length written
/// (16 bytes hold any 64-bit value).
pub fn fmt_hex(buf: &mut [u8], v: usize) -> usize {
    push_hex(buf, 0, v)
}

/// octal digits of `v` appended at `buf[n..]`; returns new length.
pub fn push_oct(buf: &mut [u8], mut n: usize, mut v: usize) -> usize {
    let mut tmp = [0u8; 23];
    let mut m = 0usize;
    if v == 0 {
        tmp[0] = b'0';
        m = 1;
    } else {
        while v > 0 && m < 23 {
            tmp[m] = b'0' + (v & 7) as u8;
            v >>= 3;
            m += 1;
        }
    }
    let mut i = m;
    while i > 0 && n < buf.len() {
        i -= 1;
        buf[n] = tmp[i];
        n += 1;
    }
    n
}

/// octal digits of `v` written at `buf[..]`; returns length written
/// (23 bytes hold any 64-bit value).
pub fn fmt_oct(buf: &mut [u8], v: usize) -> usize {
    push_oct(buf, 0, v)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dec(v: usize) -> Vec<u8> {
        let mut b = [0u8; 20];
        let n = fmt_dec(&mut b, v);
        b[..n].to_vec()
    }

    #[test]
    fn dec_values() {
        assert_eq!(dec(0), b"0");
        assert_eq!(dec(9), b"9");
        assert_eq!(dec(10), b"10");
        assert_eq!(dec(12345), b"12345");
        assert_eq!(dec(100000), b"100000");
        assert_eq!(dec(usize::MAX), usize::MAX.to_string().as_bytes());
    }

    #[test]
    fn dec_saturates() {
        let mut b = [0u8; 3];
        let n = fmt_dec(&mut b, 12345);
        assert_eq!(n, 3);
        assert_eq!(&b, b"123");
        // push appends at the offset
        let mut b = [0u8; 8];
        b[0] = b'x';
        let n = push_dec(&mut b, 1, 42);
        assert_eq!(n, 3);
        assert_eq!(&b[..3], b"x42");
    }

    #[test]
    fn hex_values() {
        let mut b = [0u8; 16];
        let n = fmt_hex(&mut b, 0);
        assert_eq!(&b[..n], b"0");
        let n = fmt_hex(&mut b, 255);
        assert_eq!(&b[..n], b"ff");
        let n = fmt_hex(&mut b, 0xdeadbeef);
        assert_eq!(&b[..n], b"deadbeef");
        let n = fmt_hex(&mut b, usize::MAX);
        assert_eq!(n, 16);
        assert!(b.iter().all(|&c| c == b'f'));
    }

    #[test]
    fn oct_values() {
        let mut b = [0u8; 23];
        let n = fmt_oct(&mut b, 0);
        assert_eq!(&b[..n], b"0");
        let n = fmt_oct(&mut b, 8);
        assert_eq!(&b[..n], b"10");
        let n = fmt_oct(&mut b, 0o755);
        assert_eq!(&b[..n], b"755");
    }
}
