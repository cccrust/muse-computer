//! Tiny argv helpers for muse-riscv-os guest programs.
//!
//! `core`-only, zero dependencies: usable from `#![no_std]` guest
//! binaries and from host tests alike. Collected from patterns
//! hand-rolled across `ctr`/`sh` (v2.8: quota flags, v3.x: package
//! specs) so future guest programs stop reinventing them.
//!
//! (`cfg_attr` keeps `std` for the host test harness only; guest and
//! non-test builds stay pure `core`, so `std` creep fails fast.)

#![cfg_attr(not(test), no_std)]

/// Version constraint operators (mirror the `ctr install` grammar).
pub const VEXACT: u8 = 0;
pub const VATLEAST: u8 = 1;

/// Split `foo` / `foo=1.0` / `foo>=1.0` into (name, Option<(ver, op)>).
/// A `>` not followed by `=` degrades to name-only (callers reject it
/// loudly when strictness matters).
pub fn split_vreq(tok: &[u8]) -> (&[u8], Option<(&[u8], u8)>) {
    let mut e = 0;
    while e < tok.len() && tok[e] != b'=' && tok[e] != b'>' {
        e += 1;
    }
    if e >= tok.len() {
        return (tok, None);
    }
    if tok[e] == b'>' {
        if e + 1 >= tok.len() || tok[e + 1] != b'=' {
            return (&tok[..e], None);
        }
        return (&tok[..e], Some((&tok[e + 2..], VATLEAST)));
    }
    (&tok[..e], Some((&tok[e + 1..], VEXACT)))
}

/// Strict decimal parse: all bytes must be digits, at least one digit,
/// checked arithmetic (overflow -> None).
pub fn parse_dec(s: &[u8]) -> Option<usize> {
    if s.is_empty() {
        return None;
    }
    let mut v = 0usize;
    for &c in s {
        if !(b'0'..=b'9').contains(&c) {
            return None;
        }
        v = v.checked_mul(10)?.checked_add((c - b'0') as usize)?;
    }
    Some(v)
}

/// Memory size to 4K frames: decimal with optional single-letter
/// `K`/`M`/`G` suffix (case-insensitive; bytes, rounded UP to frames).
/// A bare number IS frames. `None` on garbage or overflow.
/// (`ctr` v2.8 semantics: `--memory 16M` == `--memory 4096`.)
pub fn parse_mem(b: &[u8]) -> Option<usize> {
    let mut i = 0;
    while i < b.len() && b[i] >= b'0' && b[i] <= b'9' {
        i += 1;
    }
    let v = parse_dec(&b[..i])?;
    if i == b.len() {
        return Some(v); // frames
    }
    let per = match b[i] {
        b'K' | b'k' => 1024usize,
        b'M' | b'm' => 1024 * 1024,
        b'G' | b'g' => 1024 * 1024 * 1024,
        _ => return None,
    };
    if i + 1 != b.len() {
        return None; // single-letter suffix only (no "MB" aliases)
    }
    let bytes = v.checked_mul(per)?;
    Some(bytes.checked_add(4095)? / 4096)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_shapes() {
        assert_eq!(split_vreq(b"hello"), (&b"hello"[..], None));
        assert_eq!(split_vreq(b"hello=1.0"), (&b"hello"[..], Some((&b"1.0"[..], VEXACT))));
        assert_eq!(
            split_vreq(b"hello>=1.0"),
            (&b"hello"[..], Some((&b"1.0"[..], VATLEAST)))
        );
        // bare '>' degrades to name-only
        assert_eq!(split_vreq(b"a>1"), (&b"a"[..], None));
        // empty version rides along (callers validate with ver_valid)
        assert_eq!(split_vreq(b"a="), (&b"a"[..], Some((&b""[..], VEXACT))));
    }

    #[test]
    fn dec_strict() {
        assert_eq!(parse_dec(b"0"), Some(0));
        assert_eq!(parse_dec(b"007"), Some(7));
        assert_eq!(parse_dec(b"100000"), Some(100000));
        assert_eq!(parse_dec(b""), None);
        assert_eq!(parse_dec(b"12a"), None);
        assert_eq!(parse_dec(b"a12"), None);
        assert_eq!(parse_dec(b" 12"), None);
        // overflow (64-bit)
        assert_eq!(parse_dec(b"99999999999999999999"), None);
    }

    #[test]
    fn mem_frames() {
        assert_eq!(parse_mem(b"5"), Some(5)); // bare = frames
        assert_eq!(parse_mem(b"0"), Some(0));
        assert_eq!(parse_mem(b"16M"), Some(4096));
        assert_eq!(parse_mem(b"16m"), Some(4096));
        assert_eq!(parse_mem(b"1K"), Some(1)); // 1024B rounds up to 1 frame
        assert_eq!(parse_mem(b"1G"), Some(262144));
        assert_eq!(parse_mem(b"100000"), Some(100000));
        assert_eq!(parse_mem(b""), None);
        assert_eq!(parse_mem(b"M"), None);
        assert_eq!(parse_mem(b"12KB"), None); // no multi-letter aliases
        assert_eq!(parse_mem(b"12X"), None);
        assert_eq!(parse_mem(b"-5"), None);
    }
}
