// v3.14: TLS 1.3 client, single suite (TLS_AES_128_GCM_SHA256 + x25519
// ECDHE + ecdsa_secp256r1_sha256 server auth). Transport-agnostic PURE
// functions over caller buffers: the driver (wgets) owns sockets,
// WouldBlock retries, and staging. Covers exactly our registry's flight:
// ServerHello, EncryptedExtensions, Certificate (first cert used),
// CertificateVerify, Finished. No HRR (we always offer an x25519 share
// the server accepts), no resumption, no alerts, no SNI (IP-literal
// server; SAN-IP verification lives in x509).
//
// Transcript rule (RFC 8446 §4.4.1): hash the handshake FRAGMENTS
// (4-byte header + body), never the 5-byte record headers. Use
// `frag_of(record)` to strip. Sequence numbers: independent per
// direction AND per key phase (RFC 8446 §5.3: each key phase restarts
// at 0 -- hs records use 0,1,2.., app records restart at 0). Client
// Finished is sealed under the client HANDSHAKE key (RFC 8446 §4.4.4);
// application data under the application keys.

use super::aes128::Key128;
use super::gcm;
use super::n256::ecdsa_verify_raw;
use super::{hkdf_expand, hkdf_extract, hmac_sha256, Sha256};

pub const REC_HANDSHAKE: u8 = 22;
pub const REC_APPDATA: u8 = 23;
pub const HS_CLIENT_HELLO: u8 = 1;
pub const HS_SERVER_HELLO: u8 = 2;
pub const HS_ENCRYPTED_EXT: u8 = 8;
pub const HS_CERT: u8 = 11;
pub const HS_CERT_VERIFY: u8 = 15;
pub const HS_FINISHED: u8 = 20;
pub const INNER_HANDSHAKE: u8 = 22;
pub const INNER_APPDATA: u8 = 23;

const CS_AES128_GCM_SHA256: u16 = 0x1301;
const GROUP_X25519: u16 = 0x001d;
const SIG_ECDSA_P256_SHA256: u16 = 0x0403;

/// Strip the 5-byte record header. None on short/garbage input.
pub fn frag_of(record: &[u8]) -> Option<&[u8]> {
    if record.len() < 5 || record[0] != REC_HANDSHAKE {
        return None;
    }
    let n = ((record[3] as usize) << 8) | record[4] as usize;
    if record.len() < 5 + n {
        return None;
    }
    Some(&record[5..5 + n])
}

/// Build a ClientHello TLSPlaintext record.
/// `random` (32B) and `eph_pub` (x25519, 32B) come from getrandom +
/// x25519(priv, basepoint) in the driver. Returns total record bytes.
/// Layout: record(5) + frag-hdr(4) + body(108) = 117 bytes.
pub fn build_client_hello(random: &[u8; 32], eph_pub: &[u8; 32], out: &mut [u8]) -> Option<usize> {
    // extensions: SV(7) + SG(8) + SA(8) + KS(42) = 65 bytes (see below).
    const EXTN: usize = 65;
    const BODY: usize = 2 + 32 + 1 + 4 + 2 + 2 + EXTN; // 108
    const TOTAL: usize = 5 + 4 + BODY; // 117
    if out.len() < TOTAL {
        return None;
    }
    struct W<'a> {
        b: &'a mut [u8],
        p: usize,
    }
    impl W<'_> {
        fn w(&mut self, v: u8) {
            self.b[self.p] = v;
            self.p += 1;
        }
        fn w16(&mut self, v: u16) {
            let a = v;
            self.w((a >> 8) as u8);
            self.w(a as u8);
        }
    }
    let mut o = W { b: out, p: 0 };
    // record header (frag length filled below)
    o.w(REC_HANDSHAKE);
    o.w(0x03);
    o.w(0x03);
    o.w16((4 + BODY) as u16);
    // fragment header
    o.w(HS_CLIENT_HELLO);
    o.w(0);
    o.w16(BODY as u16);
    // body
    o.w16(0x0303); // legacy_version
    for i in 0..32 {
        o.w(random[i]);
    }
    o.w(0); // session_id: empty
    o.w16(2); // cipher_suites length
    o.w16(CS_AES128_GCM_SHA256);
    o.w(1); // compression_methods length
    o.w(0);
    o.w16(EXTN as u16); // extensions length
    // supported_versions: 002b | 0003 | 02 | 0304
    o.w16(0x002b);
    o.w16(3);
    o.w(2);
    o.w(0x03);
    o.w(0x04);
    // supported_groups: 000a | 0004 | 0002 | 001d
    o.w16(0x000a);
    o.w16(4);
    o.w16(2);
    o.w16(GROUP_X25519);
    // signature_algorithms: 000d | 0006 | 0004... list is [0403]:
    // data = list_len(2) || schemes = 0002 0403 (4 bytes)
    o.w16(0x000d);
    o.w16(4);
    o.w16(2);
    o.w16(SIG_ECDSA_P256_SHA256);
    // key_share: 0033 | len | client_shares_len(0024) | 001d | 0020 | pub
    o.w16(0x0033);
    o.w16(38);
    o.w16(36);
    o.w16(GROUP_X25519);
    o.w16(32);
    for i in 0..32 {
        o.w(eph_pub[i]);
    }
    debug_assert!(o.p == TOTAL);
    if o.p != TOTAL {
        return None;
    }
    Some(TOTAL)
}

/// Parse a ServerHello BODY (fragment header already stripped).
/// Checks version/cipher/supported_versions/key_share, returns the
/// server's 32-byte x25519 share. (Transcript uses the raw bytes, which
/// the caller already holds.)
pub fn parse_server_hello(body: &[u8]) -> Option<[u8; 32]> {
    if body.len() < 38 {
        return None;
    }
    if body[0] != 0x03 || body[1] != 0x03 {
        return None; // legacy_version must be 0303
    }
    let mut p = 2 + 32; // skip random (transcript covers it)
    if p >= body.len() {
        return None;
    }
    let sid_len = body[p] as usize;
    p += 1 + sid_len;
    if p + 2 > body.len() {
        return None;
    }
    if body[p] != 0x13 || body[p + 1] != 0x01 {
        return None; // cipher_suite must be TLS_AES_128_GCM_SHA256
    }
    p += 2;
    if p >= body.len() {
        return None;
    }
    p += 1 + body[p] as usize; // skip compression (legacy 0x00)
    if p + 2 > body.len() {
        return None;
    }
    let extn = ((body[p] as usize) << 8) | body[p + 1] as usize;
    p += 2;
    if p + extn != body.len() {
        return None;
    }
    let end = p + extn;
    let mut share: Option<[u8; 32]> = None;
    let mut saw_versions = false;
    while p + 4 <= end {
        let et = ((body[p] as u16) << 8) | body[p + 1] as u16;
        let el = ((body[p + 2] as usize) << 8) | body[p + 3] as usize;
        p += 4;
        if p + el > end {
            return None;
        }
        let d = &body[p..p + el];
        match et {
            0x002b => {
                // supported_versions: selected_version u16, must be 0304
                if d.len() != 2 || d[0] != 0x03 || d[1] != 0x04 {
                    return None;
                }
                saw_versions = true;
            }
            0x0033 => {
                // key_share: group u16, ke_len u16, ke (server sends one)
                if d.len() != 36 || d[0] != 0x00 || d[1] != 0x1d || d[2] != 0x00 || d[3] != 0x20 {
                    return None;
                }
                let mut s = [0u8; 32];
                s.copy_from_slice(&d[4..36]);
                share = Some(s);
            }
            _ => {} // ignore the rest (our server sends no others)
        }
        p += el;
    }
    if p != end || !saw_versions {
        return None;
    }
    share
}

/// Handshake message iterator over concatenated decrypted fragments.
/// Stops at the first incomplete message (driver reads more).
pub struct HsMsgs<'a> {
    buf: &'a [u8],
}

impl<'a> HsMsgs<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf }
    }
    pub fn next(&mut self) -> Option<(u8, &'a [u8])> {
        if self.buf.len() < 4 {
            return None;
        }
        let n = ((self.buf[1] as usize) << 16) | ((self.buf[2] as usize) << 8) | self.buf[3] as usize;
        if self.buf.len() < 4 + n {
            return None;
        }
        let t = self.buf[0];
        let body = &self.buf[4..4 + n];
        self.buf = &self.buf[4 + n..];
        Some((t, body))
    }
}

/// Parse a Certificate BODY: require empty request context, take the
/// FIRST certificate's DER bytes (our server sends exactly one; extras
/// are ignored -- trust is decided by the root pin, see x509).
pub fn parse_certificate(body: &[u8]) -> Option<&[u8]> {
    if body.is_empty() || body[0] != 0 {
        return None; // cert_request_context must be empty (we never send it)
    }
    let mut p = 1;
    if p + 3 > body.len() {
        return None;
    }
    let list_len = ((body[p] as usize) << 16) | ((body[p + 1] as usize) << 8) | body[p + 2] as usize;
    p += 3;
    if p + list_len != body.len() || list_len < 4 {
        return None;
    }
    let cert_len = ((body[p] as usize) << 16) | ((body[p + 1] as usize) << 8) | body[p + 2] as usize;
    p += 3;
    if cert_len == 0 || p + cert_len > body.len() {
        return None;
    }
    let cert = &body[p..p + cert_len];
    p += cert_len;
    if p + 2 > body.len() {
        return None;
    }
    let ext_len = ((body[p] as usize) << 8) | body[p + 1] as usize;
    p += 2;
    if p + ext_len > body.len() {
        return None;
    }
    // NOTE: trailing entries (further certs) are ignored by design.
    Some(cert)
}

/// Parse + verify CertificateVerify: scheme 0403, DER ECDSA over
/// (64x0x20 || "TLS 1.3, server CertificateVerify" || 0x00 || th_cv).
pub fn check_cert_verify(
    body: &[u8],
    leaf_x: &[u8; 32],
    leaf_y: &[u8; 32],
    th_cv: &[u8; 32],
) -> bool {
    if body.len() < 4 {
        return false;
    }
    if body[0] != 0x04 || body[1] != 0x03 {
        return false; // ecdsa_secp256r1_sha256 only
    }
    let sl = ((body[2] as usize) << 8) | body[3] as usize;
    if 4 + sl != body.len() {
        return false;
    }
    let sig = &body[4..];
    // DER SEQ { r INTEGER, s INTEGER } (minimal parse, same rules as x509)
    if sig.len() < 8 || sig[0] != 0x30 {
        return false;
    }
    let (slen, sp) = match der_len(&sig[1..]) {
        Some(v) => v,
        None => return false,
    };
    if 1 + sp + slen != sig.len() {
        return false;
    }
    let inner = &sig[1 + sp..];
    let (r, rest) = match der_int32(inner) {
        Some(v) => v,
        None => return false,
    };
    let (s, rest2) = match der_int32(rest) {
        Some(v) => v,
        None => return false,
    };
    if !rest2.is_empty() {
        return false;
    }
    // signed content
    let mut h = Sha256::new();
    h.update(&[0x20u8; 64]);
    h.update(b"TLS 1.3, server CertificateVerify");
    h.update(&[0x00]);
    h.update(th_cv);
    let digest = h.finalize();
    ecdsa_verify_raw(leaf_x, leaf_y, &digest, &r, &s)
}

/// DER length prefix: returns (length, header_len).
fn der_len(b: &[u8]) -> Option<(usize, usize)> {
    if b.is_empty() {
        return None;
    }
    match b[0] {
        n if n < 0x80 => Some((n as usize, 1)),
        0x81 => {
            if b.len() < 2 {
                return None;
            }
            Some((b[1] as usize, 2))
        }
        0x82 => {
            if b.len() < 3 {
                return None;
            }
            Some((((b[1] as usize) << 8) | b[2] as usize, 3))
        }
        _ => None,
    }
}

/// DER INTEGER -> [u8;32] + rest (same rules as x509::int32).
fn der_int32(b: &[u8]) -> Option<([u8; 32], &[u8])> {
    if b.len() < 2 || b[0] != 0x02 {
        return None;
    }
    let (ilen, hp) = der_len(&b[1..])?;
    let total = 1 + hp + ilen;
    if b.len() < total || ilen == 0 || ilen > 33 {
        return None;
    }
    let mut v = &b[1 + hp..1 + hp + ilen];
    while v.len() > 1 && v[0] == 0 && (v[1] & 0x80) == 0 {
        v = &v[1..];
    }
    if v.len() > 1 && v[0] == 0 && (v[1] & 0x80) != 0 {
        v = &v[1..];
    }
    if v.is_empty() || v.len() > 32 {
        return None;
    }
    let mut out = [0u8; 32];
    out[32 - v.len()..].copy_from_slice(v);
    Some((out, &b[total..]))
}

// ---- key schedule (RFC 8446 §7.1, SHA-256) ----

/// HKDF-Expand-Label(secret, label, context, len). `label` WITHOUT the
/// "tls13 " prefix (added here). Labels/contexts are short literals at
/// every call site (<= 12 / <= 32 bytes); longer inputs panic loudly
/// (index out of bounds = fail closed, never silent truncation).
fn expand_label(secret: &[u8; 32], label: &[u8], ctx: &[u8], out: &mut [u8]) {
    debug_assert!(label.len() <= 32 && ctx.len() <= 32 && out.len() <= 32);
    let mut info = [0u8; 2 + 1 + 6 + 32 + 1 + 32];
    // HkdfLabel: len u16 || label_len u8 || "tls13 "+label || ctx_len u8 || ctx
    info[0] = (out.len() >> 8) as u8;
    info[1] = out.len() as u8;
    info[2] = (6 + label.len()) as u8;
    info[3..9].copy_from_slice(b"tls13 ");
    info[9..9 + label.len()].copy_from_slice(label);
    let mut p = 9 + label.len();
    info[p] = ctx.len() as u8;
    p += 1;
    info[p..p + ctx.len()].copy_from_slice(ctx);
    p += ctx.len();
    let prk = *secret;
    let okm = hkdf_expand(&prk, &info[..p], out);
    debug_assert!(okm);
}

/// Derive-Secret(secret, label, transcript-hash[32]) -> [u8;32].
fn derive_secret(secret: &[u8; 32], label: &[u8], th: &[u8; 32]) -> [u8; 32] {
    let mut out = [0u8; 32];
    expand_label(secret, label, th, &mut out);
    out
}

fn sha256_of(data: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(data);
    h.finalize()
}

/// Handshake traffic keys + stashed handshake secret for the app phase.
pub struct HsKeys {
    pub c_key: [u8; 16],
    pub c_iv: [u8; 12],
    pub s_key: [u8; 16],
    pub s_iv: [u8; 12],
    pub c_fin: [u8; 32],
    pub s_fin: [u8; 32],
    pub hs_secret: [u8; 32],
}

/// Application traffic keys (no Finished keys needed at this level).
pub struct ApKeys {
    pub c_key: [u8; 16],
    pub c_iv: [u8; 12],
    pub s_key: [u8; 16],
    pub s_iv: [u8; 12],
}

fn split_key_iv(secret: &[u8; 32], key: &mut [u8; 16], iv: &mut [u8; 12]) {
    let mut kb = [0u8; 16];
    expand_label(secret, b"key", b"", &mut kb);
    key.copy_from_slice(&kb);
    let mut ib = [0u8; 12];
    expand_label(secret, b"iv", b"", &mut ib);
    iv.copy_from_slice(&ib);
}

/// Full handshake-secret schedule from the shared secret.
/// `th_ch_sh` = Hash(ClientHello..ServerHello).
pub fn schedule_hs(shared: &[u8; 32], th_ch_sh: &[u8; 32], out: &mut HsKeys) {
    // RFC 8446 §7.1: Early Secret = Extract(salt=0^32, IKM=PSK-or-zeros);
    // with no PSK the IKM is Hash.length ZERO BYTES (not the empty string).
    let mut early = [0u8; 32];
    hkdf_extract(&[0u8; 32], &[0u8; 32], &mut early);
    let derived = derive_secret(&early, b"derived", &sha256_of(&[]));
    let mut hs = [0u8; 32];
    hkdf_extract(&derived, shared, &mut hs);
    let chs = derive_secret(&hs, b"c hs traffic", th_ch_sh);
    let shs = derive_secret(&hs, b"s hs traffic", th_ch_sh);
    split_key_iv(&chs, &mut out.c_key, &mut out.c_iv);
    split_key_iv(&shs, &mut out.s_key, &mut out.s_iv);
    expand_label(&chs, b"finished", b"", &mut out.c_fin);
    expand_label(&shs, b"finished", b"", &mut out.s_fin);
    out.hs_secret = hs;
}

/// Application secrets from the stashed handshake secret.
/// `th_full` = Hash(ClientHello..server Finished).
pub fn schedule_ap(hs_secret: &[u8; 32], th_full: &[u8; 32], out: &mut ApKeys) {
    let derived = derive_secret(hs_secret, b"derived", &sha256_of(&[]));
    let mut master = [0u8; 32];
    // IKM here is likewise 32 zero BYTES (same trap as the early secret).
    hkdf_extract(&derived, &[0u8; 32], &mut master);
    let cap = derive_secret(&master, b"c ap traffic", th_full);
    let sap = derive_secret(&master, b"s ap traffic", th_full);
    split_key_iv(&cap, &mut out.c_key, &mut out.c_iv);
    split_key_iv(&sap, &mut out.s_key, &mut out.s_iv);
}

/// Finished verify_data = HMAC(finished_key, transcript-hash).
pub fn finished_verify(fin_key: &[u8; 32], th: &[u8; 32], out: &mut [u8; 32]) {
    hmac_sha256(fin_key, th, out);
}

// ---- record protection (AES-128-GCM, TLS 1.3 nonce/AAD) ----

/// nonce = iv[0..4] || (iv[4..12] XOR seq_be64).
fn make_nonce(iv: &[u8; 12], seq: u64, out: &mut [u8; 12]) {
    out[..4].copy_from_slice(&iv[..4]);
    let sb = seq.to_be_bytes();
    let mut i = 0;
    while i < 8 {
        out[4 + i] = iv[4 + i] ^ sb[i];
        i += 1;
    }
}

/// Seal one TLSCiphertext record: header(0x17 0303 len) + AES-GCM(frag ||
/// [inner_type]). Returns total bytes. frag.len() <= 16384 enforced.
pub fn seal(key: &[u8; 16], iv: &[u8; 12], seq: u64, frag: &[u8], ctype: u8, out: &mut [u8]) -> Option<usize> {
    if frag.len() > 16384 {
        return None;
    }
    let total = 5 + frag.len() + 1 + 16;
    if out.len() < total {
        return None;
    }
    out[0] = 23;
    out[1] = 0x03;
    out[2] = 0x03;
    let body = frag.len() + 1 + 16;
    out[3] = (body >> 8) as u8;
    out[4] = body as u8;
    out[5..5 + frag.len()].copy_from_slice(frag);
    out[5 + frag.len()] = ctype;
    let mut nonce = [0u8; 12];
    make_nonce(iv, seq, &mut nonce);
    let k = Key128::expand(key);
    // plaintext (frag || [ctype]) is already staged at out[5..]; encrypt
    // in place, then append the tag (split borrows to satisfy the checker).
    let pt_len = frag.len() + 1;
    let (head, rest) = out.split_at_mut(5);
    let (pt, tag768) = rest.split_at_mut(pt_len);
    let (tagb, _) = tag768.split_at_mut(16);
    let mut tag = [0u8; 16];
    if !gcm::encrypt(&k, &nonce, head, pt, &mut tag) {
        return None; // out[5..] was mutated; caller drops the record
    }
    tagb.copy_from_slice(&tag);
    Some(total)
}

/// Open one TLSCiphertext record. Returns (plaintext_len, inner_type);
/// plaintext (without type/padding) is written to out[..len]. Rejects
/// non-0x17 outer type, bad lengths, GCM failure, and inner types other
/// than Finished(20)/handshake(22)/appdata(23).
pub fn open(key: &[u8; 16], iv: &[u8; 12], seq: u64, rec: &[u8], out: &mut [u8]) -> Option<(usize, u8)> {
    if rec.len() < 5 + 17 || rec[0] != 23 || rec[1] != 0x03 || rec[2] != 0x03 {
        return None;
    }
    let body = ((rec[3] as usize) << 8) | rec[4] as usize;
    if body + 5 != rec.len() || body < 17 {
        return None; // exact-length records only (our server's shape)
    }
    let ct_len = body - 16;
    if out.len() < ct_len {
        return None;
    }
    let mut nonce = [0u8; 12];
    make_nonce(iv, seq, &mut nonce);
    let k = Key128::expand(key);
    out[..ct_len].copy_from_slice(&rec[5..5 + ct_len]);
    let mut tag = [0u8; 16];
    tag.copy_from_slice(&rec[5 + ct_len..]);
    // aad borrows rec (immutable) while out is written: disjoint objects.
    let mut aad = [0u8; 5];
    aad.copy_from_slice(&rec[..5]);
    if !gcm::decrypt(&k, &nonce, &aad, &mut out[..ct_len], &tag) {
        return None;
    }
    // strip padding: inner type = last nonzero byte; must be 20/22/23.
    let mut end = ct_len;
    while end > 0 && out[end - 1] == 0 {
        end -= 1;
    }
    if end == 0 {
        return None;
    }
    let t = out[end - 1];
    if t != 20 && t != 22 && t != 23 {
        return None;
    }
    Some((end - 1, t))
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

    fn b16(s: &str) -> [u8; 16] {
        let v = hex(s);
        let mut o = [0u8; 16];
        o.copy_from_slice(&v);
        o
    }

    fn b12(s: &str) -> [u8; 12] {
        let v = hex(s);
        let mut o = [0u8; 12];
        o.copy_from_slice(&v);
        o
    }

    /// Key schedule vs the python reference (/tmp/tlsref.py, RFC 8446 §7.1).
    /// shared = 32x0x11, th = sha256("").
    #[test]
    fn schedule_vector1() {
        let shared = b32("1111111111111111111111111111111111111111111111111111111111111111");
        let th = b32("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
        let mut hs = HsKeys {
            c_key: [0u8; 16], c_iv: [0u8; 12], s_key: [0u8; 16], s_iv: [0u8; 12],
            c_fin: [0u8; 32], s_fin: [0u8; 32], hs_secret: [0u8; 32],
        };
        schedule_hs(&shared, &th, &mut hs);
        assert!(hs.c_key == b16("357bfcec1d4017db5c8c1aa993b46408"));
        assert!(hs.c_iv == b12("f40753e3ae775fff0d3f4955"));
        assert!(hs.s_key == b16("f5228b0d9c2e5bdc8151bb6a9509914e"));
        assert!(hs.s_iv == b12("40e1e5764637802da3922327"));
        assert!(hs.c_fin == b32("2aa9f9d7763d124ec4c7df4d38d6421e5392395c2e82a640af9778bd33b4e89e"));
        let mut ap = ApKeys { c_key: [0u8; 16], c_iv: [0u8; 12], s_key: [0u8; 16], s_iv: [0u8; 12] };
        schedule_ap(&hs.hs_secret, &th, &mut ap);
        assert!(ap.c_key == b16("aa5710ddd7cc433297c6811cce14ef30"));
        assert!(ap.c_iv == b12("6062b7b4c6dc6e56fd725c4b"));
    }

    /// Second vector (different shared + different th).
    #[test]
    fn schedule_vector2() {
        let shared = b32("ab54ff54ff54ff54ff54ff54ff54ff54ff54ff54ff54ff54ff54ff54ff54ff54");
        let th = b32("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef");
        let mut hs = HsKeys {
            c_key: [0u8; 16], c_iv: [0u8; 12], s_key: [0u8; 16], s_iv: [0u8; 12],
            c_fin: [0u8; 32], s_fin: [0u8; 32], hs_secret: [0u8; 32],
        };
        schedule_hs(&shared, &th, &mut hs);
        assert!(hs.c_key == b16("752ff9e5cd546211098463935fd6394f"));
        assert!(hs.c_iv == b12("e2c98c3d7041d2e17e1d3fa0"));
        assert!(hs.s_key == b16("58bc7d1bbed5d8fc1ce050091e3a4caf"));
        assert!(hs.s_iv == b12("580089148d44df697c245929"));
        assert!(hs.c_fin == b32("6b3eeebd44c2924417fed4eec3d68296341f726b22a45717a938970db6fe7c61"));
        let mut ap = ApKeys { c_key: [0u8; 16], c_iv: [0u8; 12], s_key: [0u8; 16], s_iv: [0u8; 12] };
        schedule_ap(&hs.hs_secret, &th, &mut ap);
        assert!(ap.c_key == b16("6a6a92893b776c2b7a18b8377656dc6d"));
        assert!(ap.c_iv == b12("5ea4f42efb0d865b13f21969"));
    }

    #[test]
    fn client_hello_shape() {
        let rnd = [0xabu8; 32];
        let pubk = [0x09u8; 32];
        let mut out = [0u8; 256];
        let n = build_client_hello(&rnd, &pubk, &mut out).expect("ch");
        assert!(n == 117);
        assert!(out[0] == 22 && out[1] == 0x03 && out[2] == 0x03);
        assert!(out[5] == 1); // ClientHello
        assert!(out[9] == 0x03 && out[10] == 0x03); // legacy_version
        assert!(out[11..43] == rnd);
        assert!(out[46] == 0x13 && out[47] == 0x01); // cipher suite
        // key_share holds our ephemeral pubkey at the tail
        assert!(out[n - 32..n] == pubk);
        // supported_versions selected 0304 present
        assert!(out[52..59] == [0x00, 0x2b, 0x00, 0x03, 0x02, 0x03, 0x04]);
        // frag_of strips the record header
        let frag = frag_of(&out[..n]).expect("frag");
        assert!(frag.len() == 112 && frag[0] == 1);
    }

    #[test]
    fn seal_open_roundtrip() {
        let key = b16("357bfcec1d4017db5c8c1aa993b46408");
        let iv = b12("f40753e3ae775fff0d3f4955");
        let msg = b"GET /pkg/hello/1.0/manifest HTTP/1.0\r\n\r\n";
        let mut rec = [0u8; 256];
        let n = seal(&key, &iv, 7, msg, INNER_APPDATA, &mut rec).expect("seal");
        assert!(rec[0] == 23);
        let mut pt = [0u8; 256];
        let (ln, ty) = open(&key, &iv, 7, &rec[..n], &mut pt).expect("open");
        assert!(ty == INNER_APPDATA && ln == msg.len() && pt[..ln] == *msg);
        // wrong seq fails
        assert!(open(&key, &iv, 8, &rec[..n], &mut pt).is_none());
        // tampered ciphertext fails
        let mut bad = [0u8; 256];
        bad[..n].copy_from_slice(&rec[..n]);
        bad[10] ^= 1;
        assert!(open(&key, &iv, 7, &bad[..n], &mut pt).is_none());
        // truncated record fails
        assert!(open(&key, &iv, 7, &rec[..n - 1], &mut pt).is_none());
    }

    #[test]
    fn hs_msgs_coalesced() {
        // two messages in one buffer + incomplete tail stops iteration
        let mut buf = [0u8; 32];
        buf[0] = 8;
        buf[3] = 4; // len 4
        buf[4..8].copy_from_slice(&[1, 2, 3, 4]);
        buf[8] = 20;
        buf[11] = 2; // len 2, but only 1 byte present -> incomplete
        buf[12] = 9;
        let mut it = HsMsgs::new(&buf[..13]);
        let (t1, b1) = it.next().expect("first");
        assert!(t1 == 8 && b1 == [1, 2, 3, 4]);
        assert!(it.next().is_none());
    }

    #[test]
    fn server_hello_rejects() {
        // bad cipher suite
        let mut good = [0u8; 80];
        good[0] = 0x03;
        good[1] = 0x03;
        // random 32B
        good[34] = 0; // sid len 0
        good[35] = 0x13;
        good[36] = 0x01; // cipher 1301
        good[37] = 0; // compression len... (need care: comp is len+bytes)
        // body: ver(2) + random(32) + sidlen(1) + cs(2) + comp(len+1) + extlen(2) + exts
        // comp at 37: len byte then bytes; set len 1, value 0
        good[37] = 1;
        good[38] = 0;
        good[39] = 0;
        good[40] = 10; // ext total 10: SV(7)... need 7+? use SV only (7) -> total 7
        good[40] = 0;
        good[41] = 7;
        // SV ext
        good[42] = 0x00;
        good[43] = 0x2b;
        good[44] = 0x00;
        good[45] = 0x03;
        good[46] = 0x02;
        good[47] = 0x03;
        good[48] = 0x04;
        let _ = &good[49..];
        // no key_share -> None (share required)
        assert!(parse_server_hello(&good[..49]).is_none());
    }
}

/// Host interop: full TLS 1.3 handshake against a real `openssl s_server`
/// (forced suite TLS_AES_128_GCM_SHA256, ECDSA P-256 leaf) plus an HTTP
/// roundtrip. This is the absolute proof that ClientHello / key schedule
/// / record layer / cert chain / Finished interoperate with a compliant
/// peer -- unit vectors alone cannot show that. Skips gracefully when
/// `openssl` is absent. (QEMU suite later repeats this against the real
/// registry with the guest TCP stack.)
#[cfg(test)]
mod interop_tests {
    use super::*;
    use crate::x509::verify_chain;
    use crate::x25519::x25519;

    fn have_openssl() -> bool {
        std::process::Command::new("openssl")
            .arg("version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    fn run(cmd: &str, args: &[&str], cwd: &std::path::Path) -> bool {
        std::process::Command::new(cmd)
            .args(args)
            .current_dir(cwd)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    fn read_exact(s: &mut std::net::TcpStream, mut buf: &mut [u8]) -> std::io::Result<()> {
        while !buf.is_empty() {
            match s.read(&mut buf) {
                Ok(0) => return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "eof")),
                Ok(n) => {
                    let t = buf;
                    buf = &mut t[n..];
                }
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    use std::io::{Read, Write};

    fn read_record(s: &mut std::net::TcpStream) -> std::io::Result<std::vec::Vec<u8>> {
        let mut hdr = [0u8; 5];
        read_exact(s, &mut hdr)?;
        let n = ((hdr[3] as usize) << 8) | hdr[4] as usize;
        if n > 17000 {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "record too big"));
        }
        let mut rec = std::vec::Vec::with_capacity(5 + n);
        rec.extend_from_slice(&hdr);
        rec.resize(5 + n, 0);
        read_exact(s, &mut rec[5..])?;
        Ok(rec)
    }

    #[test]
    fn interop_openssl_server() {
        if !have_openssl() {
            println!("SKIP: openssl not present");
            return;
        }
        let dir = std::env::temp_dir().join(format!("guest-tls-it-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        // Fresh P-256 root + leaf (SAN IP:127.0.0.1 for this test).
        assert!(run("openssl", &["ecparam", "-name", "prime256v1", "-genkey", "-noout", "-out", "rk.pem"], &dir));
        assert!(run(
            "openssl",
            &["req", "-new", "-x509", "-key", "rk.pem", "-out", "rc.der", "-outform", "DER",
              "-days", "2", "-subj", "/CN=IT Root", "-addext", "basicConstraints=critical,CA:TRUE"],
            &dir
        ));
        assert!(run("openssl", &["ecparam", "-name", "prime256v1", "-genkey", "-noout", "-out", "lk.pem"], &dir));
        assert!(run("openssl", &["req", "-new", "-key", "lk.pem", "-out", "l.csr", "-subj", "/CN=localhost"], &dir));
        std::fs::write(
            dir.join("lext.cnf"),
            "subjectAltName=DNS:localhost,IP:127.0.0.1\nbasicConstraints=CA:FALSE\nkeyUsage=digitalSignature\nextendedKeyUsage=serverAuth\n",
        )
        .unwrap();
        assert!(run(
            "openssl",
            &["x509", "-req", "-in", "l.csr", "-CA", "rc.der", "-CAkey", "rk.pem", "-CAcreateserial",
              "-out", "lc.der", "-outform", "DER", "-days", "2", "-extfile", "lext.cnf"],
            &dir
        ));
        // openssl wants PEM for s_server: convert.
        assert!(run("openssl", &["x509", "-in", "lc.der", "-inform", "DER", "-out", "lc.pem"], &dir));
        let port = 18443;
        let mut server = std::process::Command::new("openssl")
            .args([
                "s_server", "-accept", &port.to_string(), "-tls1_3",
                "-ciphersuites", "TLS_AES_128_GCM_SHA256",
                "-cert", "lc.pem", "-key", "lk.pem", "-naccept", "1", "-www",
            ])
            .current_dir(&dir)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn s_server");
        // connect with retries (server needs a moment to bind)
        let mut stream = None;
        for _ in 0..50 {
            match std::net::TcpStream::connect(("127.0.0.1", port)) {
                Ok(s) => {
                    stream = Some(s);
                    break;
                }
                Err(_) => std::thread::sleep(std::time::Duration::from_millis(100)),
            }
        }
        let mut stream = match stream {
            Some(s) => s,
            None => {
                let _ = server.kill();
                panic!("s_server did not come up");
            }
        };
        stream.set_read_timeout(Some(std::time::Duration::from_secs(10))).unwrap();
        let root_der = std::fs::read(dir.join("rc.der")).unwrap();

        // ---- handshake (deterministic test randomness) ----
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        drive(&mut stream, &root_der, [127, 0, 0, 1], now, "GET / HTTP/1.0\r\n\r\n", "200");

        drop(stream);
        // -www keeps serving until the client goes away; we already closed
        // our side above, so reap with a deadline then kill (no hangs).
        let mut gone = false;
        for _ in 0..50 {
            match server.try_wait() {
                Ok(Some(_)) => {
                    gone = true;
                    break;
                }
                Ok(None) => std::thread::sleep(std::time::Duration::from_millis(100)),
                Err(_) => break,
            }
        }
        if !gone {
            let _ = server.kill();
        }
        let _ = std::fs::remove_dir_all(&dir);
    }


    /// Shared handshake driver (both interop tests): TLS 1.3 against the
    /// given server, chain pinned to `root_der`, HTTP GET, body assert.
    #[allow(clippy::too_many_arguments)]
    fn drive(
        mut stream: &mut std::net::TcpStream,
        root_der: &[u8],
        expect_ip: [u8; 4],
        now: u64,
        get_path: &str,
        expect_body: &str,
    ) {
    // ---- handshake (deterministic test randomness) ----
    let eph_priv = [0x22u8; 32];
    let base = {
        let mut b = [0u8; 32];
        b[0] = 9;
        b
    };
    let eph_pub = x25519(&eph_priv, &base);
    let random = [0x33u8; 32];
    let mut ch = [0u8; 256];
    let chn = build_client_hello(&random, &eph_pub, &mut ch).unwrap();
    let mut trans = Sha256::new();
    trans.update(&ch[5..chn]); // fragments only, never record headers
    stream.write_all(&ch[..chn]).unwrap();

    // ServerHello (plaintext)
    let sh = read_record(&mut stream).unwrap();
    assert!(sh[0] == REC_HANDSHAKE);
    let shfrag = frag_of(&sh).unwrap();
    assert!(shfrag[0] == HS_SERVER_HELLO);
    trans.update(shfrag);
    let share = parse_server_hello(&shfrag[4..]).unwrap();
    let shared = x25519(&eph_priv, &share);
    let th_ch_sh = {
        let mut h = trans.clone();
        // NOTE: Sha256::finalize consumes; clone first (Clone derived).
        let _ = &h;
        sha_of_transcript(&trans)
    };
    let mut hs = HsKeys {
        c_key: [0u8; 16], c_iv: [0u8; 12], s_key: [0u8; 16], s_iv: [0u8; 12],
        c_fin: [0u8; 32], s_fin: [0u8; 32], hs_secret: [0u8; 32],
    };
    schedule_hs(&shared, &th_ch_sh, &mut hs);

    // Encrypted flight: read records until EE+Cert+CV+Finished parsed.
    let mut staging = std::vec::Vec::new();
    let mut seq_rx: u64 = 0;
    let flight = loop {
        let rec = read_record(&mut stream).unwrap();
        if rec[0] == 20 {
            // Middlebox-compat ChangeCipherSpec (RFC 8446 §5): single
            // 0x01 byte, unprotected, consumes no sequence number.
            assert!(rec.len() == 6 && rec[5] == 1);
            continue;
        }
        if rec[0] != 23 {
            panic!("flight rec type={} len={}", rec[0], rec.len());
        }
        let mut pt = [0u8; 17000];
        let (ln, ty) = open(&hs.s_key, &hs.s_iv, seq_rx, &rec, &mut pt).unwrap();
        seq_rx += 1;
        assert!(ty == INNER_HANDSHAKE);
        staging.extend_from_slice(&pt[..ln]);
        // try parse: exactly EE, Cert, CV, Finished in order
        let mut it = HsMsgs::new(&staging);
        let m1 = it.next();
        let m2 = it.next();
        let m3 = it.next();
        let m4 = it.next();
        match (m1, m2, m3, m4) {
            (Some((8, _)), Some((11, _)), Some((15, _)), Some((20, _))) => break (m1, m2, m3, m4),
            _ => continue,
        }
    };
    let (Some((_, ee)), Some((_, cert)), Some((_, cv)), Some((_, sfin))) = flight else {
        panic!("flight shape");
    };
    let _ = ee;
    trans.update(raw_of(&staging, 0));
    // NOTE: transcript needs per-message raw bytes; recompute below.
    // ---- verify cert chain (root from disk, IP SAN, wall clock) ----
    let leaf_der = parse_certificate(cert).unwrap();
    verify_chain(leaf_der, root_der, now, expect_ip).expect("chain");
    // ---- transcript hashes at the two Finished points ----
    // Rebuild: CH frag + SH frag + EE + Cert + CV fragments.
    // (staging holds EE..SF plaintexts concatenated; CH/SH saved above.)
    let mut th = Sha256::new();
    th.update(&ch[5..chn]);
    th.update(shfrag);
    // NOTE: shfrag borrow ended; re-read not needed (bytes in staging only
    // cover post-SH). Track lengths: EE+Cert+CV = staging minus sfin frag.
    let sfin_frag_len = 4 + sfin.len();
    let pre = staging.len() - sfin_frag_len;
    // th_cv = Hash(CH..Cert): feed CH, SH, then EE+Cert fragments.
    {
        let mut h = Sha256::new();
        h.update(&ch[5..chn]);
        h.update(shfrag);
        // find EE+Cert span: walk messages again up to CV
        let mut it = HsMsgs::new(&staging);
        let mut span_end = 0usize;
        let mut seen = 0;
        // walk raw: recompute offsets by re-iterating with lengths
        let mut off = 0usize;
        while seen < 2 {
            let ml = ((staging[off + 1] as usize) << 16)
                | ((staging[off + 2] as usize) << 8)
                | staging[off + 3] as usize;
            off += 4 + ml;
            seen += 1;
            let _ = it.next();
        }
        span_end = off;
        h.update(&staging[..span_end]);
        let digest = h.finalize();
        // CertificateVerify check against the leaf key
        let cert_parsed = crate::x509::parse_cert(leaf_der).unwrap();
        assert!(check_cert_verify(cv, &cert_parsed.spki_x, &cert_parsed.spki_y, &digest));
        // th BEFORE server Finished (CH..CV)
        let mut h2 = Sha256::new();
        h2.update(&ch[5..chn]);
        h2.update(shfrag);
        h2.update(&staging[..pre]);
        let th_sfin = h2.finalize();
        let mut expect = [0u8; 32];
        finished_verify(&hs.s_fin, &th_sfin, &mut expect);
        assert!(expect[..] == sfin[..]);
        // th_full = CH..serverFinished for the app schedule
        let mut h3 = Sha256::new();
        h3.update(&ch[5..chn]);
        h3.update(shfrag);
        h3.update(&staging[..]);
        let th_full = h3.finalize();
        let mut ap = ApKeys { c_key: [0u8; 16], c_iv: [0u8; 12], s_key: [0u8; 16], s_iv: [0u8; 12] };
        schedule_ap(&hs.hs_secret, &th_full, &mut ap);
        // client Finished under the HANDSHAKE key
        let mut cfrag = [0u8; 40];
        cfrag[0] = HS_FINISHED;
        cfrag[1] = 0;
        cfrag[2] = 0;
        cfrag[3] = 32;
        let mut cfin = [0u8; 32];
        finished_verify(&hs.c_fin, &th_full, &mut cfin);
        cfrag[4..36].copy_from_slice(&cfin);
        let mut rec = [0u8; 128];
        let rn = seal(&hs.c_key, &hs.c_iv, 0, &cfrag[..36], INNER_HANDSHAKE, &mut rec).unwrap();
        stream.write_all(&rec[..rn]).unwrap();
        // HTTP under application keys; skip NST (inner handshake) records
        let mut arec = [0u8; 17000];
        let mut greq = [0u8; 256];
    greq[..get_path.len()].copy_from_slice(get_path.as_bytes());
    let an = seal(&ap.c_key, &ap.c_iv, 0, &greq[..get_path.len()], INNER_APPDATA, &mut arec).unwrap();
        stream.write_all(&arec[..an]).unwrap();
        let mut body = std::vec::Vec::new();
        let mut aseq: u64 = 0;
        for _ in 0..16 {
            let r = read_record(&mut stream).unwrap();
            let mut pto = [0u8; 17000];
            let (ln, ty) = open(&ap.s_key, &ap.s_iv, aseq, &r, &mut pto).unwrap();
            aseq += 1;
            if ty == INNER_HANDSHAKE {
                continue; // NewSessionTicket: no resumption, skip
            }
            assert!(ty == INNER_APPDATA);
            body.extend_from_slice(&pto[..ln]);
            if body.windows(4).any(|w| w == b"\r\n\r\n") {
                break;
            }
        }
        let text = String::from_utf8_lossy(&body);
        assert!(text.contains(expect_body), "server replied: {}", &text[..text.len().min(200)]);
    }
    }

    /// Transcript-Hash helper: clone-safe snapshot (Sha256 is Clone).
    fn sha_of_transcript(h: &Sha256) -> [u8; 32] {
        h.clone().finalize()
    }

    /// Live registry interop (diagnostic + contract): same handshake
    /// against img_registry.py:8443 with its persisted chain. Gated by
    /// MUSE_REGISTRY_TLS_TEST=1 (registry must be up; test.sh does not
    /// set it, so the suite never depends on ambient state).
    #[test]
    fn interop_registry() {
        if std::env::var("MUSE_REGISTRY_TLS_TEST").unwrap_or_default() != "1" {
            println!("SKIP: set MUSE_REGISTRY_TLS_TEST=1 with img_registry.py running");
            return;
        }
        let mut stream = std::net::TcpStream::connect(("127.0.0.1", 8443)).expect("registry :8443");
        stream.set_read_timeout(Some(std::time::Duration::from_secs(10))).unwrap();
        let root_der = std::fs::read("../../tools/registry-state/rootcert.der").expect("rootcert");
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        drive(
            &mut stream,
            &root_der,
            [10, 0, 2, 2],
            now,
            "GET /pkg/hello/1.0/manifest HTTP/1.0\r\n\r\n",
            "name: hello",
        );
    }

    fn raw_of(_staging: &[u8], _i: usize) -> &[u8] {
        &[]
    }
}
