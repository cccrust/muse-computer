// v3.14: X25519 (RFC 7748 §5). Field GF(2^255-19) in 4x64-bit limbs;
// u128 is used for multiply-accumulate only (single mulhu/mul on riscv64,
// no u128 div/mod anywhere, so no compiler-rt libcalls in no_std).
//
// Test-grade timing honesty: the ladder itself is swap-constant-time, but
// field add/sub carry chains are not formally constant-time (same honesty
// as SYS_GETRANDOM's test-grade label).

const MASK64: u128 = 0xffff_ffff_ffff_ffff;
/// p = 2^255 - 19, little-endian limbs.
const P: [u64; 4] = [
    0xffff_ffff_ffff_ffed,
    0xffff_ffff_ffff_ffff,
    0xffff_ffff_ffff_ffff,
    0x7fff_ffff_ffff_ffff,
];

/// Field element, always < p after every op.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Fe([u64; 4]);

impl Fe {
    fn zero() -> Self {
        Self([0, 0, 0, 0])
    }
    fn one() -> Self {
        Self([1, 0, 0, 0])
    }
    /// Load 32 LE bytes, reduced mod p (input < 2^256 < 2p + 37).
    fn from_bytes(b: &[u8; 32]) -> Self {
        let mut l = [0u64; 4];
        let mut i = 0;
        while i < 4 {
            let mut j = 0;
            while j < 8 {
                l[i] |= (b[i * 8 + j] as u64) << (j * 8);
                j += 1;
            }
            i += 1;
        }
        let mut f = Self(l);
        f.cond_sub();
        f.cond_sub();
        f
    }
    fn to_bytes(&self) -> [u8; 32] {
        let mut out = [0u8; 32];
        let mut i = 0;
        while i < 4 {
            let mut j = 0;
            while j < 8 {
                out[i * 8 + j] = (self.0[i] >> (j * 8)) as u8;
                j += 1;
            }
            i += 1;
        }
        out
    }
    /// self -= p if self >= p (branchless).
    fn cond_sub(&mut self) {
        let mut b: u64 = 0;
        let mut d = [0u64; 4];
        let mut i = 0;
        while i < 4 {
            let (r1, o1) = self.0[i].overflowing_sub(P[i]);
            let (r2, o2) = r1.overflowing_sub(b);
            d[i] = r2;
            b = (o1 || o2) as u64;
            i += 1;
        }
        // borrow=0 (self>=p) -> take d; borrow=1 -> keep self (branchless).
        let m = 0u64.wrapping_sub(b ^ 1);
        i = 0;
        while i < 4 {
            self.0[i] = (self.0[i] & !m) | (d[i] & m);
            i += 1;
        }
    }
    fn add(&self, o: &Self) -> Self {
        let mut l = [0u64; 4];
        let mut c = 0u64;
        let mut i = 0;
        while i < 4 {
            let s = self.0[i] as u128 + o.0[i] as u128 + c as u128;
            l[i] = s as u64;
            c = (s >> 64) as u64;
            i += 1;
        }
        // sum < 2p < 2^256: bit-256 carry is always 0, one cond_sub suffices
        // (the second call below is harmless belt-and-braces).
        let mut f = Self(l);
        f.cond_sub();
        f.cond_sub();
        f
    }
    fn sub(&self, o: &Self) -> Self {
        let mut d = [0u64; 4];
        let mut b = 0u64;
        let mut i = 0;
        while i < 4 {
            let (r1, o1) = self.0[i].overflowing_sub(o.0[i]);
            let (r2, o2) = r1.overflowing_sub(b);
            d[i] = r2;
            b = (o1 || o2) as u64;
            i += 1;
        }
        // borrow=1 -> add p back (branchless).
        let m = 0u64.wrapping_sub(b);
        let mut c = 0u64;
        i = 0;
        while i < 4 {
            let s = d[i] as u128 + (P[i] & m) as u128 + c as u128;
            d[i] = s as u64;
            c = (s >> 64) as u64;
            i += 1;
        }
        Self(d)
    }
    fn mul(&self, o: &Self) -> Self {
        // Schoolbook product into u64 limbs. A single product always fits
        // u128, but an accumulator does NOT (t[1] = a0*b1 + a1*b0 can reach
        // 2^129), so each product is split (lo -> current limb, hi -> carry)
        // and every running sum stays < 3*2^64 (u128, never overflows).
        let mut t = [0u64; 9];
        let mut i = 0;
        while i < 4 {
            let mut carry: u128 = 0; // < 2^64, by induction (see below)
            let mut j = 0;
            while j < 4 {
                let p = self.0[i] as u128 * o.0[j] as u128; // < 2^128
                let cur = t[i + j] as u128 + (p & MASK64) + carry; // < 3*2^64
                t[i + j] = cur as u64;
                carry = (cur >> 64) + (p >> 64); // <= 2 + (2^64-2) < 2^64
                j += 1;
            }
            // drain carry upward (touches at most slots i+4, i+5).
            let mut k = i + 4;
            let mut c = carry;
            while c != 0 {
                let cur = t[k] as u128 + c;
                t[k] = cur as u64;
                c = cur >> 64;
                k += 1;
            }
            i += 1;
        }
        // Fold 2^256 == 38 (and 2^512 == 38^2 == 1444 for slot 8).
        // Result < 2^256 + 38*2^256 + 1444*2^64 < 2^262.
        let mut lo = [0u128; 5];
        let mut j = 0;
        while j < 4 {
            lo[j] = t[j] as u128 + 38 * t[j + 4] as u128; // < 2^70
            j += 1;
        }
        lo[0] += 1444 * t[8] as u128; // < 2^75
        // Normalize exactly. Each round chains carries upward and folds
        // slot 4 back (*38). Rounds needed (value strictly shrinks):
        // r1: value < 2^262 -> slot4 < 2^6  -> fold -> value < 2^256 + 2432;
        // r2: slot4 <= 1 -> fold -> value < 2470;
        // r3: slot4 == 0 -> break. The 4th round is unused margin.
        let mut r = 0;
        while r < 4 {
            i = 0;
            while i < 4 {
                let c = lo[i] >> 64;
                lo[i] &= MASK64;
                lo[i + 1] += c;
                i += 1;
            }
            if lo[4] == 0 {
                break;
            }
            lo[0] += 38 * lo[4];
            lo[4] = 0;
            r += 1;
        }
        // Every limb is now < 2^64 (value < 2^256). Reduce mod p (not mod
        // 2^256!): if bit 255 is set, value = lo - 2^255 + 19, because
        // 2^255 == 19 (mod p). (Forgetting the +19 was a real bug: small
        // products passed, anything with bit 255 set came out 19 short.)
        let mut f = Self([lo[0] as u64, lo[1] as u64, lo[2] as u64, lo[3] as u64]);
        let top = (lo[3] >> 63) as u64; // 0 or 1
        f.0[3] = f.0[3].wrapping_sub(top << 63);
        let (r0, mut c) = f.0[0].overflowing_add(19 * top);
        f.0[0] = r0;
        let mut k = 1;
        while k < 4 && c {
            let (rk, cc) = f.0[k].overflowing_add(1);
            f.0[k] = rk;
            c = cc;
            k += 1;
        }
        // value < 2^255 + 38 < 2^256: no bit-256 carry is possible here.
        debug_assert!(!c);
        f.cond_sub();
        f
    }
    fn sq(&self) -> Self {
        self.mul(self)
    }
    /// Inverse via binary square-and-multiply, e = p - 2
    /// (255-bit exponent; 254 squares + popcount mults; no chain to get wrong).
    fn inv(&self) -> Self {
        // e = 2^255 - 21: byte0 = 0xeb, bytes1..30 = 0xff, byte31 = 0x7f.
        let mut e = [0xffu8; 32];
        e[0] = 0xeb;
        e[31] = 0x7f;
        let mut r = Self::one();
        let mut i = 254i32;
        while i >= 0 {
            r = r.sq();
            if ((e[(i as usize) / 8] >> ((i as usize) % 8)) & 1) != 0 {
                r = r.mul(self);
            }
            i -= 1;
        }
        r
    }
    /// Branchless cswap (swap=0/1).
    fn cswap(swap: u8, a: &mut Self, b: &mut Self) {
        let m = 0u64.wrapping_sub(swap as u64);
        let mut i = 0;
        while i < 4 {
            let t = m & (a.0[i] ^ b.0[i]);
            a.0[i] ^= t;
            b.0[i] ^= t;
            i += 1;
        }
    }
}

/// X25519(secret, u): both 32 LE bytes; scalar is clamped per RFC 7748.
/// The top bit of u is masked on receipt (RFC 7748 §5); the output always
/// has it clear (value < p < 2^255).
pub fn x25519(secret: &[u8; 32], u: &[u8; 32]) -> [u8; 32] {
    let mut s = *secret;
    s[0] &= 248;
    s[31] &= 127;
    s[31] |= 64;
    let mut ub = *u;
    ub[31] &= 127;
    let x1 = Fe::from_bytes(&ub);
    let mut x2 = Fe::one();
    let mut z2 = Fe::zero();
    let mut x3 = x1;
    let mut z3 = Fe::one();
    let mut swap: u8 = 0;
    let mut t = 254i32;
    while t >= 0 {
        let kt = ((s[(t as usize) / 8] >> ((t as usize) % 8)) & 1) as u8;
        let c = swap ^ kt;
        Fe::cswap(c, &mut x2, &mut x3);
        Fe::cswap(c, &mut z2, &mut z3);
        swap = kt;
        let a = x2.add(&z2);
        let aa = a.sq();
        let b = x2.sub(&z2);
        let bb = b.sq();
        let e = aa.sub(&bb);
        let c3 = x3.add(&z3);
        let d = x3.sub(&z3);
        let da = d.mul(&a);
        let cb = c3.mul(&b);
        let t1 = da.add(&cb);
        x3 = t1.sq();
        let t2 = da.sub(&cb);
        let t2s = t2.sq();
        z3 = x1.mul(&t2s);
        x2 = aa.mul(&bb);
        // z2 = E * (AA + a24 * E), a24 = (486662 - 2) / 4 = 121665 (RFC 7748 §5).
        let c121665 = Fe([121665, 0, 0, 0]);
        let ke = c121665.mul(&e);
        let ape = aa.add(&ke);
        z2 = e.mul(&ape);
        t -= 1;
    }
    Fe::cswap(swap, &mut x2, &mut x3);
    Fe::cswap(swap, &mut z2, &mut z3);
    x2.mul(&z2.inv()).to_bytes()
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

    fn b32(s: &str) -> [u8; 32] {
        let v = hex(s);
        let mut o = [0u8; 32];
        o.copy_from_slice(&v);
        o
    }

    /// RFC 7748 §5.2-style base vector (node/OpenSSL ground truth).
    #[test]
    fn base_vector() {
        let s = b32("0900000000000000000000000000000000000000000000000000000000000000");
        let got = x25519(&s, &s);
        assert_eq!(
            got,
            b32("422c8e7a6227d7bca1350b3e2bb7279f7897b87bb6854b783c60e80311ae3079")
        );
    }

    #[test]
    fn fixed_keypair_a() {
        let ka = b32("a046a9b2fb9e3e3f8e6e6e3e1111111111111111111111111111111111111111");
        let g = b32("0900000000000000000000000000000000000000000000000000000000000000");
        assert_eq!(
            x25519(&ka, &g),
            b32("95aa87e63baa504abff5d983350b78fb2fd28c49cb980aac78714b9c26ad6d22")
        );
    }

    #[test]
    fn fixed_keypair_b() {
        let kb = b32("b046a9b2fb9e3e3f8e6e6e3e2222222222222222222222222222222222222222");
        let g = b32("0900000000000000000000000000000000000000000000000000000000000000");
        assert_eq!(
            x25519(&kb, &g),
            b32("a8b955b9179aaf7957f49661100eb55316ad374461a56daa39c199738cc9c653")
        );
    }

    /// DH agreement, both directions (node: Sab == Sba).
    #[test]
    fn dh_agreement() {
        let ka = b32("a046a9b2fb9e3e3f8e6e6e3e1111111111111111111111111111111111111111");
        let kb = b32("b046a9b2fb9e3e3f8e6e6e3e2222222222222222222222222222222222222222");
        let pa = b32("95aa87e63baa504abff5d983350b78fb2fd28c49cb980aac78714b9c26ad6d22");
        let pb = b32("a8b955b9179aaf7957f49661100eb55316ad374461a56daa39c199738cc9c653");
        let want = b32("664a95ee6b0e4ef1e72f8d674f384c36a1f2791baed641be11f28415641a9427");
        assert_eq!(x25519(&ka, &pb), want);
        assert_eq!(x25519(&kb, &pa), want);
    }

    /// RFC 7748 §6.1 official ECDH vector (authoritative, no node needed).
    #[test]
    fn rfc_section6_vector() {
        let a = b32("77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a");
        let b = b32("5dab087e624a8a4b79e17f8b83800ee66f3bb1292618b6fd1c2f8b27ff88e0eb");
        let g = b32("0900000000000000000000000000000000000000000000000000000000000000");
        assert_eq!(
            x25519(&a, &g),
            b32("8520f0098930a754748b7ddcb43ef75a0dbf3a0d26381af4eba4a98eaa9b4e6a")
        );
        assert_eq!(
            x25519(&b, &g),
            b32("de9edb7d7b7dc1b4d35b61c2ece435373f8343c85b78674dadfc7e146f882b4f")
        );
        let ka = b32("8520f0098930a754748b7ddcb43ef75a0dbf3a0d26381af4eba4a98eaa9b4e6a");
        let kb = b32("de9edb7d7b7dc1b4d35b61c2ece435373f8343c85b78674dadfc7e146f882b4f");
        let want = b32("4a5d9d5ba4ce2de1728e3bf480350f25e07e21c947d19e3376f09b3c1e161742");
        assert_eq!(x25519(&a, &kb), want);
        assert_eq!(x25519(&b, &ka), want);
    }

    /// Field sanity: small ops, wrap-around, (-1)^2, sq chain, inv.
    /// (Regression net for the two real bugs found here: the u128
    /// accumulator overflow and the missing +19 on bit-255 clear.)
    #[test]
    fn field_sanity() {
        let a = Fe([7, 0, 0, 0]);
        let b = Fe([6, 0, 0, 0]);
        assert!(a.add(&b) == Fe([13, 0, 0, 0]));
        assert!(a.mul(&b) == Fe([42, 0, 0, 0]));
        assert!(a.sub(&b) == Fe([1, 0, 0, 0]));
        // wrap: p-1 + 1 == 0 ; 0 - 1 == p-1
        let pm1 = Fe(super::P).sub(&Fe::one());
        assert!(pm1.add(&Fe::one()) == Fe::zero());
        assert!(Fe::zero().sub(&Fe::one()) == pm1);
        // (-1)^2 == 1 (maximal limbs through mul)
        assert!(pm1.mul(&pm1) == Fe::one());
        // sq chain: 3^2=9, 3^4=81
        let three = Fe([3, 0, 0, 0]);
        assert!(three.sq() == Fe([9, 0, 0, 0]));
        assert!(three.sq().sq() == Fe([81, 0, 0, 0]));
        // inv(2) == 2^254 - 9 (exercises bit-255 handling in mul)
        let two = Fe([2, 0, 0, 0]);
        assert!(
            two.inv()
                == Fe([
                    0xffff_ffff_ffff_fff7,
                    0xffff_ffff_ffff_ffff,
                    0xffff_ffff_ffff_ffff,
                    0x3fff_ffff_ffff_ffff
                ])
        );
        // inv: a * a^-1 == 1 on a wide value
        let big = Fe([0x123456789abcdef, 0xfedcba9876543210, 0x1111111111111111, 0x2222222222222222]);
        assert!(big.mul(&big.inv()) == Fe::one());
    }
}
