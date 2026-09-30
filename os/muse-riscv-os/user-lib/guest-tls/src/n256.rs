// v3.14: arithmetic mod n (P-256 group order) + ECDSA verify.
// n is NOT pseudo-Mersenne, so reduction is binary long division
// (restoring division, obviously correct, ~512 iterations); inversion is
// Fermat pow(n-2) reusing mul_mod. no_std + no_alloc + zero deps.
//
// n = ffffffff00000000ffffffffffffffffbce6faada7179e84f3b9cac2fc632551
// (limbs LE, machine-split by python -- never hand-convert these again).

use super::p256::{generator, on_curve, F256};

/// n, little-endian limbs (python-split, verified by n_selfcheck test).
const N: [u64; 4] = [
    0xf3b9cac2fc632551,
    0xbce6faada7179e84,
    0xffff_ffff_ffff_ffff,
    0xffff_ffff_0000_0000,
];

/// Integers mod n, always < n. Distinct type from F256 (different prime).
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct U256(pub [u64; 4]);

impl U256 {
    pub fn zero() -> Self {
        Self([0, 0, 0, 0])
    }
    pub fn one() -> Self {
        Self([1, 0, 0, 0])
    }
    pub fn is_zero(&self) -> bool {
        self.0[0] | self.0[1] | self.0[2] | self.0[3] == 0
    }
    /// self >= n ?
    fn ge_n(&self) -> bool {
        let mut i = 4;
        while i > 0 {
            i -= 1;
            if self.0[i] != N[i] {
                return self.0[i] > N[i];
            }
        }
        true // equal
    }
    /// Raw 256-bit add; returns (4 limbs, carry). Sum of two <n values.
    fn raw_add(a: &[u64; 4], b: &[u64; 4]) -> ([u64; 4], u64) {
        let mut l = [0u64; 4];
        let mut c = 0u64;
        let mut i = 0;
        while i < 4 {
            let s = a[i] as u128 + b[i] as u128 + c as u128;
            l[i] = s as u64;
            c = (s >> 64) as u64;
            i += 1;
        }
        (l, c)
    }
    /// (a + b) mod n. Sum < 2n (+carry impossible: 2n < 2^257 but our
    /// limbs hold < 2^256 + carry bit; carry=1 means >= 2^256 > n).
    pub fn add(&self, o: &Self) -> Self {
        let (mut l, c) = Self::raw_add(&self.0, &o.0);
        // subtract n if >= n (carry=1 implies >= 2^256 > n, always subtract).
        let mut ge = c != 0;
        if !ge {
            let t = Self(l);
            ge = t.ge_n();
        }
        if ge {
            let mut b = 0u64;
            let mut i = 0;
            while i < 4 {
                let (r1, o1) = l[i].overflowing_sub(N[i]);
                let (r2, o2) = r1.overflowing_sub(b);
                l[i] = r2;
                b = (o1 || o2) as u64;
                i += 1;
            }
            // borrow with c=1 would need a second pass, but (2^256+lo)-n
            // with lo<2^256: result < 2^256 always fits (borrow absorbed).
            // If c was 1 and borrow occurred... (2^256 + lo) - n where
            // lo + 2^256 >= n always (2^256 > n) so exactly one sub, no
            // borrow out. debug_assert documents it.
            debug_assert!(b == 0 || c == 0);
            let _ = b;
        }
        Self(l)
    }
    /// (a - b) mod n (branchless add-back).
    pub fn sub(&self, o: &Self) -> Self {
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
        let m = 0u64.wrapping_sub(b);
        let mut c = 0u64;
        i = 0;
        while i < 4 {
            let s = d[i] as u128 + (N[i] & m) as u128 + c as u128;
            d[i] = s as u64;
            c = (s >> 64) as u64;
            i += 1;
        }
        Self(d)
    }
    /// Load 32 BE bytes, reduced mod n (hashes and r/s land here; values
    /// are < 2^256 < 2n... 2n vs 2^256: n > 2^255 (top bit set) so
    /// 2n > 2^256 > value: ONE cond_sub suffices).
    pub fn from_be(b: &[u8; 32]) -> Self {
        // b[0] is MSB -> top byte of limb 3 (shift 56 within its group).
        let mut l = [0u64; 4];
        let mut i = 0;
        while i < 4 {
            let mut j = 0;
            while j < 8 {
                l[3 - i] |= (b[i * 8 + j] as u64) << ((7 - j) * 8);
                j += 1;
            }
            i += 1;
        }
        let mut f = Self(l);
        f.cond_sub_once();
        f
    }
    fn cond_sub_once(&mut self) {
        if !self.ge_n() {
            return;
        }
        let mut b = 0u64;
        let mut i = 0;
        while i < 4 {
            let (r1, o1) = self.0[i].overflowing_sub(N[i]);
            let (r2, o2) = r1.overflowing_sub(b);
            self.0[i] = r2;
            b = (o1 || o2) as u64;
            i += 1;
        }
    }
    pub fn to_be(&self) -> [u8; 32] {
        let mut out = [0u8; 32];
        let mut i = 0;
        while i < 4 {
            let mut j = 0;
            while j < 8 {
                out[i * 8 + j] = (self.0[3 - i] >> ((7 - j) * 8)) as u8;
                j += 1;
            }
            i += 1;
        }
        out
    }
    /// Wide product: 8 u64 limbs (safe split-carry pattern).
    fn wide(&self, o: &Self) -> [u64; 8] {
        let mut t = [0u64; 9];
        let mut i = 0;
        while i < 4 {
            let mut carry: u128 = 0;
            let mut j = 0;
            while j < 4 {
                let p = self.0[i] as u128 * o.0[j] as u128;
                let cur = t[i + j] as u128 + (p & 0xffff_ffff_ffff_ffff) + carry;
                t[i + j] = cur as u64;
                carry = (cur >> 64) + (p >> 64);
                j += 1;
            }
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
        [t[0], t[1], t[2], t[3], t[4], t[5], t[6], t[7]]
    }
    /// Remainder of 8-limb value mod n (binary restoring division).
    fn rem8(w: &[u64; 8]) -> Self {
        // rem as 5 limbs (4 + overflow), divisor n as 4 limbs.
        let mut rem = [0u64; 5];
        let mut i = 512i32 - 1;
        while i >= 0 {
            // rem <<= 1; rem |= bit i of w.
            let mut c = ((w[(i as usize) / 64] >> ((i as usize) % 64)) & 1) as u64;
            let mut k = 0;
            while k < 5 {
                let v = (rem[k] << 1) | c;
                // careful: rem[k]<<1 drops bit 63; c for next = old bit63.
                c = rem[k] >> 63;
                rem[k] = v;
                k += 1;
            }
            // if rem >= n (compare low 4 limbs, or slot 4 nonzero): rem -= n.
            let mut ge = rem[4] != 0;
            if !ge {
                ge = true;
                let mut q = 4;
                while q > 0 {
                    q -= 1;
                    if rem[q] != N[q] {
                        ge = rem[q] > N[q];
                        break;
                    }
                }
            }
            if ge {
                let mut b = 0u64;
                let mut k = 0;
                while k < 5 {
                    let nk = if k < 4 { N[k] } else { 0 };
                    let (r1, o1) = rem[k].overflowing_sub(nk);
                    let (r2, o2) = r1.overflowing_sub(b);
                    rem[k] = r2;
                    b = (o1 || o2) as u64;
                    k += 1;
                }
            }
            i -= 1;
        }
        Self([rem[0], rem[1], rem[2], rem[3]])
    }
    pub fn mul(&self, o: &Self) -> Self {
        Self::rem8(&self.wide(o))
    }
    /// Inverse via Fermat (n prime): a^(n-2). n-2 LE bytes: n limb0 minus
    /// 2 (0x...f3b9cac2fc632551 - 2 = ...f3b9cac2fc63254f), rest as n.
    pub fn inv(&self) -> Self {
        // e = n - 2 as LE bytes (machine-derived shape: only byte0 differs
        // from n's bytes... n byte0 = 0x51 -> e byte0 = 0x4f; top byte 0xff).
        let mut e = [0xffu8; 32];
        // Build e from N limbs minus 2 (no hand hex): compute once here.
        let mut nl = N;
        let (r, _) = nl[0].overflowing_sub(2);
        nl[0] = r;
        // (no borrow out: N > 2.)
        let mut i = 0;
        while i < 32 {
            e[i] = (nl[i / 8] >> ((i % 8) * 8)) as u8;
            i += 1;
        }
        let mut r = Self::one();
        let mut b = 255i32;
        while b >= 0 {
            r = r.mul(&r);
            if ((e[(b as usize) / 8] >> ((b as usize) % 8)) & 1) != 0 {
                r = r.mul(self);
            }
            b -= 1;
        }
        r
    }
}

/// Raw ECDSA verify over a caller-provided 32-byte hash (x509 Certificate
/// and CertificateVerify paths). qx/qy/r/s are BE bytes.
pub fn ecdsa_verify_raw(
    qx: &[u8; 32],
    qy: &[u8; 32],
    hash: &[u8; 32],
    r: &[u8; 32],
    s: &[u8; 32],
) -> bool {
use super::p256::F256;
    let rr = U256::from_be(r);
    let ss = U256::from_be(s);
    if rr.is_zero() || ss.is_zero() {
        return false;
    }
    if ge_n_bytes(r) || ge_n_bytes(s) {
        return false;
    }
    let qxf = F256::from_be(qx);
    let qyf = F256::from_be(qy);
    if !on_curve(&qxf, &qyf) {
        return false;
    }
    let z = U256::from_be(hash);
    let w = ss.inv();
    let u1 = z.mul(&w);
    let u2 = rr.mul(&w);
    let (gx, gy) = generator();
    let p1 = mult_jac(&u1.to_be(), &gx, &gy);
    let p2 = mult_jac(&u2.to_be(), &qxf, &qyf);
    let p = match (p1, p2) {
        (None, None) => return false,
        (None, Some(q)) => return affine_x_eq_r(&q, &rr),
        (Some(q), None) => return affine_x_eq_r(&q, &rr),
        (Some(a), Some(b)) => a.add_jj(&b),
    };
    match p.to_affine() {
        None => false,
        Some((x, _)) => U256::from_be(&x.to_be()) == rr,
    }
}

/// ECDSA verify (P-256/SHA-256): pubkey (Qx,Qy) affine BE, hash (32B),
/// (r,s) BE. True iff valid. Follows SEC1: rejects r/s out of [1,n),
/// off-curve Q, infinity X.
pub fn ecdsa_verify(qx: &[u8; 32], qy: &[u8; 32], hash: &[u8; 32], r: &[u8; 32], s: &[u8; 32]) -> bool {
    // NOTE on range checks: from_be reduces mod n, so wire r == n reads as
    // 0 (rejected by the zero check); wire r > n wraps -- SEC1 says reject,
    // enforced fail-closed by ge_n_bytes inside ecdsa_verify_raw.
    ecdsa_verify_raw(qx, qy, hash, r, s)
}

/// Raw scalar mult returning the Jacobian (None only for k == 0).
fn mult_jac(k: &[u8; 32], px: &F256, py: &F256) -> Option<super::p256::J> {
    use super::p256::J;
    let mut r = J::inf();
    let mut i = 0;
    while i < 256 {
        r = r.dbl();
        if ((k[(i as usize) / 8] >> (7 - (i as usize) % 8)) & 1) != 0 {
            r = r.add_mixed(px, py);
        }
        i += 1;
    }
    if r.is_inf() {
        None
    } else {
        Some(r)
    }
}

fn affine_x_eq_r(p: &super::p256::J, rr: &U256) -> bool {
    match p.to_affine() {
        None => false,
        Some((x, _)) => U256::from_be(&x.to_be()) == *rr,
    }
}

/// Big-endian bytes >= n ? (lexicographic, 32 bytes).
fn ge_n_bytes(b: &[u8; 32]) -> bool {
    // n BE bytes from N limbs (machine-consistent, no hand hex).
    let mut nb = [0u8; 32];
    let mut i = 0;
    while i < 4 {
        let mut j = 0;
        while j < 8 {
            nb[i * 8 + j] = (N[3 - i] >> ((7 - j) * 8)) as u8;
            j += 1;
        }
        i += 1;
    }
    let mut k = 0;
    while k < 32 {
        if b[k] != nb[k] {
            return b[k] > nb[k];
        }
        k += 1;
    }
    true // equal
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

    #[test]
    fn n_selfcheck() {        // (n-1) + 1 == 0; n*1 == n... via add/sub (catches N limb typos
        // functionally; the limbs themselves are python-split).
        let nm1 = U256(N).sub(&U256::one());
        assert!(nm1.add(&U256::one()) == U256::zero());
        assert!(U256::zero().sub(&U256::one()) == nm1);
        // (n-1)^2 == 1 through divmod reduction (maximal wide product).
        assert!(nm1.mul(&nm1) == U256::one());
        // small: 7*6 == 42; inv(2)*2 == 1.
        let a = U256([7, 0, 0, 0]);
        let b = U256([6, 0, 0, 0]);
        assert!(a.mul(&b) == U256([42, 0, 0, 0]));
        let two = U256([2, 0, 0, 0]);
        assert!(two.mul(&two.inv()) == U256::one());
    }

        #[test]
    fn be_roundtrip() {        // from_be/to_be must be exact inverses (ECDSA math depends on it).
        let v = b32("d131a8db03ea5495a36cb1d584b2d9617958a9cbe2d75b0bf4b560c03b383e78");
        assert!(U256::from_be(&v).to_be() == v);
        assert!(U256::from_be(&b32("ffffffff00000000ffffffffffffffffbce6faada7179e84f3b9cac2fc632551")).is_zero());
    }

    #[test]
    fn mult_jac_agrees() {        // mult_jac (used by verify) must match scalar_mult_be (proven).
        use super::super::p256::{generator, scalar_mult_be};
        let (gx, gy) = generator();
        let ka = b32("c0ffee0000000000000000000000000000000000000000000000000000000001");
        let (ex, ey) = scalar_mult_be(&ka, &gx, &gy).expect("PA");
        let got = mult_jac(&ka, &gx, &gy).expect("jac");
        let (jx, jy) = got.to_affine().expect("aff");
        assert!(jx.to_be() == ex.to_be());
        assert!(jy.to_be() == ey.to_be());
    }

    #[test]
    fn n_anchor() {
        // Absolute anchors (not self-consistent): from_be(n) == 0 ties the
        // N limbs AND from_be to the true order; from_be(1) == one() ties
        // the byte order absolutely.
        assert!(U256::from_be(&b32("ffffffff00000000ffffffffffffffffbce6faada7179e84f3b9cac2fc632551")) == U256::zero());
        assert!(U256::from_be(&b32("0000000000000000000000000000000000000000000000000000000000000001")) == U256::one());
        assert!(U256::from_be(&b32("0000000000000000000000000000000000000000000000000000000000000002")) == U256([2, 0, 0, 0]));
    }

    #[test]
    fn ge_n_gate() {
        // The SEC1 range gate must not misfire on valid r/s (a false
        // positive here rejects every real signature).
        let r = b32("399bf277b8f3c0f55466848cccddb1fdc67c5a0f0f078ea86a57e78a2b111c1f");
        assert!(!ge_n_bytes(&r));
        let nm1 = U256(N).sub(&U256::one());
        assert!(!ge_n_bytes(&nm1.to_be()));
        assert!(ge_n_bytes(&U256(N).to_be())); // rendering n itself
        assert!(ge_n_bytes(&[0xffu8; 32]));
        assert!(!ge_n_bytes(&[0u8; 32]));
    }

    #[test]
    fn inv_big() {
        // Fermat inv on a real signature scalar (not just 2).
        let s = U256::from_be(&b32("8307ca2424dd8f6987aa338f878c30174a436492395c414398707c025f104480"));
        assert!(s.mul(&s.inv()) == U256::one());
    }

    /// node crypto.sign (ECDSA P-256/SHA-256, ieee-p1363) ground truth.
    #[test]
    fn ecdsa_node_sig_a() {
        let qx = b32("19962c1db43584d71435d1b44224dd62a00c0359c0a53875179e71ad38eaf6d8");
        let qy = b32("e2dc1be2ece6323dc4dd5fe6ce7dcebf7d03d3671032cb20dc9f84ef5c7d652b");
        let h = b32("e820b06b49de75735186941597d97aecb953fc6cd187c2f49a063884ec3794f6");
        let r = b32("8a614ee135e2c4afbc4ec7f5b98941960a15d7bc7fe5934e949465c37c24bbd0");
        let s = b32("7a4b327c8bf6b293af33ecef3c015c8ed94e962dc901ce642ee1cb5e254f93d3");
        assert!(ecdsa_verify(&qx, &qy, &h, &r, &s));
    }

    #[test]
    fn ecdsa_node_sig_b() {
        let qx = b32("13d541e61978d54dd7b1c11250416f85dce7fc2bb178d05e7914c74185b34103");
        let qy = b32("9e2c5cdabd2b2e8fd0f60a9ac9ecfc1cce7dbdc309f99ac5428d5295af90ad87");
        let h = b32("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
        let r = b32("3e3b0e9e355add7bc26c1c576adbefbd24b89510f877c41132044f2a431310a7");
        let s = b32("6e6bd7a3fa149e2d23c6e3896f762792199e425fa909ce7ecff5386fee282089");
        assert!(ecdsa_verify(&qx, &qy, &h, &r, &s));
    }

    #[test]
    fn ecdsa_rejects_tampered() {
        let qx = b32("19962c1db43584d71435d1b44224dd62a00c0359c0a53875179e71ad38eaf6d8");
        let qy = b32("e2dc1be2ece6323dc4dd5fe6ce7dcebf7d03d3671032cb20dc9f84ef5c7d652b");
        let h = b32("e820b06b49de75735186941597d97aecb953fc6cd187c2f49a063884ec3794f6");
        let r = b32("8a614ee135e2c4afbc4ec7f5b98941960a15d7bc7fe5934e949465c37c24bbd0");
        let s = b32("7a4b327c8bf6b293af33ecef3c015c8ed94e962dc901ce642ee1cb5e254f93d3");
        // flip a hash bit
        let mut h2 = h;
        h2[0] ^= 1;
        assert!(!ecdsa_verify(&qx, &qy, &h2, &r, &s));
        // s + 1
        let mut s2 = s;
        let mut c = 1u16;
        let mut i = 32;
        while i > 0 && c != 0 {
            i -= 1;
            let v = s2[i] as u16 + c;
            s2[i] = v as u8;
            c = v >> 8;
        }
        assert!(!ecdsa_verify(&qx, &qy, &h, &r, &s2));
        // r = 0
        assert!(!ecdsa_verify(&qx, &qy, &h, &b32("0000000000000000000000000000000000000000000000000000000000000000"), &s));
        // wrong pubkey (PB with A's sig)
        let px = b32("13d541e61978d54dd7b1c11250416f85dce7fc2bb178d05e7914c74185b34103");
        let py = b32("9e2c5cdabd2b2e8fd0f60a9ac9ecfc1cce7dbdc309f99ac5428d5295af90ad87");
        assert!(!ecdsa_verify(&px, &py, &h, &r, &s));
    }

    #[test]
    fn p1_matches_node() {        // Isolate u1*G: node ECDH(priv=u1) ground truth for sigA's u1.
        use super::super::p256::generator;
        let (gx, gy) = generator();
        let u1 = b32("b9fd9825a2ad5f591e21caf8b5cc910679123d1f38185021e6ae35965c68c22a");
        let p1 = mult_jac(&u1, &gx, &gy).expect("p1");
        let (x, y) = p1.to_affine().expect("aff");
        assert!(x.to_be() == b32("746b53efa139fc8a7c2d4fa597fc4ec1f773a8a88f80916db630190fc1ef2d3f"));
        assert!(y.to_be() == b32("70dff20c754fe078100379b8edc62c8b974e8696171a72ff3b38db977ed95dc6"));
    }

    #[test]
    fn p2_matches_node() {
        // Isolate u2*Q: u2*Q = (u2*a)*G with a = privA; node ECDH(priv=k)
        // ground truth, k = u2*a mod n (python-split).
        use super::super::p256::F256;
        let qx = F256::from_be(&b32("19962c1db43584d71435d1b44224dd62a00c0359c0a53875179e71ad38eaf6d8"));
        let qy = F256::from_be(&b32("e2dc1be2ece6323dc4dd5fe6ce7dcebf7d03d3671032cb20dc9f84ef5c7d652b"));
        let u2 = b32("1095cdb116565a0946b3040f56ed91175dbb4321d5381c7f3d632ebb2e0a21da");
        let p2 = mult_jac(&u2, &qx, &qy).expect("p2");
        let (x, y) = p2.to_affine().expect("aff");
        assert!(x.to_be() == b32("fc185e1b3a62e6051e23f794045ac816abb371a519b6a7438e1a811534631de3"));
        assert!(y.to_be() == b32("5248200d323734f4e711b094920c13a54780b1ec411f54cbd0102ad43b5b9ef9"));
    }
}
