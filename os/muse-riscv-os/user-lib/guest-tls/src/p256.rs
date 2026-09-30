// v3.14: NIST P-256 (secp256r1) field + curve (short Weierstrass,
// y^2 = x^3 - 3x + b). no_std + no_alloc + zero deps, like the rest.
//
// Field strategy (deliberately NOT the NIST S1-S9 sums from memory:
// those are easy to mistranscribe): schoolbook product into u64 limbs
// (the same split-carry pattern proven in x25519.rs), then a fold loop
// using 2^256 == C' (mod p), C' = 2^224 - 2^192 - 2^96 + 1.
// Termination proof: each fold with nonzero high part H replaces
// V = L + 2^256*H by V' = L + C'*H, and V' < V iff C' < 2^256 (true:
// C' < 2^224). Strict decrease of a nonneg integer -> terminates; while
// V >= 2^257 each fold more than halves V, so ~10 folds suffice from
// < 2^576 and the cap of 20 is never hit (debug_assert documents it).

/// p = 2^256 - 2^224 + 2^192 + 2^96 - 1, little-endian limbs.
const P256: [u64; 4] = [
    0xffff_ffff_ffff_ffff,
    0x0000_0000_ffff_ffff,
    0x0000_0000_0000_0000,
    0xffff_ffff_0000_0001,
];

/// Field element, always < p after every op.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct F256(pub [u64; 4]);

impl F256 {
    pub fn zero() -> Self {
        Self([0, 0, 0, 0])
    }
    pub fn one() -> Self {
        Self([1, 0, 0, 0])
    }
    fn from_limbs(l: [u64; 4]) -> Self {
        let mut f = Self(l);
        f.cond_sub();
        f
    }
    /// Load 32 LE bytes, reduced mod p (input < 2^256 < 2*P: one sub).
    pub fn from_bytes(b: &[u8; 32]) -> Self {
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
        Self::from_limbs(l)
    }
    /// Load 32 BE bytes (wire format for coordinates).
    pub fn from_be(b: &[u8; 32]) -> Self {
        let mut le = [0u8; 32];
        let mut i = 0;
        while i < 32 {
            le[i] = b[31 - i];
            i += 1;
        }
        Self::from_bytes(&le)
    }
    pub fn to_bytes(&self) -> [u8; 32] {
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
    pub fn to_be(&self) -> [u8; 32] {
        let le = self.to_bytes();
        let mut be = [0u8; 32];
        let mut i = 0;
        while i < 32 {
            be[i] = le[31 - i];
            i += 1;
        }
        be
    }
    /// self -= P if self >= P. Valid for any value < 2*P (branchless).
    fn cond_sub(&mut self) {
        let mut b: u64 = 0;
        let mut d = [0u64; 4];
        let mut i = 0;
        while i < 4 {
            let (r1, o1) = self.0[i].overflowing_sub(P256[i]);
            let (r2, o2) = r1.overflowing_sub(b);
            d[i] = r2;
            b = (o1 || o2) as u64;
            i += 1;
        }
        let m = 0u64.wrapping_sub(b ^ 1);
        i = 0;
        while i < 4 {
            self.0[i] = (self.0[i] & !m) | (d[i] & m);
            i += 1;
        }
    }
    /// Sum < 2*P (operands < P): one cond_sub. 5-limb compare/sub.
    pub fn add(&self, o: &Self) -> Self {
        let mut l = [0u64; 5];
        let mut c = 0u64;
        let mut i = 0;
        while i < 4 {
            let s = self.0[i] as u128 + o.0[i] as u128 + c as u128;
            l[i] = s as u64;
            c = (s >> 64) as u64;
            i += 1;
        }
        l[4] = c;
        // compare >= P (top limb decides first), subtract if so.
        let mut ge = l[4] != 0;
        if !ge {
            ge = true;
            i = 4;
            while i > 0 {
                i -= 1;
                if l[i] != P256[i] {
                    ge = l[i] > P256[i];
                    break;
                }
            }
        }
        if ge {
            let mut b = 0u64;
            i = 0;
            while i < 5 {
                let pi = if i < 4 { P256[i] } else { 0 };
                let (r1, o1) = l[i].overflowing_sub(pi);
                let (r2, o2) = r1.overflowing_sub(b);
                l[i] = r2;
                b = (o1 || o2) as u64;
                i += 1;
            }
        }
        Self([l[0], l[1], l[2], l[3]])
    }
    /// Difference in (-P, P): add P back on borrow (branchless mask).
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
            let s = d[i] as u128 + (P256[i] & m) as u128 + c as u128;
            d[i] = s as u64;
            c = (s >> 64) as u64;
            i += 1;
        }
        Self(d)
    }
    pub fn neg(&self) -> Self {
        Self::zero().sub(self)
    }
    pub fn mul(&self, o: &Self) -> Self {
        // schoolbook with split products (same proven pattern as x25519).
        let mut t = [0u64; 10];
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
        Self::reduce10(t)
    }
    /// Reduce 10 u64 limbs (value < 2^640) to < p. Exact; terminates by
    /// the strict-decrease argument in the module docs (cap never hit).
    fn reduce10(t: [u64; 10]) -> Self {
        let mut t = t;
        let mut round = 0;
        loop {
            // high part H = slots 4..10 (value H * 2^256).
            let mut allzero = true;
            let mut k = 4;
            while k < 10 {
                if t[k] != 0 {
                    allzero = false;
                    break;
                }
                k += 1;
            }
            if allzero {
                break;
            }
            debug_assert!(round < 20);
            if round >= 20 {
                panic!("p256 reduce10: fold did not terminate (math bug)");
            }
            // acc = L + C'*H, C' = +1 -2^96 -2^192 +2^224 (i128, exact).
            // 11 slots: the value needs 9 and the normalize chain below
            // needs one headroom slot on top (slot 10).
            let mut acc = [0i128; 11];
            let mut j = 0;
            while j < 4 {
                acc[j] = t[j] as i128;
                j += 1;
            }
            let mut kk = 0;
            while kk < 6 {
                let h = t[kk + 4] as i128;
                acc[kk] += h;
                acc[kk + 1] -= h << 32;
                acc[kk + 3] -= h;
                acc[kk + 3] += h << 32;
                kk += 1;
            }
            // signed normalize (arithmetic shift = exact floor for negatives).
            let mut i = 0;
            while i < 10 {
                let c = acc[i] >> 64;
                acc[i] -= c << 64;
                acc[i + 1] += c;
                i += 1;
            }
            // Slots 0..9 are now in [0, 2^64); slot 10 holds a tiny residue
            // (< 2^36 by the value bound) that the next round folds via H.
            i = 0;
            while i < 10 {
                t[i] = acc[i] as u64;
                i += 1;
            }
            round += 1;
        }
        // value < 2^256 (< 2*P): one cond_sub.
        let mut f = Self([t[0], t[1], t[2], t[3]]);
        f.cond_sub();
        f
    }
    pub fn sq(&self) -> Self {
        self.mul(self)
    }
    /// Inverse via binary square-and-multiply, e = p - 2
    /// (p = 2^256 - C, C = 2^224 - 2^192 - 2^96 + 1, so p - 2 ends in
    /// ...fffffffd: byte0 = 0xfd, rest as p).
    pub fn inv(&self) -> Self {
        // e = p - 2. p is 256-bit (top byte 0xff), so unlike x25519's
        // 255-bit prime, byte 31 keeps its init 0xff; only the zero limbs
        // of p and the low-byte borrow differ from solid 0xff.
        let mut e = [0xffu8; 32];
        e[0] = 0xfd;
        e[12] = 0x00;
        e[13] = 0x00;
        e[14] = 0x00;
        e[15] = 0x00;
        e[16] = 0x00;
        e[17] = 0x00;
        e[18] = 0x00;
        e[19] = 0x00;
        e[20] = 0x00;
        e[21] = 0x00;
        e[22] = 0x00;
        e[23] = 0x00;
        e[24] = 0x01;
        e[25] = 0x00;
        e[26] = 0x00;
        e[27] = 0x00;
        // e[28..32] stay 0xff; e[12..16] stay 0xff. (bytes of p minus 2.)
        let mut r = Self::one();
        let mut i = 255i32;
        while i >= 0 {
            r = r.sq();
            if ((e[(i as usize) / 8] >> ((i as usize) % 8)) & 1) != 0 {
                r = r.mul(self);
            }
            i -= 1;
        }
        r
    }
    pub fn is_zero(&self) -> bool {
        self.0[0] | self.0[1] | self.0[2] | self.0[3] == 0
    }
}

// ---- curve (Jacobian, a = -3) ----

/// b = 5ac635d8aa3a93e7b3ebbd55769886bc651d06b0cc53b0f63bce3c3e27d2604b.
fn curve_b() -> F256 {
    F256::from_be(&[
        0x5a, 0xc6, 0x35, 0xd8, 0xaa, 0x3a, 0x93, 0xe7, 0xb3, 0xeb, 0xbd, 0x55, 0x76, 0x98, 0x86,
        0xbc, 0x65, 0x1d, 0x06, 0xb0, 0xcc, 0x53, 0xb0, 0xf6, 0x3b, 0xce, 0x3c, 0x3e, 0x27, 0xd2,
        0x60, 0x4b,
    ])
}

/// Generator, affine (openssl ground truth, verified by g_mult_1 test).
pub fn generator() -> (F256, F256) {
    (
        F256::from_be(&[
            0x6b, 0x17, 0xd1, 0xf2, 0xe1, 0x2c, 0x42, 0x47, 0xf8, 0xbc, 0xe6, 0xe5, 0x63, 0xa4,
            0x40, 0xf2, 0x77, 0x03, 0x7d, 0x81, 0x2d, 0xeb, 0x33, 0xa0, 0xf4, 0xa1, 0x39, 0x45,
            0xd8, 0x98, 0xc2, 0x96,
        ]),
        F256::from_be(&[
            0x4f, 0xe3, 0x42, 0xe2, 0xfe, 0x1a, 0x7f, 0x9b, 0x8e, 0xe7, 0xeb, 0x4a, 0x7c, 0x0f,
            0x9e, 0x16, 0x2b, 0xce, 0x33, 0x57, 0x6b, 0x31, 0x5e, 0xce, 0xcb, 0xb6, 0x40, 0x68,
            0x37, 0xbf, 0x51, 0xf5,
        ]),
    )
}

/// Jacobian point; infinity iff z == 0 (x/y then garbage, never read:
// all entry points check z first).
#[derive(Clone, Copy)]
pub struct J {
    pub x: F256,
    pub y: F256,
    pub z: F256,
}

impl J {
    pub fn inf() -> Self {
        Self { x: F256::zero(), y: F256::zero(), z: F256::zero() }
    }
    pub fn is_inf(&self) -> bool {
        self.z.is_zero()
    }
    pub fn from_affine(x: F256, y: F256) -> Self {
        Self { x, y, z: F256::one() }
    }
    /// Doubling, dbl-2007-bl with a = -3 (M = 3*(XX - ZZ^2)).
    pub fn dbl(&self) -> Self {
        if self.is_inf() {
            return Self::inf();
        }
        let xx = self.x.sq();
        let yy = self.y.sq();
        let yyyy = yy.sq();
        let zz = self.z.sq();
        let s = (self.x.add(&yy).sq().sub(&xx).sub(&yyyy)).add(&self.x.add(&yy).sq().sub(&xx).sub(&yyyy));
        // S = 2*((X+YY)^2 - XX - YYYY)
        let m = xx.sub(&zz.sq()).add(&xx.sub(&zz.sq())).add(&xx.sub(&zz.sq()));
        // M = 3*(XX - ZZ^2)
        let t = m.sq().sub(&s).sub(&s);
        let x3 = t;
        let y3 = m.mul(&s.sub(&t)).sub(&yyyy.add(&yyyy).add(&yyyy).add(&yyyy).add(&yyyy).add(&yyyy).add(&yyyy).add(&yyyy));
        // Y3 = M*(S-T) - 8*YYYY
        let z3 = self.y.add(&self.z).sq().sub(&yy).sub(&zz);
        Self { x: x3, y: y3, z: z3 }
    }
    /// Mixed addition J + affine, add-2007-bl. Handles INF/H==0.
    pub fn add_mixed(&self, qx: &F256, qy: &F256) -> Self {
        if self.is_inf() {
            return Self::from_affine(*qx, *qy);
        }
        let z1z1 = self.z.sq();
        let u2 = qx.mul(&z1z1);
        let s2 = qy.mul(&self.z).mul(&z1z1);
        let h = u2.sub(&self.x);
        let r0 = s2.sub(&self.y);
        if h.is_zero() {
            if r0.is_zero() {
                return self.dbl();
            } else {
                return Self::inf();
            }
        }
        let hh = h.sq();
        let hhh = h.mul(&hh);
        let v = self.x.mul(&hh);
        let x3 = r0.sq().sub(&hhh).sub(&v).sub(&v);
        // X3 = R^2 - H^3 - 2*X1*H^2 (no 2x/4x scaling games: the scaled
        // add-2007-bl transcription had Z3 = 2*H*Z1^2 instead of Z1*H).
        let y3 = r0.mul(&v.sub(&x3)).sub(&self.y.mul(&hhh));
        let z3 = self.z.mul(&h);
        Self { x: x3, y: y3, z: z3 }
    }
    /// Full Jacobian + Jacobian (for u1*G + u2*Q).
    pub fn add_jj(&self, o: &Self) -> Self {
        if self.is_inf() {
            return *o;
        }
        if o.is_inf() {
            return *self;
        }
        let z1z1 = self.z.sq();
        let z2z2 = o.z.sq();
        let u1 = self.x.mul(&z2z2);
        let u2 = o.x.mul(&z1z1);
        let s1 = self.y.mul(&o.z).mul(&z2z2);
        let s2 = o.y.mul(&self.z).mul(&z1z1);
        let h = u2.sub(&u1);
        let r0 = s2.sub(&s1);
        if h.is_zero() {
            if r0.is_zero() {
                return self.dbl();
            } else {
                return Self::inf();
            }
        }
        let i4 = h.sq().add(&h.sq()).add(&h.sq()).add(&h.sq());
        let j = h.mul(&i4);
        let r = r0.add(&r0);
        let v = u1.mul(&i4);
        let x3 = r.sq().sub(&j).sub(&v).sub(&v);
        let y3 = r.mul(&v.sub(&x3)).sub(&s1.mul(&j)).sub(&s1.mul(&j));
        let z3 = self.z.add(&o.z).sq().sub(&z1z1).sub(&z2z2).mul(&h);
        Self { x: x3, y: y3, z: z3 }
    }
    pub fn to_affine(&self) -> Option<(F256, F256)> {
        if self.is_inf() {
            return None;
        }
        let zi = self.z.inv();
        let z2 = zi.sq();
        Some((self.x.mul(&z2), self.y.mul(&z2).mul(&zi)))
    }
}

/// k (32 BE bytes) times affine base. Returns None for k == 0 / INF.
pub fn scalar_mult_be(k: &[u8; 32], px: &F256, py: &F256) -> Option<(F256, F256)> {
    let mut r = J::inf();
    let mut i = 0;
    while i < 256 {
        r = r.dbl();
        // MSB-first: byte i/8, bit 7 - i%8.
        if ((k[(i as usize) / 8] >> (7 - (i as usize) % 8)) & 1) != 0 {
            r = r.add_mixed(px, py);
        }
        i += 1;
    }
    r.to_affine()
}

/// y^2 == x^3 - 3x + b ?
pub fn on_curve(x: &F256, y: &F256) -> bool {
    let x2 = x.sq();
    let x3 = x2.mul(x);
    let three_x = x.add(&x).add(&x);
    y.sq() == x3.sub(&three_x).add(&curve_b())
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
    fn const_p_selfcheck() {
        // from_be(p) == 0 (p mod p; exercises from_be + cond_sub with P).
        let be = b32("ffffffff00000001000000000000000000000000ffffffffffffffffffffffff");
        assert!(F256::from_be(&be) == F256::zero());
        // BE roundtrip on Gx (exercises both directions, no hand limbs).
        let gx = b32("6b17d1f2e12c4247f8bce6e563a440f277037d812deb33a0f4a13945d898c296");
        assert!(F256::from_be(&gx).to_be() == gx);
        // (p-1) + 1 == 0 (exercises P limbs in add).
        let pm1 = F256(P256).sub(&F256::one());
        assert!(pm1.add(&F256::one()) == F256::zero());
    }

    #[test]
    fn field_mul_sanity() {
        // (p-1)^2 == 1 exercises maximal limbs through the fold loop.
        let pm1 = F256(P256).sub(&F256::one());
        assert!(pm1.mul(&pm1) == F256::one());
        // (2^128)^2 = 2^256 = C' = 2^224 - 2^192 - 2^96 + 1 (first real
        // workout of the fold: the raw product is a single bit at 2^512).
        let h = F256([0, 0, 1, 0]); // 2^128
        let got = h.mul(&h);
        let cprime = F256::from_be(&b32(
            "00000000fffffffeffffffffffffffffffffffff000000000000000000000001",
        ));
        assert!(got == cprime);
        // 2^8 via sq chain (isolates sq from the inv loop).
        let two = F256([2, 0, 0, 0]);
        assert!(two.sq() == F256([4, 0, 0, 0]));
        assert!(two.sq().sq() == F256([16, 0, 0, 0]));
        assert!(two.sq().sq().sq() == F256([256, 0, 0, 0]));
        // inv(p-1) == p-1 ((-1)^-1 == -1; exercises inv with top bits set).
        assert!(pm1.inv() == pm1);
        // 2 * inv(2) == 1 (Fermat exponent ends in ...fffffffd).
        assert!(two.mul(&two.inv()) == F256::one());
    }


    #[test]
    fn field_mul_oracle() {
        // python big-int oracle (random 256-bit, seed 42): catches any
        // pattern-dependent fold bug the hand-picked vectors miss.
        assert!(F256::from_be(&b32("23b8c1e9392456de3eb13b9046685257bdd640fb06671ad11c80317fa3b1799d")).mul(&F256::from_be(&b32("972a846916419f828b9d2434e465e150bd9c66b3ad3c2d6d1a3d1fa7bc8960a9"))) == F256::from_be(&b32("5788f8b94da17d8dee65c44361ea8e55283007638c78f670e68c3d1ac6193ece")));
        assert!(F256::from_be(&b32("9a1de644815ef6d13b8faa1837f8a88b17fc695a07a0ca6e0822e8f36c031199")).mul(&F256::from_be(&b32("6b65a6a48b8148f6b38a088ca65ed389b74d0fb132e706298fadc1a606cb0fb3"))) == F256::from_be(&b32("16af02604ce7944a4304f10ad0f42195164e8655bd01bac146dca24004072212")));
        assert!(F256::from_be(&b32("c241330b01a9e71fde8a774bcf36d58b4737819096da1dac72ff5d2a386ecbe0")).mul(&F256::from_be(&b32("371ecd7b27cd813047229389571aa8766c307511b2b9437a28df6ec4ce4a2bbd"))) == F256::from_be(&b32("93c02e3491c5930d474651e46b62970649b9b48c82c5f2c41997bd9c1e159387")));
        assert!(F256::from_be(&b32("5be6128e18c267976142ea7d17be31111a2a73ed562b0f79c37459eef50bea63")).mul(&F256::from_be(&b32("759cde66bacfb3d00b1f9163ce9ff57f43b7a3a69a8dca03580d7b71d8f56413"))) == F256::from_be(&b32("56bf4aa3d0bd72383c973cd9457cdd878016205b5a1a0257373b0080d2fd2ecf")));
        assert!(F256::from_be(&b32("4b0dbb418d5288f1142c3fe860e7a113ec1b8ca1f91e1d4c1ff49b7889463e85")).mul(&F256::from_be(&b32("3139d32c93cd59bf5c941cf0dc98d2c1e2acf72f9e574f7aa0ee89aed453dd32"))) == F256::from_be(&b32("6adba98d45b65fcbf724ca8b037d6838f9327257f3891b3ff85761d078d86458")));
        assert!(F256::from_be(&b32("fc377a4c4a15544dc5e7ce8a3a578a8ea9488d990bbb259911ce5dd2b45ed1f0")).mul(&F256::from_be(&b32("7412b29347294739614ff3d719db3ad0ddd1dfb23b982ef8daf61a26146d3f31"))) == F256::from_be(&b32("e754c93ef46e6a7672364cb957d78ce353e519ae4dcd1ab443309b80e2188c28")));
    }

    #[test]
    fn field_mul_oracle100() {
        // 100x python big-int oracle (seed 1234).
        assert!(F256::from_be(&b32("f149f542e935b87017346b4501eaf6141de9ea6670d3da1fc735df5ef7697fb9")).mul(&F256::from_be(&b32("19322fed157cf9c6b16e2d5cabeb959208f0ebd4950cddd9ce97b5bdf073eed1"))) == F256::from_be(&b32("9d458f3fd8ae686189b3551c6002b342bcf4dd543da8854a353dab88744b1fb7")));
        assert!(F256::from_be(&b32("040e1e30c9ed0248fc9799a707e36d6004762a223c9f90c95ac96628c4381836")).mul(&F256::from_be(&b32("175e99412607ad5f76ab14759da618fd7bf78a4d9f8f5ffba5f80a0a58994953"))) == F256::from_be(&b32("85ed3de066a4389ba3ccc1d26ad03bdf456ef766a8a3d402ec10ecfc5a3cc293")));
        assert!(F256::from_be(&b32("e16682717c9bbfae80ca17b703be0e66d868c2cf1d4a2b12b6a20bb02edf0743")).mul(&F256::from_be(&b32("118dc10e774520d7e98d7c358a84c15caad14268108727563ff4bb8cf703c9ff"))) == F256::from_be(&b32("d050d98c211e7870c496882cc12ab5a7262d29a082624be0592254676bf3d9a9")));
        assert!(F256::from_be(&b32("d30aad4b45038e220bc4621b9439852083d9fca716c40a33acd51e6699f9823c")).mul(&F256::from_be(&b32("443658625af0f3e0d9a54a0d7b25331f4d6bfd8fa506bfc51025dbe58e725d57"))) == F256::from_be(&b32("32f92d251daca23bd7d3a9ae3ff85a8b353866610157eaa7ba0631d6df14a867")));
        assert!(F256::from_be(&b32("aa58695187b8a518e065e3eb74113cb033354fc7eefadf23a7cda6c23fc86ee6")).mul(&F256::from_be(&b32("b5c36ec124ce01e15560eaba017ad051121213ca8212f7c6f1048aa604f0d0f2"))) == F256::from_be(&b32("16ec86a2ae4ea62d51018b1870e36adcabe01091a5d38cca30e9e9f513ba8b3b")));
        assert!(F256::from_be(&b32("7f1ff9fe966844aa138411eb0dde6d082ac7e1da6099d795a8486261790b2f7c")).mul(&F256::from_be(&b32("58a295d4eff35b6106f1e77124ed49b137106d208ead31c81348486129fc1d9d"))) == F256::from_be(&b32("dbeb6ebbbbe68574d98431262053a19acb447fd536bf9fbaafceaa56c832f697")));
        assert!(F256::from_be(&b32("7442a1244e2e3761aba601ca242780aa879951fff4f991a81c63373ac55ef186")).mul(&F256::from_be(&b32("458c3cdb2d665a7b0a4adb41ce779a93a99226f446db4bc46a8f69260a228ba8"))) == F256::from_be(&b32("50327b60937aad105ddfbfc6ef03fdaf2131dcab21f84808a9ddcbc1a21ce517")));
        assert!(F256::from_be(&b32("76e99afdd579c4c9c777b54b2790ae2cd8fba355f46871014cdead2e2791eef8")).mul(&F256::from_be(&b32("95365d68577a296e7ef077e0597ee18bc3a671c462dcec669027b9ad0a831788"))) == F256::from_be(&b32("19ed54ce7d4bb17455c1c48989ca9dc7e385e15b16c0c5b389cae543f56fb13a")));
        assert!(F256::from_be(&b32("dec4ac467888fa7aeed66a5ac86b7f7f0b9ab36679d6dedb77d6a830d103b91f")).mul(&F256::from_be(&b32("84eb1e73c7f3c8a0bbc9a6e0708963bb2b833e28e1ae6a00984c6df8d13d74f3"))) == F256::from_be(&b32("0114de4f0199c49d8412dce03448909d38b635eb34b47133812450a5f7363fd6")));
        assert!(F256::from_be(&b32("6a94427d2f84ea0fc7154f271fb661b44669165f4bb19d02701861c0d092e07f")).mul(&F256::from_be(&b32("d72f9ecdd454f1e81a644d9287a0eabff0689ae11e956a7dc4e145896fa19d46"))) == F256::from_be(&b32("c98d98a61c9d84c8886cc719b2a9f4bde4179234f3a063ec72b56079bd7fcc79")));
        assert!(F256::from_be(&b32("31007342a41b657a4166c3fba8094805d11776a4d15703e0607741867c362491")).mul(&F256::from_be(&b32("949606b3283c4412f6a11b92cf58440cb33bfa31b3e174eb1bb039fa5868c99b"))) == F256::from_be(&b32("feabd73de536908e4b5597aa742b67e0cb84c0bb97bc6e7847dc768ef02a850f")));
        assert!(F256::from_be(&b32("fbd67b31d729a460d911b0dc27a40502f8d07ea044f2d5a4f1689e5fe87212b0")).mul(&F256::from_be(&b32("14180ee8121e3cc4b87ec0d17259023bc9782a58cb26e7a552d037e4b85a4eab"))) == F256::from_be(&b32("69e41f43402d7a1fdb876d5654f838ed2061d52d756d2a61d926c5906eaf3ef8")));
        assert!(F256::from_be(&b32("382a0c2d09ba039acdabc3ea49400b81e50008d80c38614436d288d4652ea613")).mul(&F256::from_be(&b32("ff977125b30b0b98f0604517eecda947caf4a2a116716583d4621dc481b81040"))) == F256::from_be(&b32("4711c1c7467c8e7082aaea8d8479af02e05b30e987add5b7fb201f57b94d926a")));
        assert!(F256::from_be(&b32("db44062d41e6dc0995c13a7910f44ac075f93a5ff1eeddee4e3d69675125ff0f")).mul(&F256::from_be(&b32("31d17176658aa25d19ee50c433af81d9f312c9346d22469db0575eb712b01ad0"))) == F256::from_be(&b32("a6910308113c4f76f1002be908db56a77886287b7d60f631cbf687e187241af1")));
        assert!(F256::from_be(&b32("cc2c6076d18b48943c7ff71f8021ef3275a66c1ae32996b4e2ee229ab471b2e6")).mul(&F256::from_be(&b32("6bd20911b3d18022e3c1bbce83d9479c3480251adbd2db62ee57a9865c7b2cec"))) == F256::from_be(&b32("4bd4c8c2fd53f0d10a6b9d6d664da84d52db160da8d4edc105abdabecbbc8d71")));
        assert!(F256::from_be(&b32("5a3c8204d01707780f7087574d4142f83408d67f95a290e3f3d9ff9f2cb87a7a")).mul(&F256::from_be(&b32("78f190db8c8d4630f36d31e8c5f8a3f2e80dcc197fe4f416272ef8a588ca8e3c"))) == F256::from_be(&b32("3c1a1c5365fd15e726c72a10e82d6ca4ace0c7cd5d915056406401673f7508db")));
        assert!(F256::from_be(&b32("251a0926ece37d772a42ab569fdf9fe20c4e7af82184d0ea2383bc6655712a55")).mul(&F256::from_be(&b32("ab11c60e006fd4242bc835ab5345e427b6bb83ff11db1f308bcae492d7a384ea"))) == F256::from_be(&b32("1b30777ed78abba4b9023658cba707ec74702b2b5ccc2943887720687ae66129")));
        assert!(F256::from_be(&b32("ea60312b25f547a0cac74fe0c2064f3e166e4be7a36653630f923425acb8f4af")).mul(&F256::from_be(&b32("f8c1c3a53505c031530c999ebc9a000564357c976eca2ba00a37a0db378f8529"))) == F256::from_be(&b32("1f25cd946c8e1327c98eb4105a451131fed9b0f69783c114a9162015ca7159cd")));
        assert!(F256::from_be(&b32("47b133eef5c42d3eea4bf216dfd13150efb725cd619664f3acb9cf9fd2a570de")).mul(&F256::from_be(&b32("93e7605daf6ddc5951a6efbb6c0a9bba4df5f4a8ebcb1200a1c09da38998cb80"))) == F256::from_be(&b32("ad6748611208422905d932c112e7ce7af4693489ccf9224f0f40204c6fe5dff4")));
        assert!(F256::from_be(&b32("472c577042a40dc8c3ac088d17c6405e885b13d2d6b97c4426c340cdd8cf04ae")).mul(&F256::from_be(&b32("b39fe58d1d1dfe49869adcef46d8992e90a01965db9a6092666ac88a6906ab68"))) == F256::from_be(&b32("c50fb32b0e2944841e7ad01b1bb8e99ad4f36779a25bd41803656bcba97cc7b0")));
        assert!(F256::from_be(&b32("65f982359667060cd64e5604b75b48a14c256abe36c138a44633acc96016d60e")).mul(&F256::from_be(&b32("7d917f7bc6e211a03b84edf770c837ead5a272e5e09f46eb597a86640d709248"))) == F256::from_be(&b32("5c750e1422fe94c3cbfdcbb289695bb57c488b8266c173fc46dec4a0b4488dd4")));
        assert!(F256::from_be(&b32("4c6552be651e1ca57aeed6af12c5830900a074ef4ed3a8d1616b2db62a527521")).mul(&F256::from_be(&b32("a0df733e6687f2112f821d3c7ac6450dd3e0ed84aecbbccfd7846e536dcd11cc"))) == F256::from_be(&b32("463b904a360010df7ee624c200304cbaa6ac2adec7825bcfe9fab0a7d857f56b")));
        assert!(F256::from_be(&b32("58b3098c99b9f557a7bcee9f5e993811976cb84e4800ae9e283da2c37a5bb776")).mul(&F256::from_be(&b32("cdc4a66675194f64916a0b9aedff0776ff074b2584d44ac42b42611a34c0df18"))) == F256::from_be(&b32("e7cac9bdbbf7a97fff7556a8acfe01f29000cb34f411b0506dc0796fb4dbfb79")));
        assert!(F256::from_be(&b32("77c7c9e9b9bdab68cbada6a0cd24f4ec073e4d07a8eb7bd679740d27ab1665d7")).mul(&F256::from_be(&b32("79cf0607cb478aacf81184f95ddbb58906e3acb31a8ccaf8cca2da50a9241885"))) == F256::from_be(&b32("8bb033e388270e5b9bdac687983a7389f8882105df16d8cf3f225404be3f2413")));
        assert!(F256::from_be(&b32("54ef7f9596885018e5357be9e7c5af18e363bad923d83263ea84727e10b07891")).mul(&F256::from_be(&b32("ae78c1bd4c7253d94a26acd1fee4c01fc0c16b36c0eaa2d15950a263faec571d"))) == F256::from_be(&b32("32272204228f8c1bf9a8c8f230008d6c4978d4a70fe766f51258a33e2a2a5568")));
        assert!(F256::from_be(&b32("9001f9094b3540c8f399f3ddf22b18b0901715a354a1552c9543f3e3afa16656")).mul(&F256::from_be(&b32("b18ed7e488b66e13128e58f7476ed0d1bfe389a3d074f080825867c731cf194d"))) == F256::from_be(&b32("5e344025d4e12c911db03bd60f68d64b089788f94f77c8fee88557829f0c764d")));
        assert!(F256::from_be(&b32("75417bc5523110c4e9f98fdc55906a7f82a1282e56f24510343969949a2a05dc")).mul(&F256::from_be(&b32("1e9f30c58c59c9d79520e2cb96d7c29a86fb3b8dc279e30015bfb225447b2100"))) == F256::from_be(&b32("41c58a95846e6913cad3b9650e51bb5d42013a3423722c45bcf5d636619a23d8")));
        assert!(F256::from_be(&b32("6500aee50743d9d6dedde4e22317e4335f331ded41d49f573ec8e662d2fabde3")).mul(&F256::from_be(&b32("27a193fcaaa64825356dfe5df94beb7a0b487f7cfd9cf50baa25bcff09900747"))) == F256::from_be(&b32("b535b3ec8cbc71dc23eec3e9daac0bec30a72511f3fbd3955d018e2e136a063f")));
        assert!(F256::from_be(&b32("e85412d483e9aad5e8a30eca6f3f05be5afd517f8401cd7750215537fd9bebf1")).mul(&F256::from_be(&b32("7c4e48f6b2b5667042ecbec04e1f10c5f51cbd6dc273e2f2d814ac0f11a9c6f8"))) == F256::from_be(&b32("cbc0bc6cdd24f3fd128da6e515796368d6b2ba8cf7df7ddc4b9f7cabd8abe4e7")));
        assert!(F256::from_be(&b32("e915af29a7344d0915372196811ecdd75905f56a3837a566946eb518e8d52b3f")).mul(&F256::from_be(&b32("3ac77737906e16f2dcd0a52354c2676f95c5cd90aec8cc0404a5f6a0cf9af702"))) == F256::from_be(&b32("bb65ac7e058d08f58e2bb21eaa719b5c4a23512c2c1c7205e66e1463e4567a4b")));
        assert!(F256::from_be(&b32("fde132a9f33f2c41acff289ef0a6fd90af5898857fd9ba927a5cfc72299bd8fd")).mul(&F256::from_be(&b32("88099c382541942f1177cc299eaf43899f64bf338845b755e0bac50623d3057c"))) == F256::from_be(&b32("f2a16cca51679d49120f58a920ce701d3f122a0fe0f4f28faf9022209fae16c0")));
        assert!(F256::from_be(&b32("c122da2281a580c8e9fdac722a0c4b7eb04bf9c43e47f944d6dd280df2f1125a")).mul(&F256::from_be(&b32("210108575395b630d6bd24ccda69d41c64a5bbcac600b67cb0fc32778147ea99"))) == F256::from_be(&b32("b8c2cf524bd9e3ce2db2c5e8d141d31d4b9dad523883a4ed6bde5783c6d095b2")));
        assert!(F256::from_be(&b32("d820550099651db19c340baf88fb155490f091988fcd0a49fff4781eb54e6265")).mul(&F256::from_be(&b32("d72d0452c34d5a295e7dad1cee0256b8119583dd5f27f9cf0a7788c272433100"))) == F256::from_be(&b32("250118950880b5a2f0fb7ce5e318d1aeb024236070f0a10bbeb033664c085b9b")));
        assert!(F256::from_be(&b32("506ca75f1226a3389ca935439aa2835f0b6ca0a7e2e548dc688c85adad05c8eb")).mul(&F256::from_be(&b32("85714ac2f08aed09f0954d9a589faaa232bc9ac4cd92bccd9f8da4603a592982"))) == F256::from_be(&b32("0cc24a9365d9e5b609bdf0227135bce5cb82089246cfd5db01b3bc094c0ea552")));
        assert!(F256::from_be(&b32("07a387be0d6d342afa2a75557e2737c896fc096b6139b443c4e4fec9b065bb30")).mul(&F256::from_be(&b32("571e381eede60c686d2859fe4b0ccc9ed40e8a1114868a28ce55459672b515ca"))) == F256::from_be(&b32("84ed2ff9fc2e8a15eef02049c43820f2a497e2c012daddb39dab01c0c62652ca")));
        assert!(F256::from_be(&b32("fbf7e066b16ce2ef3df59654a69a1a3ac14def10ad4a74957c74761225dc6184")).mul(&F256::from_be(&b32("6a3d81d15f14d3fb97683f685b4ab518bcd5f9f0dd3fe3e707a11010cb626433"))) == F256::from_be(&b32("34deff634de3837b582c81581cee04724e1273b45eb1cdf9a2c1077c59c9379e")));
        assert!(F256::from_be(&b32("d5bca4c6324a83e1c67c2e9a36575fb3048f1b2ca3b152d3131d34312c8e80fd")).mul(&F256::from_be(&b32("f867198d80799b6424366747bb0baf4e8c2e01c79ed3f4729aeb5dd8fd76b098"))) == F256::from_be(&b32("257ca6a3514bd8e884d2a6b70c5ef82ded5d46fa7279d9f8c6470c5e297eee48")));
        assert!(F256::from_be(&b32("f04041b20caca470be43bbd9a6b815864135e5c0e901b1b0ac9ca06721eb8c3d")).mul(&F256::from_be(&b32("49d50a0b5f2dc7fcbaa17f979906c8305c8e8dbe77d1f9da999172a8e9fb20f5"))) == F256::from_be(&b32("d617d7ea187a4aa075e4416edfb99210d0446f27052a7554233172b6acd36d11")));
        assert!(F256::from_be(&b32("e6350092023d1e894907786d3bf292bf5e2cb05dc745b6e3687fdb24658b2183")).mul(&F256::from_be(&b32("09106413999369888b523b231ed75a644418823efc4160f29541e6b85010579b"))) == F256::from_be(&b32("de0de5348fbe36158e1140cb4747b2facc6d5e112c8563aa28c5c6f959c798f9")));
        assert!(F256::from_be(&b32("2f1402908985b34f5e916eb797f3e40b9232decc88b37b8137f386ecdd75afc5")).mul(&F256::from_be(&b32("b1e360a7b64f021145dcafe6ed361d5fd698a72c5ce4138d4afd1877527e1ca8"))) == F256::from_be(&b32("50ee6d552db9da2a1992c454a7d3927d87dae2f197b6abebaacd57354c2220b7")));
        assert!(F256::from_be(&b32("00d3add5e07853c12b50eea1e935785ffde25b2b74f54bac2d22f101a486e9b8")).mul(&F256::from_be(&b32("7198957a3114a8f43d30ee74689aa90ab21766518b26a204f8ff5732f79711b4"))) == F256::from_be(&b32("9a23fc133311e543f5b3ac4032f1b4050e3afd3dcc1283b21a4877265b975721")));
        assert!(F256::from_be(&b32("bde4c119f22a63bf5a96bc8a2c9b6d3dba187596de21ba89d57ed9be45fdc1f0")).mul(&F256::from_be(&b32("9948e492c138035f7c750a7287b6013068cf33aed8d8d5b4d042d524961b90ab"))) == F256::from_be(&b32("86a7fb37ae306c05ecbeace290628b9ead7140d49277fb6b392f705a2574c173")));
        assert!(F256::from_be(&b32("bafc1f6bb965b05471fdb27083ab69207aee13bbd3fca1ca7b29ce4e965338fb")).mul(&F256::from_be(&b32("8771825dacccf66eb6a90d231a42eee19e4406685f7edd34bc6c1a3fcdb29178"))) == F256::from_be(&b32("d3f552b5630aa7eab2240c8508a1f34f1ec627e0a75613b65b6e691abba266df")));
        assert!(F256::from_be(&b32("7827eafec8cf74721b2907132085dc12947f1c86d584838166caf128d9abe680")).mul(&F256::from_be(&b32("4182b720c507e171cec631a721c02dc18ec6ae928bddedbc18c559d47cf47955"))) == F256::from_be(&b32("5e28350fd6657e4bd1cc6007c3812c069214b98517ed5886fd8b9c0e69ea57c9")));
        assert!(F256::from_be(&b32("4b28a002f4e4847f646615ac55d6bd95122409a4b2d10d0efdeb73c33d2d50ec")).mul(&F256::from_be(&b32("9b03e38330377d50da96c3a68c4ce95467cd94be5adfc75d85ae555db62504b1"))) == F256::from_be(&b32("a6964647c2a9bfd9f393cafb23c9373d0cf6a6300759d31ca81c5135b93dc544")));
        assert!(F256::from_be(&b32("f220a4ac51cd92bf773714a4edd45b9eacbf5c91ee5ecca8acfd90c2791193e8")).mul(&F256::from_be(&b32("769449adfd55e76acffc015b8a0704e01b9f5928865de9b5eb162bdd710b72cd"))) == F256::from_be(&b32("238b4c344731fe5e374ba87e418dbe3fcdd92fa0c683dc14a71dfc66b620c618")));
        assert!(F256::from_be(&b32("2c32fef255fd12b8e5b378949b9286cc2d01b2db328cbec11378718f15fbba18")).mul(&F256::from_be(&b32("287cd4480785930b43bb19f93318a7b7ed711826b1e5d3ee458091e960b4935d"))) == F256::from_be(&b32("e223f2e2f1c6c1f0100cf14acc8b2939d54393143efb0636d7e9ab5ae43a37de")));
        assert!(F256::from_be(&b32("ce86ce410e41b1d70b9514ac19893d929a24cf9850e7c829a9670974f8885a4a")).mul(&F256::from_be(&b32("97a512c8facc92754ed435b2570756cb52bc1d7e6c8ad74b42a673a4adfd4423"))) == F256::from_be(&b32("15b68792c048578c085c2d5a72159104c7627d080e3fd6ee0ab894ab8a4124e5")));
        assert!(F256::from_be(&b32("60310fd32a903e1cae255bbae70c71c293a561e5b6fb41cb6f09749eedfa05a2")).mul(&F256::from_be(&b32("48a0b4bbebe5c4a19826037798bac0214a020b8c02441c336501ecddf8f116b2"))) == F256::from_be(&b32("79d4874b02bcc910d4ebe4be9f83f2e9a2f916d4bdd50e71fe4d07bc96c81275")));
        assert!(F256::from_be(&b32("3fb413f19c00520bdb4f0763ed0f531d510e393ab42db9e2d47701dfe8ed9082")).mul(&F256::from_be(&b32("9b576d0073f46b3a9c2ef1ed31bd820adbaa2f6cebb712c9d74da6e727f51f96"))) == F256::from_be(&b32("9093a171070dda6b45d0f06d1b11b99d8210ec7912e41b2a101fb3488e0e2f74")));
        assert!(F256::from_be(&b32("cf5889a744cd58de85ea2e59561e1774daa1768a8078be0a6ae198ca71ed768b")).mul(&F256::from_be(&b32("50ad4b427e59412d930edbe4af80c32c4279194d644b85386f60a71de38fb707"))) == F256::from_be(&b32("31f40c011549a511391cc138451df1059da2a01a3b35c559cd33a40cbae3288a")));
        assert!(F256::from_be(&b32("692d37f4e76631ce29f6293ef85eb85e2865d6f649449c40d53daf903f68ae69")).mul(&F256::from_be(&b32("fb70a1c7ebe737048b9001e7f57de19d9388ca133b88eb0708d130a0285a6a78"))) == F256::from_be(&b32("a3de2abe96d7455fe86a2857919a7df2e5a4fb98ac43fb325f838f2360e71361")));
        assert!(F256::from_be(&b32("06325ab871bb9ec6eb00c7ca6e1ae03034ce0fec8b39c160a0d018c05a95c2ee")).mul(&F256::from_be(&b32("10263fecb7aa40d3e27b5723b7dc1dc911b618a2c7a7db476784a9048cc4d8cc"))) == F256::from_be(&b32("d436a3b1cfc0bb90e6aeac0c37924e80e76c1084622d0c6c2ef718876fcc3049")));
        assert!(F256::from_be(&b32("e2a7cd6eb6b6565a6191c232a57e5e4fd38cd22f3e90f8c3d0ff64251220ed66")).mul(&F256::from_be(&b32("4d7f58e7b1473b203ccc3d25f4e76ea7c85d045cc78df56261344f617c468bcb"))) == F256::from_be(&b32("c75fa5a5954dddad5a1e27570a634e90f1d8ffe6ffaea8c22ff5067553936cf0")));
        assert!(F256::from_be(&b32("73b3097984b40ffb84f4461bd14559d4f7063d8c53c848d3835e1550d5181223")).mul(&F256::from_be(&b32("06af980ac524a52eceb3b0614d80457d1072400baea979c6a8d16ddcce3ef6cd"))) == F256::from_be(&b32("3c4c1ae4dfa57f5f760419b8a61e86fd27cd4fea12a1a47c602927f26860b890")));
        assert!(F256::from_be(&b32("6da2e572601d5b2725991680248ec4a12e3af7028045cd38d81b2b7bccb7fc9f")).mul(&F256::from_be(&b32("8a67f88d7d0df03b1802225d4d274cd8ecfc4be9230f05cb870ca2d52d540f03"))) == F256::from_be(&b32("725016574d75c855debdf8df2ab5b3a9e25428de630d435ffdaf88e62f0618af")));
        assert!(F256::from_be(&b32("c7b68cc2eab87eb7de391095445bde332230b2b599e0de8655faa27bcd1c6d88")).mul(&F256::from_be(&b32("50c2b6f3df33adbf475db4cd78d518ffbe25f2ccdc1d05d76e534c59f3035664"))) == F256::from_be(&b32("477179323acc1dcc6e64bce459b01b089c51df5c2727fb05ec0b1b6bbd32feba")));
        assert!(F256::from_be(&b32("c102e9003fefa7e3713f93483af3f62d707266a4cabfc37710e0c2ae2ae1d429")).mul(&F256::from_be(&b32("f99d5291d49c3356a12b7c40d0d862ba27fad683d357b3c144432ae1111adeff"))) == F256::from_be(&b32("61bd906a54645deef64b877a1154951dab9238e38d9e6ba759309ef2446d377c")));
        assert!(F256::from_be(&b32("337a41a337c62f41fb9969ac64e1857bfaa0df51251647e746311dc98fb72d4c")).mul(&F256::from_be(&b32("ada5a6ebb96fca6a57b5ac4936497740ebc8a6cf256bd47485bcae138f78d6aa"))) == F256::from_be(&b32("7eb88bf1e30c88d0fb9ab3d3b55c889ae0486ab3d04594fcdb8a12f295247fd3")));
        assert!(F256::from_be(&b32("e1d6805d58a609bbf45fcbeb84b83b91007942c9adf9ca6f69c561ef9e3c7d6e")).mul(&F256::from_be(&b32("e0916cdf04851e68043f08688da90679d7c6efdb3940f4f2a42121a230f99483"))) == F256::from_be(&b32("27f8ff2bddda0d1f9255b8290458fd408632567b35c35efc67af6caf526e87f2")));
        assert!(F256::from_be(&b32("6a13323302d58574c3642d83aeb83ff53c851f9b70b56386d1109dc05cf8c66c")).mul(&F256::from_be(&b32("9538f786a6031277ea205cc7f16b57f454e70bde6bf5debd7f2daef26c1f1ba6"))) == F256::from_be(&b32("deaaed0c9295dd675cb026db45f82bda374daca74cd4b73f46f5fdd725355453")));
        assert!(F256::from_be(&b32("b84ea1bec7c73f309380a865c58d2df6963c4eb5c1cde5ae3fd2e6d184becd5e")).mul(&F256::from_be(&b32("4ac5e7fb052b27d0045e6cef973ef46b78071a504e33b8577c1dc4f89049a277"))) == F256::from_be(&b32("3c862c24792bb265a72aa1b164598afa37c8eafcfa646617674fc3a54b48cd2a")));
        assert!(F256::from_be(&b32("db3883e0926d5ad273cb068df8307efcec792f501f9826a391185976b2bd8adc")).mul(&F256::from_be(&b32("644219486e6a93ac0ac72a243599ed8c6b4e2bab652d6d983eb069f9c25dbd51"))) == F256::from_be(&b32("21a1c34029b0622391777daf584332ca9193bfbc907c6f3df62598d446d39880")));
        assert!(F256::from_be(&b32("1285e4e59c0d43e42c921cd9db535f0d2d90a7a069a15dbbf52edb191c8cc6bb")).mul(&F256::from_be(&b32("c20a75b8bf6bbec42ba8c6a5c96f3f0a5e53c2e5b88fde3cc92ac3bde99a7c0d"))) == F256::from_be(&b32("83e614f30830b7813eed0aa74dd5b5a103dd96115a26e171c99a68919ad770e1")));
        assert!(F256::from_be(&b32("44ab018f2083c6c60874c1026143117f833cd4a210326bc3cdbde5aa3ac4ea87")).mul(&F256::from_be(&b32("4650bd73ccf2d3a50f58612a8227722f58576cbd075d5c4b8075dbb7acc273fb"))) == F256::from_be(&b32("b7c124e6de2438a43fc382b1b9a4d7c34b389c39c8ca33d4439858222d7b21a9")));
        assert!(F256::from_be(&b32("7b31d043f2678335695c06620b912a6ae9e6c0870486fb8b84897ecfbae1921b")).mul(&F256::from_be(&b32("da200eed3b0f23032ebe0b6ae211291149d0e84dc111696bb51fa91f07c9a994"))) == F256::from_be(&b32("e624c501869f47aad0bb7afde73743bf3328d4c60ce1ee7b28ca3b3af40aae9c")));
        assert!(F256::from_be(&b32("7215582b3e4f1fde4b6b1cf7e49b95380fd11c44937b16957d6f1db79f65d17b")).mul(&F256::from_be(&b32("4fcccc46bdd62ef9a4ca06936adcc71829468671436ac9f02cd120e3757f36ee"))) == F256::from_be(&b32("b5ffae557754dd5f3e182777627b4540405899b78d5d8f87595c4175f13eb62d")));
        assert!(F256::from_be(&b32("5379734ea78ccb0fd580fb994ac93510e92960dc0270e899e4cd9ff185b156c7")).mul(&F256::from_be(&b32("08524cc6b354bd652f7faa39e9abd3f2348148db98419b42a0e17631f4667f83"))) == F256::from_be(&b32("8c71c846394b8bd9b58f829636aa5b9ea1853d47cea51c70e2f64c9f1502f881")));
        assert!(F256::from_be(&b32("049ee299eba5a9127fec95f19b2995e0bd191aeba1022fe44eaea9afa70b7315")).mul(&F256::from_be(&b32("de918c34496d5e469ea1a1b956cc3e90a592e0c96f4be27729953ae86cbec78d"))) == F256::from_be(&b32("6d68ed516cae8082bb7306754ddaf7631be66b9c93ba0584b8692b88a06115d3")));
        assert!(F256::from_be(&b32("6b42e4dfbbec00df93b4b54ad55334f01478e630e37f8c17f47ef783181338b2")).mul(&F256::from_be(&b32("09bb26459a015082b547466f87085beefbd3803355dd4ad1e58134ab91e1d843"))) == F256::from_be(&b32("6767e07b7eaced43ec41d61e4a1ec3e302cb7939afe227c05c9478ee0889e472")));
        assert!(F256::from_be(&b32("67e5d6f12ec40974863a01607135410ef2271ff868875f5ecd24e752a88d396a")).mul(&F256::from_be(&b32("84a54df8966f885ca6dadf4c54ca67c7847e9b78e6a44d378e328c221b4d3eec"))) == F256::from_be(&b32("1b6f1c5bd11aaccb7bf3b38cd5d13f3d4781755882f94981fd288ebb50c17257")));
        assert!(F256::from_be(&b32("16fbdaae657a126219a8c70f8488d965a7c3e11d30ab9d3b893c9528f9403532")).mul(&F256::from_be(&b32("128d1e33f2483f63db08ed62a337cebd201a7d6a834b8307b218a7545164bef8"))) == F256::from_be(&b32("25c9ddbf9ee629eef0655927ce39e901c7d062ec9fec92cd099666941dedf84e")));
        assert!(F256::from_be(&b32("7b5f81579d281b58d581645ab9e24600f62b3c105491e0a5a4efae38a43e5342")).mul(&F256::from_be(&b32("9a4bff94e980a35b10510c26138b7e81c52bce75decbdc73d91ace2b4317c68e"))) == F256::from_be(&b32("eaf017604adf940d133685467983497fe78e97f0d8d1787f0eaefe8d4a4c0efe")));
        assert!(F256::from_be(&b32("97b9c222094d962f5a5b107f02b0de54e1b5c6ec9c7b5cbfc7c02287735e6bc2")).mul(&F256::from_be(&b32("8d312ee6b3706113722799e3aa07fb38ce58f1451ba07080375f35819c354a30"))) == F256::from_be(&b32("5426599502f369c76d3274c4cd2d1b60b39725880ed03a37e68334c0df2d08f3")));
        assert!(F256::from_be(&b32("ac66d9bed4a0ce1f8093c09c444fc90a0a9f7a7292ca3a61ebd35b9e2dd9821f")).mul(&F256::from_be(&b32("9efb7c7ff0016f8194028edf4f785677666a385ed1ff33a3945db7cbc1170caa"))) == F256::from_be(&b32("f15da2cbaefb63c48bdabff0f36a554d1766b12d4489bf3af6edcda25e4ec03d")));
        assert!(F256::from_be(&b32("57bd1fd88ae24916dd810c8d56604736a4d3b09f92a45aa6a994814d084414ec")).mul(&F256::from_be(&b32("1932240bef89c9793e69257cc543184384c2d3da167f7f1d36679b3bbd2746fd"))) == F256::from_be(&b32("c0a87bcf9ca3c7bf0dd6cff313370780148605a3bdca40ae2e667646a1a323df")));
        assert!(F256::from_be(&b32("4c2fde7ed7a4a2bf0725d2a815898236117c6171044e4fdf4cbd8c4dc6a59cb8")).mul(&F256::from_be(&b32("6e61aa0bd0e394ffa1494c13ae3a5dcd9e78e120586f3e729c63778f8ebd77ff"))) == F256::from_be(&b32("d4bafb4ed0b004d0e2301bb16b27b64a5c3ced733917f2f2c6aa3338c276e463")));
        assert!(F256::from_be(&b32("142608df37d5587e166b198374a9922764107334049ef25dc63be3e89c1483cd")).mul(&F256::from_be(&b32("18543641b2092a515ae538506364b8155a4faa5055adf0b3a5d04fab59329845"))) == F256::from_be(&b32("1e3241cb2dba61c938f19134c43e4a91e7acc33f902c7059cdb4caf5303b1c00")));
        assert!(F256::from_be(&b32("203cfdb2bea1aa2fb499c3fe5bbb1862737f909b2236f5de2116a98407ff4c38")).mul(&F256::from_be(&b32("56f823149e59ee9108177d912895aa8791956b274428bd230fcce24f18afa519"))) == F256::from_be(&b32("c0b53c01d1e27171db627db813fbdee3d4ef171706b4e0da827aa6b83a5f0ea4")));
        assert!(F256::from_be(&b32("f2c4b657ca0d46cd80cdbab6150c15d499273f637e65b5979d23bd908f6b0c42")).mul(&F256::from_be(&b32("ea2117d36ba4341704970612d6bb8d6a5c2daffbac9575c06f75f557c03f2cb5"))) == F256::from_be(&b32("880274d690e87919bfa2c97a38f13aa8d4a6ea327006c723dc17fe35e1738cf9")));
        assert!(F256::from_be(&b32("3ab0c42d93115444dbe146699fdf59cc2a7bbeb88aada60f31258749509b7ad3")).mul(&F256::from_be(&b32("a7b9d67f7fe2c2b8b3755d766ebac30ea001069f2bd59fd56c631a14b568cba4"))) == F256::from_be(&b32("f1c9549398b5236b63c5eb583bea29df90351840cd85691e65a9767015e4870b")));
        assert!(F256::from_be(&b32("62b56b05936213a4b76efed179df9b3baa8e896d8039b93b611d3e1123c03b5a")).mul(&F256::from_be(&b32("f429d12e21a6095b5b9765102495c728427ecf3446e0efdd91e9bb234e205baa"))) == F256::from_be(&b32("df4baf018c0112a42d10d1aef5c56a1e02b8f5d874988360031f0e5bc60d00dd")));
        assert!(F256::from_be(&b32("9ddc6a2042dfc2a1cee5a77dfe16512ddeba7ee9a9c3bc2eb2190b0ffe4b1bf0")).mul(&F256::from_be(&b32("94b63d54a1f3e2719a7c3b2227ace8a42aed5dc84b980a403fb6de3a4fbd6f57"))) == F256::from_be(&b32("ed8601f472c4b49178cbafd036bcd0dd04c55948bec377b5230c8a2285bbb0fb")));
        assert!(F256::from_be(&b32("ddd83b075e63489b6b66a33242ead13c0efd84307498c960bd799f1ef6e211b5")).mul(&F256::from_be(&b32("c99be79cb42dd4e4c7d476f4c9bdf0aca333555d59088ae2e84384410cbea513"))) == F256::from_be(&b32("f9931acfeeea88624a51ab60a4bfb025db07f7139717df6df15c6a9409484c16")));
        assert!(F256::from_be(&b32("79d420f83083bf0a4e78a4879d250fa63280e2150996255881c3402d023eeb0f")).mul(&F256::from_be(&b32("add4dfb3ad24bb2ae1fe5877a6f5b64163495ed4f946da37ad90c530d33558af"))) == F256::from_be(&b32("b7b5ce0baae3eaa273eb027d84028686bc95ffd9a1161c79367dc732e15e193b")));
        assert!(F256::from_be(&b32("626b2c2ce88020fb8fa3b593ecde85e5ff296cbc50f435a84841fe66cd6a8b48")).mul(&F256::from_be(&b32("0e63cfdc84dcdbf9a646aef87212761defd6b8acef90fc8045d7a0426a689bfb"))) == F256::from_be(&b32("c97d6fefea2f1d3c8fe6cec4c76a7c84e6a0969072e672a769ae91c60b2fb641")));
        assert!(F256::from_be(&b32("ca0bb85431ea0b1041c80d1055a2fc469dc4bcc4a1b922369a8dcf5af52a1e3c")).mul(&F256::from_be(&b32("5d5692115d76cefb9f617eb36729d3641844315f50b80e4ff7cf5f95f2cce806"))) == F256::from_be(&b32("fb3ea8a8af8fe1a24722ab8688a9ae4e30031b33ce0ba5eaaeef7a2ee68fc601")));
        assert!(F256::from_be(&b32("4824ac7facc00b9e68988292f1096ea5d2da47e3638822624be5d5f16d552480")).mul(&F256::from_be(&b32("917f2cfa744c88492b64979d9b89518bf516d7f6eeac44a8b6824a4441c6a916"))) == F256::from_be(&b32("bda50c792116366e602198f62be8522ef373b70efe78e1c895a3f59031269f6b")));
        assert!(F256::from_be(&b32("0fed68172eddad25dd1da698efc0ae42b01ed188822847dc336e93205be43972")).mul(&F256::from_be(&b32("db3f2f57d57ee431a1e390ceeb1c2fa195fae7e3744882f6916797356087a94a"))) == F256::from_be(&b32("2b8f1a695df773d01e591004e501f248586d9c203050539b2e8735aaccc4cc69")));
        assert!(F256::from_be(&b32("5f3cfb93ecde16ad4df190c5602409393caf046f21f3fb2ce712204fc78992cd")).mul(&F256::from_be(&b32("8e24c3ca539631fb392dd754410b90b3eaec16699beffce5ee2596b2693a6e81"))) == F256::from_be(&b32("c4e8d3c8be2d5f151c5fd03ab03055418423b76c1cf5d14a720a02c30ce967c6")));
        assert!(F256::from_be(&b32("bee25412a01a437e1fb5bda3a99ed464e432e7795053c1e4e3827c7a71273b49")).mul(&F256::from_be(&b32("6787600e2d2133f2403be3d98f64d746d2d66b4db86a5d1e2a71b678cf785cf5"))) == F256::from_be(&b32("dd4d0e3396c16b3db9f8c54029621e41755737a4943588793885449177c8100b")));
        assert!(F256::from_be(&b32("85d48ec62a01d5631744cab6735e25fd46d6a526c27440e842a9fd7373145dfe")).mul(&F256::from_be(&b32("0c3faeb4d8601b20b0f69339aa52b6d5e00676de7d81d858dbcdbfaf74a84001"))) == F256::from_be(&b32("8173d590b95ea321619bfcb16ea2dc7584c2beff5a0c44a8a846e3122afb2cb7")));
        assert!(F256::from_be(&b32("69446a3021f52f1a61055060363e81acc04bd7144d0cb438a3b7f9040ed9549b")).mul(&F256::from_be(&b32("d897293cfa136d53271752143078246012396bf3944a7b4a9e77401af012b04a"))) == F256::from_be(&b32("eafe75f9aa80f36ac5480a4d2c0507137b960b4e3a9f5b90a830b40f17f2ab2b")));
        assert!(F256::from_be(&b32("89e300ce9453f3b42e4083754a9f2725dff2efe65f75c5584a200896dec6c66f")).mul(&F256::from_be(&b32("942616949367cdf03de1f727f1ad42470d65c4e478389323c640f5e86f8d5a92"))) == F256::from_be(&b32("518d5caad31a01681cab52c458197582ab333c519d967f4c79d15cc9247e5c0d")));
        assert!(F256::from_be(&b32("28afb6f2fecb2550cfb35ed818927b57641671b1f5b74f707a3b7bedc40dc5ca")).mul(&F256::from_be(&b32("fbd80db82585cff418f2ede91d68f4710ddadb49dbabf1809b8b45aff9024ada"))) == F256::from_be(&b32("f55c9df3df993b8577f1024b4d389ca9b16eb32ffdd98e54c3e8e478b0536809")));
        assert!(F256::from_be(&b32("8ff53ae3e0a6acb01af64c8dddc233225ed17f85789f90ea1fc24e0621314325")).mul(&F256::from_be(&b32("60f33d3950485f512287073d34bcd132fe32c44bc784ddab14cf4a6e0561fdbd"))) == F256::from_be(&b32("17d66ca12a95744f8e8f8b5d7f73e09c5bd9d5b28fe2b55640a3b6a0be6856b8")));
        assert!(F256::from_be(&b32("cc9ae9dbfb1e9e69af9a6db46c7d9ad5baacf2c1e82112399cea7fad41a22556")).mul(&F256::from_be(&b32("0c4ab804504cc89b1055f6823afb75ec72f86a405c119de4a24ea9464b6a47a6"))) == F256::from_be(&b32("22aae707329cf3f77c6f07885953da345054a50e03c3caf47f9e35231bee5cdb")));
        assert!(F256::from_be(&b32("5ffc248a875028ec66cff0adbded641ea1772ff84cfa27fbb192e9b8347fa7c0")).mul(&F256::from_be(&b32("f20e1e3941bbcbbe4e1b517ebb8a25617e6be50686949bc3aa25d29dfc00491c"))) == F256::from_be(&b32("8986e2232e4112b12dff3b6f74150dcbf6e5cfaa14cc1e35f596bcbfd3fd66d1")));
        assert!(F256::from_be(&b32("0556e8a49bc99c53513b583a2f61adfb2a9234f2af459fc9cef55e9af4ce7734")).mul(&F256::from_be(&b32("5c6a61869c75d7a6541e0e85835c33f0c7227eaa57c54ee9cffe004d0a1a194a"))) == F256::from_be(&b32("1b496ad0abe573dedbae7c0ad8d1f569f546c6179fe0bb9cf0b6861ece42bfe9")));
        assert!(F256::from_be(&b32("4b79241ca95835f2c20156fc14bdb3746ff5484b1f7d828dc29b8299746548d3")).mul(&F256::from_be(&b32("22eb285b52dd1bc3b5e9ff6c1b11605dd0a1bc5784fb833b6add2705a29edf49"))) == F256::from_be(&b32("ffc7d77926a73b2d39ec04c0b4afb70896aa712eef9aafb8005b4df96f2b27cf")));
    }

    #[test]
    fn g_on_curve() {
        let (gx, gy) = generator();
        assert!(on_curve(&gx, &gy));
    }

    #[test]
    fn mult_one_is_g() {
        let (gx, gy) = generator();
        let mut k = [0u8; 32];
        k[31] = 1;
        let (x, y) = scalar_mult_be(&k, &gx, &gy).expect("1*G");
        assert!(x.to_be() == b32("6b17d1f2e12c4247f8bce6e563a440f277037d812deb33a0f4a13945d898c296"));
        assert!(y.to_be() == b32("4fe342e2fe1a7f9b8ee7eb4a7c0f9e162bce33576b315ececbb6406837bf51f5"));
    }

    #[test]
    fn mult_two_is_2g() {
        // node createECDH prime256v1 ground truth.
        let (gx, gy) = generator();
        let mut k = [0u8; 32];
        k[31] = 2;
        let (x, y) = scalar_mult_be(&k, &gx, &gy).expect("2*G");
        assert!(x.to_be() == b32("7cf27b188d034f7e8a52380304b51ac3c08969e277f21b35a60b48fc47669978"));
        assert!(y.to_be() == b32("07775510db8ed040293d9ac69f7430dbba7dade63ce982299e04b79d227873d1"));
    }

    #[test]
    fn add_jj_paths() {        // G+G via full Jacobian add == dbl(G) == 2G (H==0 doubling branch).
        use super::J;
        let (gx, gy) = generator();
        let g = J::from_affine(gx, gy);
        let d = g.add_jj(&g);
        let (x, y) = d.to_affine().expect("2G");
        assert!(x.to_be() == b32("7cf27b188d034f7e8a52380304b51ac3c08969e277f21b35a60b48fc47669978"));
        assert!(y.to_be() == b32("07775510db8ed040293d9ac69f7430dbba7dade63ce982299e04b79d227873d1"));
        // 2G+G via full add == 3G (general H!=0 branch).
        let g2 = J::from_affine(
            F256::from_be(&b32("7cf27b188d034f7e8a52380304b51ac3c08969e277f21b35a60b48fc47669978")),
            F256::from_be(&b32("07775510db8ed040293d9ac69f7430dbba7dade63ce982299e04b79d227873d1")),
        );
        let t = g2.add_jj(&g);
        let (tx, ty) = t.to_affine().expect("3G");
        assert!(tx.to_be() == b32("5ecbe4d1a6330a44c8f7ef951d4bf165e6c6b721efada985fb41661bc6e7fd6c"));
        assert!(ty.to_be() == b32("8734640c4998ff7e374b06ce1a64a2ecd82ab036384fb83d9a79b127a27d5032"));
    }

    #[test]
    fn add_jj_nontrivial_z() {        // dbl(G) has z = 2*Gy != 1: add_jj(dbl(G), G) == 3G exercises the
        // general-z H!=0 path (the z==1 cases above can't catch z bugs).
        use super::J;
        let (gx, gy) = generator();
        let g = J::from_affine(gx, gy);
        let g2 = g.dbl();
        assert!(!g2.z.is_zero());
        let t = g2.add_jj(&g);
        let (tx, ty) = t.to_affine().expect("3G");
        assert!(tx.to_be() == b32("5ecbe4d1a6330a44c8f7ef951d4bf165e6c6b721efada985fb41661bc6e7fd6c"));
        assert!(ty.to_be() == b32("8734640c4998ff7e374b06ce1a64a2ecd82ab036384fb83d9a79b127a27d5032"));
    }

    #[test]
    fn add_jj_both_nontrivial_z() {        // Both inputs with z != 1 (the verify path: u1*G + u2*Q).
        // 4G + 2G == 6G == dbl(3G); 2G + 2G == 4G via the H==0 branch.
        use super::J;
        let (gx, gy) = generator();
        let g = J::from_affine(gx, gy);
        let g2 = g.dbl();
        let g4 = g2.add_jj(&g2);
        let g6a = g4.add_jj(&g2);
        let mut k = [0u8; 32];
        k[31] = 3;
        let (x3, y3) = super::scalar_mult_be(&k, &gx, &gy).expect("3G");
        let g6b = J::from_affine(x3, y3).dbl();
        let (ax, ay) = g6a.to_affine().expect("6G");
        let (bx, by) = g6b.to_affine().expect("6G");
        assert!(ax.to_be() == bx.to_be());
        assert!(ay.to_be() == by.to_be());
    }

    #[test]
    #[test]
    fn ecdh_vectors() {
        // node createECDH ground truth (Sab == Sba there).
        let (gx, gy) = generator();
        let ka = b32("c0ffee0000000000000000000000000000000000000000000000000000000001");
        let kb = b32("deadbeef00000000000000000000000000000000000000000000000000000001");
        let (pax, pay) = scalar_mult_be(&ka, &gx, &gy).expect("PA");
        assert!(pax.to_be() == b32("19962c1db43584d71435d1b44224dd62a00c0359c0a53875179e71ad38eaf6d8"));
        assert!(pay.to_be() == b32("e2dc1be2ece6323dc4dd5fe6ce7dcebf7d03d3671032cb20dc9f84ef5c7d652b"));
        let (pbx, pby) = scalar_mult_be(&kb, &gx, &gy).expect("PB");
        assert!(pbx.to_be() == b32("13d541e61978d54dd7b1c11250416f85dce7fc2bb178d05e7914c74185b34103"));
        assert!(pby.to_be() == b32("9e2c5cdabd2b2e8fd0f60a9ac9ecfc1cce7dbdc309f99ac5428d5295af90ad87"));
        let (sx, _) = scalar_mult_be(&ka, &pbx, &pby).expect("Sab");
        let (sy, _) = scalar_mult_be(&kb, &pax, &pay).expect("Sba");
        let want = b32("9780873b1dd059b0dc379841bc4d8d65350125debc77b9b87c34d5d64ae10531");
        assert!(sx.to_be() == want);
        assert!(sy.to_be() == want);
    }
    #[test]
    fn mult_three_is_3g() {
        // node createECDH prime256v1 ground truth for priv=3 (00..03).
        // Exercises add_mixed with a non-INF accumulator (dbl alone
        // already proven by mult_two_is_2g).
        let (gx, gy) = generator();
        let mut k = [0u8; 32];
        k[31] = 3;
        let (x, y) = scalar_mult_be(&k, &gx, &gy).expect("3*G");
        assert!(x.to_be() == b32("5ecbe4d1a6330a44c8f7ef951d4bf165e6c6b721efada985fb41661bc6e7fd6c"));
        assert!(y.to_be() == b32("8734640c4998ff7e374b06ce1a64a2ecd82ab036384fb83d9a79b127a27d5032"));
    }
}
