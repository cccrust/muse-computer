#![cfg_attr(not(test), no_std)]

// v3.14: minimal TLS 1.3 client crypto (single suite
// TLS_AES_128_GCM_SHA256 + X25519 + ECDSA-P256-SHA256).
// core-only, zero deps, no alloc: callers pass fixed buffers.
// This slice: SHA-256 (mirrored from user-lib, v3.4) + HMAC-SHA256
// (RFC 4231) + HKDF-SHA256 (RFC 5869). AES-GCM / X25519 / P-256 follow.

pub mod aes128;
pub mod gcm;
pub mod n256;
pub mod p256;
pub mod tls13;
pub mod x509;
pub mod x25519;

#[cfg(test)]
extern crate std;

// ---- SHA-256 (FIPS 180-4; mirror of user-lib Sha256) ----

const SHA_K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

#[derive(Clone)]
pub struct Sha256 {
    h: [u32; 8],
    len: u64,
    buf: [u8; 64],
    bufn: usize,
}

impl Sha256 {
    pub fn new() -> Self {
        Self {
            h: [
                0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
                0x5be0cd19,
            ],
            len: 0,
            buf: [0u8; 64],
            bufn: 0,
        }
    }

    fn block(&mut self, b: &[u8; 64]) {
        let mut w = [0u32; 64];
        let mut i = 0;
        while i < 16 {
            w[i] = ((b[i * 4] as u32) << 24)
                | ((b[i * 4 + 1] as u32) << 16)
                | ((b[i * 4 + 2] as u32) << 8)
                | (b[i * 4 + 3] as u32);
            i += 1;
        }
        while i < 64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
            i += 1;
        }
        let (mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh) =
            (self.h[0], self.h[1], self.h[2], self.h[3], self.h[4], self.h[5], self.h[6], self.h[7]);
        i = 0;
        while i < 64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(SHA_K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
            i += 1;
        }
        self.h[0] = self.h[0].wrapping_add(a);
        self.h[1] = self.h[1].wrapping_add(b);
        self.h[2] = self.h[2].wrapping_add(c);
        self.h[3] = self.h[3].wrapping_add(d);
        self.h[4] = self.h[4].wrapping_add(e);
        self.h[5] = self.h[5].wrapping_add(f);
        self.h[6] = self.h[6].wrapping_add(g);
        self.h[7] = self.h[7].wrapping_add(hh);
    }

    pub fn update(&mut self, mut data: &[u8]) {
        self.len = self.len.wrapping_add(data.len() as u64);
        if self.bufn > 0 {
            let take = (64 - self.bufn).min(data.len());
            self.buf[self.bufn..self.bufn + take].copy_from_slice(&data[..take]);
            self.bufn += take;
            data = &data[take..];
            if self.bufn == 64 {
                let mut b = [0u8; 64];
                b.copy_from_slice(&self.buf);
                self.block(&b);
                self.bufn = 0;
            }
        }
        while data.len() >= 64 {
            let mut b = [0u8; 64];
            b.copy_from_slice(&data[..64]);
            self.block(&b);
            data = &data[64..];
        }
        if !data.is_empty() {
            self.buf[..data.len()].copy_from_slice(data);
            self.bufn = data.len();
        }
    }

    pub fn finalize(mut self) -> [u8; 32] {
        let bitlen = self.len.wrapping_mul(8);
        self.update(&[0x80]);
        while self.bufn != 56 {
            if self.bufn > 56 {
                while self.bufn < 64 {
                    self.buf[self.bufn] = 0;
                    self.bufn += 1;
                }
                let mut b = [0u8; 64];
                b.copy_from_slice(&self.buf);
                self.block(&b);
                self.bufn = 0;
            } else {
                self.buf[self.bufn] = 0;
                self.bufn += 1;
            }
        }
        let mut tail = [0u8; 8];
        let mut i = 0;
        while i < 8 {
            tail[i] = (bitlen >> (56 - i * 8)) as u8;
            i += 1;
        }
        self.buf[56..64].copy_from_slice(&tail);
        let mut b = [0u8; 64];
        b.copy_from_slice(&self.buf);
        self.block(&b);
        let mut out = [0u8; 32];
        i = 0;
        while i < 8 {
            out[i * 4] = (self.h[i] >> 24) as u8;
            out[i * 4 + 1] = (self.h[i] >> 16) as u8;
            out[i * 4 + 2] = (self.h[i] >> 8) as u8;
            out[i * 4 + 3] = self.h[i] as u8;
            i += 1;
        }
        out
    }
}

pub fn sha256(data: &[u8]) -> [u8; 32] {
    let mut st = Sha256::new();
    st.update(data);
    st.finalize()
}

// ---- HMAC-SHA256 (RFC 4231) ----

/// One-shot HMAC-SHA256. `msg` arrives as one slice (TLS handshake
/// transcripts fit the caller's static buffer; no streaming handle).
pub fn hmac_sha256(key: &[u8], msg: &[u8], out: &mut [u8; 32]) {
    let mut kb = [0u8; 64];
    if key.len() > 64 {
        kb[..32].copy_from_slice(&sha256(key));
    } else {
        kb[..key.len()].copy_from_slice(key);
    }
    let mut ip = [0x36u8; 64];
    let mut op = [0x5cu8; 64];
    let mut i = 0;
    while i < 64 {
        ip[i] ^= kb[i];
        op[i] ^= kb[i];
        i += 1;
    }
    let mut st = Sha256::new();
    st.update(&ip);
    st.update(msg);
    let inner = st.finalize();
    let mut st2 = Sha256::new();
    st2.update(&op);
    st2.update(&inner);
    *out = st2.finalize();
}

// ---- HKDF-SHA256 (RFC 5869) ----

/// Extract: PRK = HMAC(salt, IKM). Empty salt = 32 zero bytes.
pub fn hkdf_extract(salt: &[u8], ikm: &[u8], prk: &mut [u8; 32]) {
    if salt.is_empty() {
        let z = [0u8; 32];
        hmac_sha256(&z, ikm, prk);
    } else {
        hmac_sha256(salt, ikm, prk);
    }
}

/// Expand: OKM = T(1) | T(2) | ... truncated to out.len().
/// Returns false when out is empty or exceeds 255*32 bytes.
pub fn hkdf_expand(prk: &[u8; 32], info: &[u8], out: &mut [u8]) -> bool {
    if out.is_empty() || out.len() > 255 * 32 {
        return false;
    }
    let n = (out.len() + 31) / 32;
    let mut prev = [0u8; 32];
    let mut prev_len = 0usize;
    let mut done = 0usize;
    let mut ctr = 1u8;
    while ctr as usize <= n {
        // T(ctr) = HMAC(prk, T(prev) | info | ctr)
        // (T(0) is empty; prev buffer reused.)
        let mut st_ip = [0x36u8; 64];
        let mut st_op = [0x5cu8; 64];
        let mut kb = [0u8; 64];
        kb[..32].copy_from_slice(prk);
        let mut i = 0;
        while i < 64 {
            st_ip[i] ^= kb[i];
            st_op[i] ^= kb[i];
            i += 1;
        }
        let mut inner = Sha256::new();
        inner.update(&st_ip);
        if prev_len > 0 {
            inner.update(&prev[..prev_len]);
        }
        inner.update(info);
        inner.update(&[ctr]);
        let t_inner = inner.finalize();
        let mut outer = Sha256::new();
        outer.update(&st_op);
        outer.update(&t_inner);
        let t = outer.finalize();
        let take = (out.len() - done).min(32);
        out[done..done + take].copy_from_slice(&t[..take]);
        prev.copy_from_slice(&t);
        prev_len = 32;
        done += take;
        ctr += 1;
    }
    // wipe the chaining copy (key material hygiene; cheap)
    let mut i = 0;
    while i < 32 {
        prev[i] = 0;
        i += 1;
    }
    let _ = prev_len;
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> std::vec::Vec<u8> {
        let b = s.as_bytes();
        let mut v = std::vec::Vec::with_capacity(b.len() / 2);
        let mut i = 0;
        while i < b.len() {
            let h = |c: u8| match c {
                b'0'..=b'9' => c - b'0',
                b'a'..=b'f' => c - b'a' + 10,
                b'A'..=b'F' => c - b'A' + 10,
                _ => 0,
            };
            v.push((h(b[i]) << 4) | h(b[i + 1]));
            i += 2;
        }
        v
    }

    #[test]
    fn hmac_rfc4231_case1() {
        let key = [0x0bu8; 20];
        let mut out = [0u8; 32];
        hmac_sha256(&key, b"Hi There", &mut out);
        assert_eq!(
            out,
            hex("b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7").as_slice()
        );
    }

    #[test]
    fn hmac_rfc4231_case2() {
        let mut out = [0u8; 32];
        hmac_sha256(b"Jefe", b"what do ya want for nothing?", &mut out);
        assert_eq!(
            out,
            hex("5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843").as_slice()
        );
    }

    #[test]
    fn hkdf_rfc5869_case1() {
        // IKM = 22 x 0x0b, salt = 000102..0c, info = f0..f9, L = 42.
        let ikm = [0x0bu8; 22];
        let salt = hex("000102030405060708090a0b0c");
        let info = hex("f0f1f2f3f4f5f6f7f8f9");
        let mut prk = [0u8; 32];
        hkdf_extract(&salt, &ikm, &mut prk);
        assert_eq!(
            prk,
            hex("077709362c2e32df0ddc3f0dc47bba6390b6c73bb50f9c3122ec844ad7c2b3e5").as_slice()
        );
        let mut okm = [0u8; 42];
        assert!(hkdf_expand(&prk, &info, &mut okm));
        assert_eq!(
            okm,
            hex("3cb25f25faacd57a90434f64d0362f2a2d2d0a90cf1a5a4c5db02d56ecc4c5bf34007208d5b887185865")
                .as_slice()
        );
    }

    #[test]
    fn hkdf_rfc5869_case3_empty() {
        // IKM = 22 x 0x0b, empty salt/info, L = 42.
        let ikm = [0x0bu8; 22];
        let mut prk = [0u8; 32];
        hkdf_extract(&[], &ikm, &mut prk);
        assert_eq!(
            prk,
            hex("19ef24a32c717b167f33a91d6f648bdf96596776afdb6377ac434c1c293ccb04").as_slice()
        );
        let mut okm = [0u8; 42];
        assert!(hkdf_expand(&prk, &[], &mut okm));
        assert_eq!(
            okm,
            hex("8da4e775a563c18f715f802a063c5a31b8a11f5c5ee1879ec3454e5f3c738d2d9d201395faa4b61a96c8")
                .as_slice()
        );
    }

    #[test]
    fn hkdf_expand_rejects_bounds() {
        let prk = [0u8; 32];
        let mut empty = [0u8; 0];
        assert!(!hkdf_expand(&prk, b"x", &mut empty));
        let mut big = std::vec![0u8; 255 * 32 + 1];
        assert!(!hkdf_expand(&prk, b"x", &mut big));
    }
}
