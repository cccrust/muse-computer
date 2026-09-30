// v3.14: AES-128 block ENCRYPT only (FIPS 197).
// GCM/CTR mode never needs the inverse cipher, so decrypt is omitted on
// purpose (smaller audit surface; wrong-direction use fails closed).

const SBOX: [u8; 256] = [
    0x63, 0x7c, 0x77, 0x7b, 0xf2, 0x6b, 0x6f, 0xc5, 0x30, 0x01, 0x67, 0x2b, 0xfe, 0xd7, 0xab, 0x76,
    0xca, 0x82, 0xc9, 0x7d, 0xfa, 0x59, 0x47, 0xf0, 0xad, 0xd4, 0xa2, 0xaf, 0x9c, 0xa4, 0x72, 0xc0,
    0xb7, 0xfd, 0x93, 0x26, 0x36, 0x3f, 0xf7, 0xcc, 0x34, 0xa5, 0xe5, 0xf1, 0x71, 0xd8, 0x31, 0x15,
    0x04, 0xc7, 0x23, 0xc3, 0x18, 0x96, 0x05, 0x9a, 0x07, 0x12, 0x80, 0xe2, 0xeb, 0x27, 0xb2, 0x75,
    0x09, 0x83, 0x2c, 0x1a, 0x1b, 0x6e, 0x5a, 0xa0, 0x52, 0x3b, 0xd6, 0xb3, 0x29, 0xe3, 0x2f, 0x84,
    0x53, 0xd1, 0x00, 0xed, 0x20, 0xfc, 0xb1, 0x5b, 0x6a, 0xcb, 0xbe, 0x39, 0x4a, 0x4c, 0x58, 0xcf,
    0xd0, 0xef, 0xaa, 0xfb, 0x43, 0x4d, 0x33, 0x85, 0x45, 0xf9, 0x02, 0x7f, 0x50, 0x3c, 0x9f, 0xa8,
    0x51, 0xa3, 0x40, 0x8f, 0x92, 0x9d, 0x38, 0xf5, 0xbc, 0xb6, 0xda, 0x21, 0x10, 0xff, 0xf3, 0xd2,
    0xcd, 0x0c, 0x13, 0xec, 0x5f, 0x97, 0x44, 0x17, 0xc4, 0xa7, 0x7e, 0x3d, 0x64, 0x5d, 0x19, 0x73,
    0x60, 0x81, 0x4f, 0xdc, 0x22, 0x2a, 0x90, 0x88, 0x46, 0xee, 0xb8, 0x14, 0xde, 0x5e, 0x0b, 0xdb,
    0xe0, 0x32, 0x3a, 0x0a, 0x49, 0x06, 0x24, 0x5c, 0xc2, 0xd3, 0xac, 0x62, 0x91, 0x95, 0xe4, 0x79,
    0xe7, 0xc8, 0x37, 0x6d, 0x8d, 0xd5, 0x4e, 0xa9, 0x6c, 0x56, 0xf4, 0xea, 0x65, 0x7a, 0xae, 0x08,
    0xba, 0x78, 0x25, 0x2e, 0x1c, 0xa6, 0xb4, 0xc6, 0xe8, 0xdd, 0x74, 0x1f, 0x4b, 0xbd, 0x8b, 0x8a,
    0x70, 0x3e, 0xb5, 0x66, 0x48, 0x03, 0xf6, 0x0e, 0x61, 0x35, 0x57, 0xb9, 0x86, 0xc1, 0x1d, 0x9e,
    0xe1, 0xf8, 0x98, 0x11, 0x69, 0xd9, 0x8e, 0x94, 0x9b, 0x1e, 0x87, 0xe9, 0xce, 0x55, 0x28, 0xdf,
    0x8c, 0xa1, 0x89, 0x0d, 0xbf, 0xe6, 0x42, 0x68, 0x41, 0x99, 0x2d, 0x0f, 0xb0, 0x54, 0xbb, 0x16,
];

const RCON: [u32; 10] = [
    0x0100_0000, 0x0200_0000, 0x0400_0000, 0x0800_0000, 0x1000_0000,
    0x2000_0000, 0x4000_0000, 0x8000_0000, 0x1b00_0000, 0x3600_0000,
];

/// Expanded AES-128 key: 11 round keys of 16 bytes.
pub struct Key128 {
    rk: [[u8; 16]; 11],
}

impl Key128 {
    pub fn expand(key: &[u8; 16]) -> Self {
        let mut w = [0u32; 44];
        let mut i = 0;
        while i < 4 {
            w[i] = ((key[4 * i] as u32) << 24)
                | ((key[4 * i + 1] as u32) << 16)
                | ((key[4 * i + 2] as u32) << 8)
                | (key[4 * i + 3] as u32);
            i += 1;
        }
        while i < 44 {
            let mut t = w[i - 1];
            if i % 4 == 0 {
                // RotWord + SubWord + Rcon
                t = ((SBOX[((t >> 16) & 0xff) as usize] as u32) << 24)
                    | ((SBOX[((t >> 8) & 0xff) as usize] as u32) << 16)
                    | ((SBOX[(t & 0xff) as usize] as u32) << 8)
                    | (SBOX[((t >> 24) & 0xff) as usize] as u32);
                t ^= RCON[i / 4 - 1];
            }
            w[i] = w[i - 4] ^ t;
            i += 1;
        }
        let mut rk = [[0u8; 16]; 11];
        let mut r = 0;
        while r < 11 {
            let mut c = 0;
            while c < 4 {
                let v = w[r * 4 + c];
                rk[r][4 * c] = (v >> 24) as u8;
                rk[r][4 * c + 1] = (v >> 16) as u8;
                rk[r][4 * c + 2] = (v >> 8) as u8;
                rk[r][4 * c + 3] = v as u8;
                c += 1;
            }
            r += 1;
        }
        Self { rk }
    }

    /// One block encrypt. State is column-major: state[col*4+row].
    pub fn enc_block(&self, input: &[u8; 16], out: &mut [u8; 16]) {
        let mut s = *input;
        add_round_key(&mut s, &self.rk[0]);
        let mut round = 1;
        while round < 10 {
            sub_bytes(&mut s);
            shift_rows(&mut s);
            mix_columns(&mut s);
            add_round_key(&mut s, &self.rk[round]);
            round += 1;
        }
        sub_bytes(&mut s);
        shift_rows(&mut s);
        add_round_key(&mut s, &self.rk[10]);
        *out = s;
    }
}

fn add_round_key(s: &mut [u8; 16], rk: &[u8; 16]) {
    let mut i = 0;
    while i < 16 {
        s[i] ^= rk[i];
        i += 1;
    }
}

fn sub_bytes(s: &mut [u8; 16]) {
    let mut i = 0;
    while i < 16 {
        s[i] = SBOX[s[i] as usize];
        i += 1;
    }
}

fn shift_rows(s: &mut [u8; 16]) {
    // row r rotates left by r (columns are 4 apart)
    let t = *s;
    let mut r = 1;
    while r < 4 {
        let mut c = 0;
        while c < 4 {
            s[c * 4 + r] = t[((c + r) % 4) * 4 + r];
            c += 1;
        }
        r += 1;
    }
}

fn xtime(x: u8) -> u8 {
    if x & 0x80 != 0 {
        (x << 1) ^ 0x1b
    } else {
        x << 1
    }
}

fn mix_columns(s: &mut [u8; 16]) {
    let mut c = 0;
    while c < 4 {
        let a0 = s[c * 4];
        let a1 = s[c * 4 + 1];
        let a2 = s[c * 4 + 2];
        let a3 = s[c * 4 + 3];
        let b0 = xtime(a0) ^ xtime(a1) ^ a1 ^ a2 ^ a3;
        let b1 = a0 ^ xtime(a1) ^ xtime(a2) ^ a2 ^ a3;
        let b2 = a0 ^ a1 ^ xtime(a2) ^ xtime(a3) ^ a3;
        let b3 = xtime(a0) ^ a0 ^ a1 ^ a2 ^ xtime(a3);
        s[c * 4] = b0;
        s[c * 4 + 1] = b1;
        s[c * 4 + 2] = b2;
        s[c * 4 + 3] = b3;
        c += 1;
    }
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
    fn fips197_appendix_b() {
        // key 000102..0f, pt 001122..ff -> 69c4e0d86a7b0430d8cdb78070b4c55a
        let kb = hex("000102030405060708090a0b0c0d0e0f");
        let pb = hex("00112233445566778899aabbccddeeff");
        let mut k = [0u8; 16];
        let mut p = [0u8; 16];
        k.copy_from_slice(&kb);
        p.copy_from_slice(&pb);
        let key = Key128::expand(&k);
        let mut out = [0u8; 16];
        key.enc_block(&p, &mut out);
        assert_eq!(
            out,
            hex("69c4e0d86a7b0430d8cdb78070b4c55a").as_slice()
        );
    }

    #[test]
    fn zero_key_zero_block() {
        let key = Key128::expand(&[0u8; 16]);
        let mut out = [0u8; 16];
        key.enc_block(&[0u8; 16], &mut out);
        assert_eq!(
            out,
            hex("66e94bd4ef8a2c3b884cfa59ca342b2e").as_slice()
        );
    }
}
