// v3.14: AES-128-GCM, 96-bit IV, 128-bit tag only (NIST SP 800-38D).
// TLS 1.3 uses exactly this shape (12-byte nonce from the sequence XOR,
// 16-byte tag), so no other IV/tag lengths are accepted.

use super::aes128::Key128;

/// GF(2^128) multiply (GCM "shift right" variant, R = 0xe1 || 0^120).
/// u[2] is big-endian: u[0] = high 64 bits.
fn gf_mul(x: [u64; 2], y: [u64; 2]) -> [u64; 2] {
    let mut z = [0u64; 2];
    let mut v = y;
    // bit i of x, MSB first: word = i/64, bit = 63 - i%64
    let mut i = 0;
    while i < 128 {
        let word = x[(i / 64) as usize];
        if (word >> (63 - (i % 64))) & 1 != 0 {
            z[0] ^= v[0];
            z[1] ^= v[1];
        }
        let lsb = v[1] & 1;
        v[1] = (v[1] >> 1) | ((v[0] & 1) << 63);
        v[0] >>= 1;
        if lsb != 0 {
            v[0] ^= 0xe100_0000_0000_0000;
        }
        i += 1;
    }
    z
}

fn load_be(b: &[u8]) -> [u64; 2] {
    let mut o = [0u64; 2];
    let mut i = 0;
    while i < 8 {
        o[0] = (o[0] << 8) | b[i] as u64;
        o[1] = (o[1] << 8) | b[8 + i] as u64;
        i += 1;
    }
    o
}

fn store_be(v: [u64; 2], out: &mut [u8; 16]) {
    let mut i = 0;
    while i < 8 {
        out[i] = (v[0] >> (56 - i * 8)) as u8;
        out[8 + i] = (v[1] >> (56 - i * 8)) as u8;
        i += 1;
    }
}

fn ghash(h: [u64; 2], aad: &[u8], ct: &[u8]) -> [u8; 16] {
    let mut x = [0u64; 2];
    for chunk in aad.chunks(16) {
        let mut b = [0u8; 16];
        b[..chunk.len()].copy_from_slice(chunk);
        let v = load_be(&b);
        x[0] ^= v[0];
        x[1] ^= v[1];
        x = gf_mul(x, h);
    }
    for chunk in ct.chunks(16) {
        let mut b = [0u8; 16];
        b[..chunk.len()].copy_from_slice(chunk);
        let v = load_be(&b);
        x[0] ^= v[0];
        x[1] ^= v[1];
        x = gf_mul(x, h);
    }
    let mut lb = [0u8; 16];
    let (al, cl) = (aad.len() as u64 * 8, ct.len() as u64 * 8);
    let mut i = 0;
    while i < 8 {
        lb[i] = (al >> (56 - i * 8)) as u8;
        lb[8 + i] = (cl >> (56 - i * 8)) as u8;
        i += 1;
    }
    let v = load_be(&lb);
    x[0] ^= v[0];
    x[1] ^= v[1];
    x = gf_mul(x, h);
    let mut out = [0u8; 16];
    store_be(x, &mut out);
    out
}

fn inc32(b: &mut [u8; 16]) {
    let mut v = ((b[12] as u32) << 24) | ((b[13] as u32) << 16) | ((b[14] as u32) << 8) | b[15] as u32;
    v = v.wrapping_add(1);
    b[12] = (v >> 24) as u8;
    b[13] = (v >> 16) as u8;
    b[14] = (v >> 8) as u8;
    b[15] = v as u8;
}

/// Encrypt in place (`data` is plaintext in / ciphertext out).
/// Returns false when `data` exceeds the 2^32-2 block cap (never on guest).
pub fn encrypt(
    key: &Key128,
    iv12: &[u8; 12],
    aad: &[u8],
    data: &mut [u8],
    tag: &mut [u8; 16],
) -> bool {
    if data.len() / 16 + 1 >= (1u64 << 32) as usize - 1 {
        return false;
    }
    let mut h_in = [0u8; 16];
    key.enc_block(&[0u8; 16], &mut h_in);
    let h = load_be(&h_in);
    // J0 = IV || 0^31 || 1
    let mut j = [0u8; 16];
    j[..12].copy_from_slice(iv12);
    j[15] = 1;
    // CTR from inc32(J0)
    let mut off = 0;
    while off < data.len() {
        inc32(&mut j);
        let mut e = [0u8; 16];
        key.enc_block(&j, &mut e);
        let take = (data.len() - off).min(16);
        let mut k = 0;
        while k < take {
            data[off + k] ^= e[k];
            k += 1;
        }
        off += take;
    }
    let s = ghash(h, aad, data);
    // T = AES(J0) xor S
    let mut j0 = [0u8; 16];
    j0[..12].copy_from_slice(iv12);
    j0[15] = 1;
    let mut e0 = [0u8; 16];
    key.enc_block(&j0, &mut e0);
    let mut i = 0;
    while i < 16 {
        tag[i] = e0[i] ^ s[i];
        i += 1;
    }
    true
}

/// Verify tag then decrypt in place. Returns tag-ok (false = `data`
/// left as-is, caller must drop it).
pub fn decrypt(
    key: &Key128,
    iv12: &[u8; 12],
    aad: &[u8],
    data: &mut [u8],
    tag: &[u8; 16],
) -> bool {
    if data.len() / 16 + 1 >= (1u64 << 32) as usize - 1 {
        return false;
    }
    let mut h_in = [0u8; 16];
    key.enc_block(&[0u8; 16], &mut h_in);
    let h = load_be(&h_in);
    let s = ghash(h, aad, data);
    let mut j0 = [0u8; 16];
    j0[..12].copy_from_slice(iv12);
    j0[15] = 1;
    let mut e0 = [0u8; 16];
    key.enc_block(&j0, &mut e0);
    let mut diff = 0u8;
    let mut i = 0;
    while i < 16 {
        diff |= e0[i] ^ s[i] ^ tag[i];
        i += 1;
    }
    if diff != 0 {
        return false;
    }
    // CTR decrypt (= encrypt)
    let mut j = j0;
    let mut off = 0;
    while off < data.len() {
        inc32(&mut j);
        let mut e = [0u8; 16];
        key.enc_block(&j, &mut e);
        let take = (data.len() - off).min(16);
        let mut k = 0;
        while k < take {
            data[off + k] ^= e[k];
            k += 1;
        }
        off += take;
    }
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

    /// McGrew-Viega case 1: K=0, P empty, IV=0, AAD empty.
    /// (Ground truth: host OpenSSL/node crypto, not memory.)
    #[test]
    fn gcm_empty() {
        let key = Key128::expand(&[0u8; 16]);
        let mut data = [0u8; 0];
        let mut tag = [0u8; 16];
        assert!(encrypt(&key, &[0u8; 12], &[], &mut data, &mut tag));
        assert_eq!(
            tag,
            hex("58e2fccefa7e3061367f1d57a4e7455a").as_slice()
        );
    }

    /// McGrew-Viega case 2: K=0, P=16 zero bytes.
    #[test]
    fn gcm_zeros16() {
        let key = Key128::expand(&[0u8; 16]);
        let mut data = [0u8; 16];
        let mut tag = [0u8; 16];
        assert!(encrypt(&key, &[0u8; 12], &[], &mut data, &mut tag));
        assert_eq!(
            data,
            hex("0388dace60b6a392f328c2b971b2fe78").as_slice()
        );
        assert_eq!(
            tag,
            hex("ab6e47d42cec13bdf53a67b21257bddf").as_slice()
        );
        // round-trip + tamper rejection
        assert!(decrypt(&key, &[0u8; 12], &[], &mut data, &tag));
        assert_eq!(data, [0u8; 16]);
        let mut bad = tag;
        bad[0] ^= 1;
        let mut data2 = hex("0388dace60b6a392f328c2b971b2fe78");
        assert!(!decrypt(&key, &[0u8; 12], &[], &mut data2, &bad));
    }

    /// AAD + multi-block known answer (host OpenSSL/node crypto ground
    /// truth; key feffe9../iv cafebabe../aad feedface../pt 00..3b).
    #[test]
    fn gcm_aad_multiblock_kat() {
        let kb = hex("feffe9928665731c6d6a8f9467308308");
        let mut k = [0u8; 16];
        k.copy_from_slice(&kb);
        let key = Key128::expand(&k);
        let ivb = hex("cafebabefacedbaddecaf888");
        let mut iv = [0u8; 12];
        iv.copy_from_slice(&ivb);
        let aad = hex("feedfacedeadbeeffeedfacedeadbeefabaddad2");
        let mut pt = [0u8; 60];
        let mut i = 0;
        while i < 60 {
            pt[i] = i as u8;
            i += 1;
        }
        let mut tag = [0u8; 16];
        assert!(encrypt(&key, &iv, &aad, &mut pt, &mut tag));
        assert_eq!(
            pt,
            hex("9bb32ee4ddf674c6e62222792728fc09751c9a6f2d23452d03945405bf8035431dc83a04e52bbc687a694e55c90f310f9af8d4fff4327cf7bf02a193")
                .as_slice()
        );
        assert_eq!(
            tag,
            hex("c7d70645aa3f267a0eeb0aa0e5fbf451").as_slice()
        );
        assert!(decrypt(&key, &iv, &aad, &mut pt, &tag));
        let mut want = [0u8; 60];
        i = 0;
        while i < 60 {
            want[i] = i as u8;
            i += 1;
        }
        assert_eq!(pt, want);
    }

    /// Non-multiple plaintext round-trips (partial-block path).
    #[test]
    fn gcm_aad_partial_roundtrip() {
        let kb = hex("feffe9928665731c6d6a8f9467308308");
        let mut k = [0u8; 16];
        k.copy_from_slice(&kb);
        let key = Key128::expand(&k);
        let ivb = hex("cafebabefacedbaddecaf888");
        let mut iv = [0u8; 12];
        iv.copy_from_slice(&ivb);
        let aadb = hex("feedfacedeadbeeffeedfacedeadbeefabaddad2");
        let mut pt = [0u8; 60];
        let mut i = 0;
        while i < 60 {
            pt[i] = (i * 7 + 3) as u8;
            i += 1;
        }
        let want = pt;
        let mut tag = [0u8; 16];
        assert!(encrypt(&key, &iv, &aadb, &mut pt, &mut tag));
        assert_ne!(pt, want);
        assert!(decrypt(&key, &iv, &aadb, &mut pt, &tag));
        assert_eq!(pt, want);
    }
}
