#![no_std]
#![no_main]
use core::arch::global_asm;
// NOTE: keep in sync with the other user apps (v0.11 env capture).
global_asm!(r#".section .text.entry
.globl _start
_start:
    ld a0, 0(sp)
    addi a1, sp, 8
    slli t0, a0, 3
    addi t0, t0, 16
    add t1, a1, t0
    la t2, ENVIRON_P
    sd t1, 0(t2)
    ld t0, -8(t1)
    la t2, ENVIRON_C
    sd t0, 0(t2)
    call main
    li a0, 0
    li a7, 2
    ecall
"#);
#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    user_lib::exit(-1);
}

// v3.14 wgets: TLS downloader (TLS 1.3, P-256 chain, wall clock).
// Usage: wgets <host> <tlsport> <path> <outfile> [token]
// Flow mirrors wget, with a TLS handshake in the middle:
//   1. plaintext <host>:8091/time -> SYS_SETWALL (test-grade wall clock);
//      /rootcert -> TOFU pin at /pkg/rootcert (fail closed on mismatch).
//   2. TLS 1.3 to <host>:<tlsport>: x25519 ECDHE + P-256 chain verified
//      against the pin + SAN-IP match + wall-clock validity.
//   3. GET <path> as app data ([token] spliced like wget); server
//      NewSessionTickets (inner handshake) are skipped, alerts fail.
//   4. body streams to <outfile>; exact Content-Length match required.
// Markers mirror wget ([TEST] wget PASS / wget FAIL (...)): test.sh
// counts them; the TLS port distinguishes the suite lines.
// Big buffers (TLS records up to ~17KB) come from mmap: the user stack
// is only 32KB (8 pages) and must not host them.

/// Registry plaintext port (time + rootcert TOFU channel). Same
/// convention as ctr's 8091; the TLS port stays explicit in argv.
const PLAIN_PORT: u16 = 8091;

fn fail(reason: &[u8]) -> ! {
    user_lib::print("[TEST] wget FAIL (");
    // reason is a byte string without NUL; print via write loop
    let mut i = 0;
    while i < reason.len() {
        let mut b = [0u8; 1];
        b[0] = reason[i];
        user_lib::write(1, b.as_ptr(), 1);
        i += 1;
    }
    user_lib::print(")\n");
    user_lib::exit(1);
}

fn send_all(fd: isize, buf: &[u8]) -> bool {
    let mut off = 0;
    let mut spins = 0;
    while off < buf.len() {
        let r = user_lib::send(fd, unsafe { buf.as_ptr().add(off) }, buf.len() - off);
        if r > 0 {
            off += r as usize;
            spins = 0;
            continue;
        }
        if r == -2 {
            user_lib::sleep(1);
            spins += 1;
            if spins > 1000 {
                return false;
            }
            continue;
        }
        return false;
    }
    true
}

fn recv_exact(fd: isize, buf: &mut [u8]) -> bool {
    let mut off = 0;
    let mut spins = 0;
    while off < buf.len() {
        let r = user_lib::recv(fd, unsafe { buf.as_mut_ptr().add(off) }, buf.len() - off);
        if r > 0 {
            off += r as usize;
            spins = 0;
            continue;
        }
        if r == 0 {
            return false; // EOF
        }
        if r == -2 {
            user_lib::sleep(1);
            spins += 1;
            if spins > 1000 {
                return false;
            }
            continue;
        }
        return false;
    }
    true
}

fn connect_tcp(ip: u32, port: u16) -> isize {
    let fd = user_lib::socket(1);
    if fd < 0 {
        return -1;
    }
    let mut spins = 0;
    loop {
        let r = user_lib::connect(fd, ip, port);
        if r == 0 {
            break;
        }
        if r != -2 {
            user_lib::close(fd);
            return -1;
        }
        user_lib::sleep(1);
        spins += 1;
        if spins > 500 {
            user_lib::close(fd);
            return -1;
        }
    }
    fd
}

fn resolve(host: &[u8]) -> u32 {
    match user_lib::parse_ip(host) {
        Some(v) => v,
        None => match user_lib::dns_resolve(host, 0x0a00_0203, 53) {
            Some(v) => v,
            None => fail(b"dns"),
        },
    }
}

fn parse_port(s: &[u8]) -> u16 {
    let mut v: u16 = 0;
    for &c in s.iter() {
        if c < b'0' || c > b'9' {
            fail(b"args");
        }
        v = v.saturating_mul(10).saturating_add((c - b'0') as u16);
    }
    if v == 0 {
        fail(b"args");
    }
    v
}

/// Plaintext GET into mem buf. Returns body length (header verified 200).
/// Used for /time (tiny) and /rootcert (~407B DER).
fn plain_get(ip: u32, path: &[u8], host: &[u8], out: &mut [u8]) -> usize {
    let fd = connect_tcp(ip, PLAIN_PORT);
    if fd < 0 {
        fail(b"connect");
    }
    let mut req = [0u8; 384];
    let rn = user_lib::http_req(&mut req, host, path);
    if rn == 0 || !send_all(fd, &req[..rn]) {
        user_lib::close(fd);
        fail(b"send");
    }
    // read until end-of-header (headers < out.len())
    let mut n = 0usize;
    let mut spins = 0;
    loop {
        let (code, _) = user_lib::http_split(&out[..n]);
        if code != 0 {
            break;
        }
        if n >= out.len() {
            user_lib::close(fd);
            fail(b"header");
        }
        let r = user_lib::recv(fd, unsafe { out.as_mut_ptr().add(n) }, out.len() - n);
        if r > 0 {
            n += r as usize;
            spins = 0;
            continue;
        }
        if r == 0 {
            break;
        }
        if r == -2 {
            user_lib::sleep(1);
            spins += 1;
            if spins > 1000 {
                break;
            }
            continue;
        }
        break;
    }
    let (code, off) = user_lib::http_split(&out[..n]);
    if code != 200 {
        user_lib::close(fd);
        fail(b"status");
    }
    let want = content_length(&out[..off]);
    if want == 0 || want > out.len() {
        user_lib::close(fd);
        fail(b"length");
    }
    // move staged body to front, then stream the rest
    let mut got = 0usize;
    let mut w = off;
    while w < n {
        out[got] = out[w];
        got += 1;
        w += 1;
    }
    spins = 0;
    let mut tmp = [0u8; 512];
    while got < want {
        let r = user_lib::recv(fd, tmp.as_mut_ptr(), tmp.len().min(want - got));
        if r > 0 {
            let mut o = 0;
            while o < r as usize {
                if got >= out.len() {
                    user_lib::close(fd);
                    fail(b"length");
                }
                out[got] = tmp[o];
                got += 1;
                o += 1;
            }
            spins = 0;
            continue;
        }
        if r == 0 {
            break;
        }
        if r == -2 {
            user_lib::sleep(1);
            spins += 1;
            if spins > 1000 {
                break;
            }
            continue;
        }
        break;
    }
    user_lib::close(fd);
    if got != want {
        fail(b"length");
    }
    got
}

fn content_length(head: &[u8]) -> usize {
    let needle = b"Content-Length:";
    let mut i = 0;
    while i + needle.len() <= head.len() {
        if &head[i..i + needle.len()] == needle {
            let mut j = i + needle.len();
            while j < head.len() && (head[j] == b' ' || head[j] == b'\t') {
                j += 1;
            }
            let mut v = 0usize;
            while j < head.len() && head[j] >= b'0' && head[j] <= b'9' {
                v = v * 10 + (head[j] - b'0') as usize;
                j += 1;
            }
            return v;
        }
        i += 1;
    }
    0
}

fn parse_dec(s: &[u8]) -> u64 {
    let mut v: u64 = 0;
    let mut any = false;
    for &c in s.iter() {
        if c >= b'0' && c <= b'9' {
            v = v.saturating_mul(10).saturating_add((c - b'0') as u64);
            any = true;
        } else if any {
            break;
        }
    }
    v
}

use guest_tls::tls13;
use guest_tls::x509;
use guest_tls::x25519::x25519;
use guest_tls::Sha256;

#[no_mangle]
pub extern "C" fn main(argc: usize, argv: *const *const u8) {
    if argc < 5 {
        user_lib::print("usage: wgets <host> <tlsport> <path> <outfile> [token]\n");
        user_lib::exit(1);
    }
    let host = unsafe { user_lib::argv_str(argv, 1, argc).unwrap_or(b"") };
    let tlsport = match unsafe { user_lib::argv_str(argv, 2, argc) } {
        Some(s) => parse_port(s),
        None => fail(b"args"),
    };
    let path = unsafe { user_lib::argv_str(argv, 3, argc).unwrap_or(b"/") };
    let outpath = unsafe { user_lib::argv_str(argv, 4, argc).unwrap_or(b"/dl") };
    let ip = resolve(host);
    let ip4 = [(ip >> 24) as u8, (ip >> 16) as u8, (ip >> 8) as u8, ip as u8];

    // ---- 1. wall clock via plaintext /time ----
    let mut tbuf = [0u8; 512];
    let tn = plain_get(ip, b"/time", host, &mut tbuf);
    let epoch = parse_dec(&tbuf[..tn]);
    if epoch < 946684800 {
        // sanity: later than 2000-01-01 (garbage time fails closed)
        fail(b"time");
    }
    if user_lib::setwall(epoch as usize) != 0 {
        fail(b"time");
    }

    // ---- 2. root cert TOFU via plaintext /rootcert ----
    let mut rbuf = [0u8; 2048];
    let rn = plain_get(ip, b"/rootcert", host, &mut rbuf);
    if rn == 0 || rn > 2048 {
        fail(b"rootcert");
    }
    // pin file /pkg/rootcert: first fetch writes it, later fetches compare
    // (/pkg exists once any ctr install ran, but be explicit: best effort)
    user_lib::mkdir(b"/pkg\0".as_ptr());
    let mut pb = [0u8; 32];
    {
        let k = b"/pkg/rootcert".len().min(31);
        pb[..k].copy_from_slice(&b"/pkg/rootcert"[..k]);
        pb[k] = 0;
    }
    let pf = user_lib::open(pb.as_ptr(), 0);
    if pf >= 0 {
        // pinned: must match exactly (rotation = loud failure, re-pin by rm)
        let mut old = [0u8; 2048];
        let mut got = 0usize;
        loop {
            let r = user_lib::read(pf, unsafe { old.as_mut_ptr().add(got) }, old.len() - got);
            if r > 0 {
                got += r as usize;
                if got >= old.len() {
                    break;
                }
                continue;
            }
            break;
        }
        user_lib::close(pf);
        if got != rn || old[..got] != rbuf[..rn] {
            fail(b"pin");
        }
    } else {
        let wf = user_lib::open(pb.as_ptr(), user_lib::O_CREATE | user_lib::O_TRUNC);
        if wf < 0 {
            fail(b"rootcert");
        }
        let mut off = 0;
        while off < rn {
            let r = user_lib::write(wf, unsafe { rbuf.as_ptr().add(off) }, rn - off);
            if r <= 0 {
                user_lib::close(wf);
                fail(b"rootcert");
            }
            off += r as usize;
        }
        user_lib::close(wf);
    }

    // ---- 3. TLS handshake ----
    // mmap big buffers (records up to ~17KB; user stack is 32KB total)
    let recbase = user_lib::mmap(0, 20480, 3);
    if recbase < 0 {
        fail(b"args");
    }
    let recbuf: &mut [u8] = unsafe { core::slice::from_raw_parts_mut(recbase as *mut u8, 20480) };
    let ptbase = user_lib::mmap(0, 20480, 3);
    if ptbase < 0 {
        fail(b"args");
    }
    let ptbuf: &mut [u8] = unsafe { core::slice::from_raw_parts_mut(ptbase as *mut u8, 20480) };
    let stbase = user_lib::mmap(0, 8192, 3);
    if stbase < 0 {
        fail(b"args");
    }
    let stage: &mut [u8] = unsafe { core::slice::from_raw_parts_mut(stbase as *mut u8, 8192) };
    let mut stage_len = 0usize;

    let fd = connect_tcp(ip, tlsport);
    if fd < 0 {
        fail(b"connect");
    }
    let mut eph = [0u8; 32];
    if user_lib::getrandom(&mut eph) != 32 {
        user_lib::close(fd);
        fail(b"tls-hello");
    }
    let mut chrand = [0u8; 32];
    if user_lib::getrandom(&mut chrand) != 32 {
        user_lib::close(fd);
        fail(b"tls-hello");
    }
    let mut base9 = [0u8; 32];
    base9[0] = 9;
    let ephpub = x25519(&eph, &base9);
    let mut chrec = [0u8; 256];
    let chn = match tls13::build_client_hello(&chrand, &ephpub, &mut chrec) {
        Some(n) => n,
        None => {
            user_lib::close(fd);
            fail(b"tls-hello");
        }
    };
    let mut trans = Sha256::new();
    trans.update(&chrec[5..chn]); // fragments only, never record headers
    let mut chfrag = [0u8; 128];
    chfrag[..chn - 5].copy_from_slice(&chrec[5..chn]);
    let chfrag_len = chn - 5;
    if !send_all(fd, &chrec[..chn]) {
        user_lib::close(fd);
        fail(b"tls-send");
    }
    // ServerHello (plaintext 0x16)
    if !read_record(fd, recbuf) {
        user_lib::close(fd);
        fail(b"tls-sh");
    }
    // copy out: recbuf is reused by the flight loop below
    let mut shfrag = [0u8; 256];
    let shfrag_len: usize;
    let share: [u8; 32];
    {
        let shf = match tls13::frag_of(recbuf) {
            Some(f) if f.len() >= 4 && f[0] == tls13::HS_SERVER_HELLO => f,
            _ => {
                user_lib::close(fd);
                fail(b"tls-sh");
            }
        };
        if shf.len() > shfrag.len() {
            user_lib::close(fd);
            fail(b"tls-flight");
        }
        shfrag[..shf.len()].copy_from_slice(shf);
        shfrag_len = shf.len();
    }
    trans.update(&shfrag[..shfrag_len]);
        share = match tls13::parse_server_hello(&shfrag[4..shfrag_len]) {
            Some(s) => s,
            None => {
                user_lib::close(fd);
                fail(b"tls-sh-parse");
            }
        };
    let shared = x25519(&eph, &share);
    let th_ch_sh = trans.clone().finalize();
    let mut hs = tls13::HsKeys {
        c_key: [0u8; 16],
        c_iv: [0u8; 12],
        s_key: [0u8; 16],
        s_iv: [0u8; 12],
        c_fin: [0u8; 32],
        s_fin: [0u8; 32],
        hs_secret: [0u8; 32],
    };
    tls13::schedule_hs(&shared, &th_ch_sh, &mut hs);
    // encrypted flight -> EE, Cert, CertVerify, Finished (CCS skipped)
    let mut seq_rx: u64 = 0;
    let mut ends = [0usize; 4];
    loop {
        if !read_record(fd, recbuf) {
            user_lib::close(fd);
            fail(b"tls-fread");
        }
        if recbuf[0] == 20 {
            // middlebox-compat CCS: skip, no seq advance
            continue;
        }
        if recbuf[0] != 23 {
            user_lib::close(fd);
            fail(b"tls-ftype");
        }
        // NOTE: open() demands the exact record slice (not the whole mmap).
        let reclen = 5 + (((recbuf[3] as usize) << 8) | recbuf[4] as usize);
        let (ln, ty) = match tls13::open(&hs.s_key, &hs.s_iv, seq_rx, &recbuf[..reclen], ptbuf) {
            Some(v) => v,
            None => {
                user_lib::close(fd);
                fail(b"tls-open");
            }
        };
        seq_rx += 1;
        if ty != tls13::INNER_HANDSHAKE {
            user_lib::close(fd);
            fail(b"tls-hsmsg");
        }
        if stage_len + ln > stage.len() {
            user_lib::close(fd);
            fail(b"tls-fstage");
        }
        stage[stage_len..stage_len + ln].copy_from_slice(&ptbuf[..ln]);
        stage_len += ln;
        // try parse: exactly EE, Cert, CV, Finished in order
        let mut it = tls13::HsMsgs::new(&stage[..stage_len]);
        let m1 = it.next();
        let m2 = it.next();
        let m3 = it.next();
        let m4 = it.next();
        match (m1, m2, m3, m4) {
            (Some((8, _)), Some((11, _)), Some((15, _)), Some((20, _))) => {
                let mut off = 0usize;
                let mut k = 0;
                while k < 4 {
                    let ml = ((stage[off + 1] as usize) << 16)
                        | ((stage[off + 2] as usize) << 8)
                        | stage[off + 3] as usize;
                    off += 4 + ml;
                    ends[k] = off;
                    k += 1;
                }
                break;
            }
            _ => continue,
        }
    }
    // transcript hashes at each Finished point (fragments only).
    // ends[] was filled by the loop above (any other exit diverges).
    // hs-direction seq ends here; app records below restart at 0.
    let _ = seq_rx;
    let (cert_end, cv_end, sf_end) = (ends[1], ends[2], ends[3]);
    let th_cv = hash3(&chfrag[..chfrag_len], &shfrag[..shfrag_len], &stage[..cert_end]);
    let th_sfin = hash3(&chfrag[..chfrag_len], &shfrag[..shfrag_len], &stage[..cv_end]);
    let th_full = hash3(&chfrag[..chfrag_len], &shfrag[..shfrag_len], &stage[..sf_end]);
    let mut it = tls13::HsMsgs::new(&stage[..sf_end]);
    let (_, ee_body) = it.next().unwrap();
    let (_, cert_body) = it.next().unwrap();
    let (_, cv_body) = it.next().unwrap();
    let (_, sf_body) = it.next().unwrap();
    let _ = ee_body; // EncryptedExtensions: no enforced content (our server)
    let leaf_der = match tls13::parse_certificate(cert_body) {
        Some(d) => d,
        None => {
            user_lib::close(fd);
            fail(b"tls-cert");
        }
    };
    let wt = user_lib::walltime();
    if wt < 0 {
        user_lib::close(fd);
        fail(b"time");
    }
    if x509::verify_chain(leaf_der, &rbuf[..rn], wt as u64, ip4).is_err() {
        user_lib::close(fd);
        fail(b"tls-verify");
    }
    let leaf = match x509::parse_cert(leaf_der) {
        Ok(c) => c,
        Err(_) => {
            user_lib::close(fd);
            fail(b"tls-verify");
        }
    };
    if !tls13::check_cert_verify(cv_body, &leaf.spki_x, &leaf.spki_y, &th_cv) {
        user_lib::close(fd);
        fail(b"tls-verify");
    }
    {
        let mut expect = [0u8; 32];
        tls13::finished_verify(&hs.s_fin, &th_sfin, &mut expect);
        if expect[..] != sf_body[..] {
            user_lib::close(fd);
            fail(b"tls-finished");
        }
    }
    let mut ap = tls13::ApKeys {
        c_key: [0u8; 16],
        c_iv: [0u8; 12],
        s_key: [0u8; 16],
        s_iv: [0u8; 12],
    };
    tls13::schedule_ap(&hs.hs_secret, &th_full, &mut ap);
    // client Finished under the HANDSHAKE key (RFC 8446 4.4.4), first and
    // only hs-direction send, so seq 0. (App sends below restart at 0.)
    {
        let mut vf = [0u8; 32];
        tls13::finished_verify(&hs.c_fin, &th_full, &mut vf);
        let mut fin = [0u8; 36];
        fin[0] = tls13::HS_FINISHED;
        fin[3] = 32;
        fin[4..36].copy_from_slice(&vf);
        let mut out = [0u8; 128];
        let n = match tls13::seal(&hs.c_key, &hs.c_iv, 0, &fin, tls13::INNER_HANDSHAKE, &mut out) {
            Some(n) => n,
            None => {
                user_lib::close(fd);
                fail(b"tls-finished");
            }
        };
        if !send_all(fd, &out[..n]) {
            user_lib::close(fd);
            fail(b"tls-send");
        }
    }
    // ---- 4. HTTP GET over application data ----
    let mut req = [0u8; 384];
    let mut rn2 = user_lib::http_req(&mut req, host, path);
    if rn2 == 0 {
        user_lib::close(fd);
        fail(b"send");
    }
    if argc > 5 {
        let tok = unsafe { user_lib::argv_str(argv, 5, argc).unwrap_or(b"") };
        if !tok.is_empty() {
            if rn2 < 4 || tok.len() > 63 {
                user_lib::close(fd);
                fail(b"args");
            }
            let pre = b"Authorization: Bearer ";
            let need = rn2 - 2 + pre.len() + tok.len() + 2;
            if need > req.len() {
                user_lib::close(fd);
                fail(b"args");
            }
            let mut nb = [0u8; 384];
            nb[..rn2 - 2].copy_from_slice(&req[..rn2 - 2]);
            let mut w = rn2 - 2;
            nb[w..w + pre.len()].copy_from_slice(pre);
            w += pre.len();
            nb[w..w + tok.len()].copy_from_slice(tok);
            w += tok.len();
            nb[w..w + 2].copy_from_slice(b"\r\n");
            w += 2;
            nb[w..w + 2].copy_from_slice(b"\r\n");
            w += 2;
            req[..w].copy_from_slice(&nb[..w]);
            rn2 = w;
        }
    }
    {
        let mut out = [0u8; 640];
        // NOTE: app-traffic sequence numbers restart at 0 (RFC 8446 5.3:
        // each key phase has its own space); the hs seq is done here.
        let n = match tls13::seal(&ap.c_key, &ap.c_iv, 0, &req[..rn2], tls13::INNER_APPDATA, &mut out) {
            Some(n) => n,
            None => {
                user_lib::close(fd);
                fail(b"send");
            }
        };
        // seq_tx stays 1 here (Finished=0, GET=1); nothing else is sent.
        if !send_all(fd, &out[..n]) {
            user_lib::close(fd);
            fail(b"send");
        }
    }
    // app-direction receive sequence starts at 0 under the new keys.
    let mut ap_rx: u64 = 0;
    // stream the response: headers first (<= 4KB), then body to file
    let mut pb2 = [0u8; 128];
    {
        let k = outpath.len().min(127);
        pb2[..k].copy_from_slice(&outpath[..k]);
        pb2[k] = 0;
    }
    let f = user_lib::open(pb2.as_ptr(), user_lib::O_CREATE | user_lib::O_TRUNC);
    if f < 0 {
        user_lib::close(fd);
        fail(b"open");
    }
    let mut hbuf = [0u8; 4096];
    let mut hn = 0usize;
    let mut want = 0usize;
    let mut got = 0usize;
    let mut hdr_done = false;
    loop {
        if hdr_done && want > 0 && got >= want {
            break;
        }
        if !read_record(fd, recbuf) {
            break; // EOF: fall through to the length check below
        }
        if recbuf[0] != 23 {
            user_lib::close(f);
            user_lib::close(fd);
            fail(b"tls-flight");
        }
        let reclen = 5 + (((recbuf[3] as usize) << 8) | recbuf[4] as usize);
        let (ln, ty) = match tls13::open(&ap.s_key, &ap.s_iv, ap_rx, &recbuf[..reclen], ptbuf) {
            Some(v) => v,
            None => {
                user_lib::close(f);
                user_lib::close(fd);
                fail(b"tls-flight");
            }
        };
        ap_rx += 1;
        if ty == tls13::INNER_HANDSHAKE {
            continue; // NewSessionTicket: no resumption, skip
        }
        if ty != tls13::INNER_APPDATA {
            user_lib::close(f);
            user_lib::close(fd);
            fail(b"tls-flight");
        }
        let mut off = 0;
        while off < ln {
            if !hdr_done {
                if hn >= hbuf.len() {
                    user_lib::close(f);
                    user_lib::close(fd);
                    fail(b"header");
                }
                hbuf[hn] = ptbuf[off];
                hn += 1;
                off += 1;
                let (code, hlen) = user_lib::http_split(&hbuf[..hn]);
                if code != 0 {
                    if code != 200 {
                        user_lib::close(f);
                        user_lib::close(fd);
                        fail(b"status");
                    }
                    want = content_length(&hbuf[..hlen]);
                    if want == 0 {
                        user_lib::close(f);
                        user_lib::close(fd);
                        fail(b"length");
                    }
                    let mut w = hlen;
                    while w < hn {
                        if user_lib::write(f, unsafe { hbuf.as_ptr().add(w) }, 1) != 1 {
                            user_lib::close(f);
                            user_lib::close(fd);
                            fail(b"write");
                        }
                        got += 1;
                        w += 1;
                    }
                    hdr_done = true;
                }
            } else {
                let mut c = off;
                while c < ln {
                    let q = user_lib::write(f, unsafe { ptbuf.as_ptr().add(c) }, ln - c);
                    if q <= 0 {
                        user_lib::close(f);
                        user_lib::close(fd);
                        fail(b"write");
                    }
                    c += q as usize;
                    got += q as usize;
                }
                off = ln;
            }
        }
        if hdr_done && got >= want {
            break;
        }
    }
    user_lib::close(fd);
    user_lib::close(f);
    if !hdr_done || got != want {
        fail(b"length");
    }
    // Distinct per-fetch marker (single write: console lock covers one
    // syscall; test.sh presence-checks each fetch by its basename).
    let mut line = [0u8; 160];
    let pre = b"[TEST] wgets PASS ";
    line[..pre.len()].copy_from_slice(pre);
    let mut w = pre.len();
    let mut seg = 0;
    let mut i = 0;
    while i < path.len() {
        if path[i] == b'/' {
            seg = i + 1;
        }
        i += 1;
    }
    while seg < path.len() && w + 1 < line.len() && path[seg] >= 0x20 && path[seg] < 0x7f {
        line[w] = path[seg];
        w += 1;
        seg += 1;
    }
    line[w] = b'\n';
    w += 1;
    match core::str::from_utf8(&line[..w]) {
        Ok(s) => user_lib::print(s),
        Err(_) => user_lib::print("[TEST] wgets PASS\n"),
    }
    user_lib::exit(0);
}

/// Hash(a || b || c) for transcript snapshots.
fn hash3(a: &[u8], b: &[u8], c: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(a);
    h.update(b);
    h.update(c);
    h.finalize()
}

/// Read one TLS record (5-byte header + exact body) into buf.
/// Returns false on EOF/error/timeout. buf must fit the record.
fn read_record(fd: isize, buf: &mut [u8]) -> bool {
    if buf.len() < 5 || !recv_exact(fd, &mut buf[..5]) {
        return false;
    }
    if buf[0] != 22 && buf[0] != 23 && buf[0] != 20 && buf[0] != 21 {
        return false;
    }
    let n = ((buf[3] as usize) << 8) | buf[4] as usize;
    if n == 0 || n > buf.len() - 5 {
        return false;
    }
    recv_exact(fd, &mut buf[5..5 + n])
}
