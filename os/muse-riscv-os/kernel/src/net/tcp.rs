// v1.5: minimal TCP (passive open only -- enough for the static web server).
// Scope: 3-way handshake, in-order transfer, fixed-RTO retransmit, half-close.
// Deliberately absent: congestion control, SACK, window scaling, keepalive,
// reassembly (out-of-order dropped, peer retransmits), TIME_WAIT, active open.
//
// All state lives in super::Sock under the single NET lock; every function
// takes &mut NetState/&mut Sock. TX goes through the shared single-flight
// submitter; callers must not hold any other lock (same contract as UDP).

use super::{be16, be32, wbe16, wbe32, GUEST_IP, NetState, Sock, NSOCK};
use alloc::collections::VecDeque;
use alloc::vec::Vec;

pub const TCP_PROTO: u8 = 6;
const MSS: usize = 1460;
const RX_CAP: usize = 32768;
const UNACK_CAP: usize = 65536;
const OUR_WND: u16 = 8192;
const RTO_TICKS: u64 = 20; // 200ms @10ms tick
const MAX_RETRY: u8 = 5;
const CLOSE_TIMEOUT: u64 = 3000; // 30s for SYN_RCVD/FIN states

const F_FIN: u8 = 1;
const F_SYN: u8 = 2;
const F_RST: u8 = 4;
const F_ACK: u8 = 16;

#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum TState {
    Closed = 0,
    Listen = 1,
    SynRcvd = 2,
    Est = 3,
    FinW1 = 4,
    FinW2 = 5,
    Closing = 6,
    CloseWait = 7,
    LastAck = 8,
}

pub struct Tcb {
    pub state: TState,
    pub rip: u32,   // peer ip BE
    pub rport: u16, // peer port
    iss: u32,
    pub snd_una: u32,
    pub snd_nxt: u32,
    pub rcv_nxt: u32,
    fin_sent: bool,
    pub accepted: bool, // server child handed out via accept()
    pub parent: Option<usize>, // listener sock idx (backlog accounting)
    pub unacked: Vec<u8>, // bytes [snd_una .. snd_nxt), for retransmit
    pub rto_at: u64,
    retries: u8,
    pub rx: VecDeque<u8>, // in-order payload
}

impl Tcb {
    pub const fn new() -> Self {
        Self {
            state: TState::Closed,
            rip: 0,
            rport: 0,
            iss: 0,
            snd_una: 0,
            snd_nxt: 0,
            rcv_nxt: 0,
            fin_sent: false,
            accepted: false,
            parent: None,
            unacked: Vec::new(),
            rto_at: 0,
            retries: 0,
            rx: VecDeque::new(),
        }
    }

    pub(crate) fn reset(&mut self) {
        *self = Self::new();
    }
}

// ---- checksum (pseudo-header + segment) ----
fn tcp_cksum(src: u32, dst: u32, seg: &[u8]) -> u16 {
    let mut sum: u32 = 0;
    sum += (src >> 16) as u32 + (src & 0xffff) as u32;
    sum += (dst >> 16) as u32 + (dst & 0xffff) as u32;
    sum += TCP_PROTO as u32;
    sum += seg.len() as u32;
    let mut i = 0;
    while i + 1 < seg.len() {
        sum += be16(&seg[i..i + 2]) as u32;
        i += 2;
    }
    if i < seg.len() {
        sum += (seg[i] as u32) << 8;
    }
    while sum >> 16 != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    // NOTE: no 0 -> 0xffff mapping here (that mapping is only for UDP's
    // "no checksum" idiom). TCP validates as `tcp_cksum(...) == 0`, and a
    // mapping would turn every VALID packet into 0xffff (v1.5 blood lesson:
    // all inbound SYNs were silently dropped by validation).
    !(sum as u16)
}

// ---- transmit one segment (caller holds NET lock via n) ----
fn tx_seg(n: &mut NetState, dst_ip: u32, dport: u16, sport: u16, seq: u32, ack: u32, flags: u8, payload: &[u8]) -> bool {
    let total = 14 + 20 + 20 + payload.len();
    if total > 2048 {
        return false;
    }
    let mut f = [0u8; 2048];
    super::eth_build(&n.gw_mac, 0x0800, &mut f);
    // IP (no options)
    f[14] = 0x45;
    f[15] = 0;
    wbe16(&mut f[16..18], (20 + 20 + payload.len()) as u16);
    wbe16(&mut f[18..20], 0);
    wbe16(&mut f[20..22], 0);
    f[22] = 64;
    f[23] = TCP_PROTO;
    wbe16(&mut f[24..26], 0);
    wbe32(&mut f[26..30], GUEST_IP);
    wbe32(&mut f[30..34], dst_ip);
    let mut cks = super::ip_cksum(&f[14..34]);
    if cks == 0 {
        cks = 0xffff;
    }
    wbe16(&mut f[24..26], cks);
    // TCP (no options, data offset 5)
    let to = 34;
    wbe16(&mut f[to..to + 2], sport);
    wbe16(&mut f[to + 2..to + 4], dport);
    wbe32(&mut f[to + 4..to + 8], seq);
    wbe32(&mut f[to + 8..to + 12], ack);
    f[to + 12] = 0x50;
    f[to + 13] = flags;
    wbe16(&mut f[to + 14..to + 16], OUR_WND);
    wbe16(&mut f[to + 16..to + 18], 0); // checksum placeholder
    wbe16(&mut f[to + 18..to + 20], 0); // urgent
    f[to + 20..to + 20 + payload.len()].copy_from_slice(payload);
    let c = tcp_cksum(GUEST_IP, dst_ip, &f[to..to + 20 + payload.len()]);
    wbe16(&mut f[to + 16..to + 18], c);
    unsafe {
        super::TXH[..10].copy_from_slice(&[0; 10]);
        super::TXP[..total].copy_from_slice(&f[..total]);
    }
    super::tx_submit_locked(n, total)
}

fn send_rst(n: &mut NetState, rip: u32, rport: u16, lport: u16, ack: u32) {
    // RST answering an unexpected segment (best-effort, no state).
    tx_seg(n, rip, rport, lport, 0, ack, F_RST | F_ACK, &[]);
}

fn close_tcb(s: &mut Sock) {
    // Drop the slot's TCP state (called when fully closed / reset).
    s.tcp.reset();
    s.closing = false;
}

// ---- receive path (called from parse_frame under NET lock) ----
// Returns true if a sleeper may need waking (state/data changed).
pub fn tcp_rx(n: &mut NetState, src_ip: u32, seg: &[u8]) -> bool {
    if seg.len() < 20 {
        return false;
    }
    let sport = be16(&seg[0..2]);
    let dport = be16(&seg[2..4]);
    let seq = be32(&seg[4..8]);
    let ackn = be32(&seg[8..12]);
    let doff = ((seg[12] >> 4) as usize) * 4;
    if doff < 20 || seg.len() < doff {
        return false;
    }
    let flags = seg[13];
    // validate checksum over the whole segment
    if tcp_cksum(src_ip, GUEST_IP, seg) != 0 {
        return false;
    }
    let payload = &seg[doff..];
    // find TCB: exact 4-tuple child first, then listener on dport
    let mut child: Option<usize> = None;
    let mut listener: Option<usize> = None;
    for (i, s) in n.socks.iter().enumerate() {
        if !s.used || s.kind != 1 || s.closing {
            continue;
        }
        if s.lport != dport {
            continue;
        }
        match s.tcp.state {
            TState::Listen => {
                listener = Some(i);
            }
            _ => {
                if s.tcp.rip == src_ip && s.tcp.rport == sport {
                    child = Some(i);
                    break;
                }
            }
        }
    }
    if flags & F_RST != 0 {
        if let Some(i) = child {
            // hard reset: drop everything, wake both ends
            close_tcb(&mut n.socks[i]);
            n.socks[i].used = false;
            return true;
        }
        return false;
    }
    // TEMP DBG v1.5: search outcome (first hits only)
    static QD: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
    if QD.fetch_add(1, core::sync::atomic::Ordering::SeqCst) < 3 {
        crate::println!(
            "[DBG] lookup dport={} flags={:#x} child={} listener={}",
            dport,
            flags,
            child.map(|v| v as isize).unwrap_or(-1),
            listener.map(|v| v as isize).unwrap_or(-1)
        );
    }
    if let Some(i) = child {
        return child_rx(n, i, seq, ackn, flags, payload);
    }
    // no child: SYN to a listener?
    if flags & F_SYN != 0 {
        if let Some(li) = listener {
            return listener_syn(n, li, src_ip, sport, seq);
        }
        // no listener: refuse
        let ack = seq.wrapping_add(payload.len() as u32 + if flags & F_FIN != 0 { 1 } else { 0 });
        send_rst(n, src_ip, sport, dport, ack);
        return false;
    }
    // non-SYN to nothing: RST (ack their seq to look sane)
    let ack = seq.wrapping_add(payload.len() as u32 + if flags & F_FIN != 0 { 1 } else { 0 });
    send_rst(n, src_ip, sport, dport, ack);
    false
}

fn unaccepted_count(n: &NetState, li: usize) -> usize {
    let mut k = 0;
    for s in n.socks.iter() {
        if s.used && !s.closing && s.kind == 1 && s.tcp.parent == Some(li) && !s.tcp.accepted {
            k += 1;
        }
    }
    k
}

fn listener_syn(n: &mut NetState, li: usize, src_ip: u32, sport: u16, seq: u32) -> bool {
    // TEMP DBG v1.5: trace handshake (first SYN only)
    static SD: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
    let sd = SD.fetch_add(1, core::sync::atomic::Ordering::SeqCst);
    // backlog 1: one unaccepted child max (peer retransmits otherwise)
    if unaccepted_count(n, li) >= 1 {
        if sd < 2 {
            crate::println!("[DBG] syn backlog-full");
        }
        return false;
    }
    // need a free slot
    let mut free = None;
    for (i, s) in n.socks.iter().enumerate() {
        if !s.used {
            free = Some(i);
            break;
        }
    }
    let ci = match free {
        Some(i) => i,
        None => {
            if sd < 2 {
                crate::println!("[DBG] syn no free slot");
            }
            return false;
        }
    };
    let lport = n.socks[li].lport;
    let iss = (crate::timer::ticks() as u32)
        .wrapping_add((lport as u32) << 16)
        .wrapping_add(0x1_2345);
    {
        let c = &mut n.socks[ci];
        c.used = true;
        c.kind = 1;
        c.closing = false;
        c.lport = lport;
        c.tcp.state = TState::SynRcvd;
        c.tcp.rip = src_ip;
        c.tcp.rport = sport;
        c.tcp.iss = iss;
        c.tcp.snd_una = iss;
        c.tcp.snd_nxt = iss.wrapping_add(1); // SYN consumes one
        c.tcp.rcv_nxt = seq.wrapping_add(1);
        c.tcp.accepted = false;
        c.tcp.parent = Some(li);
        c.tcp.unacked.clear();
        c.tcp.rx.clear();
        c.tcp.rto_at = crate::timer::ticks() as u64 + RTO_TICKS;
        c.tcp.retries = 0;
    }
    // SYN+ACK
    let c = &n.socks[ci];
    let ok = tx_seg(n, src_ip, sport, lport, iss, seq.wrapping_add(1), F_SYN | F_ACK, &[]);
    if sd < 2 {
        crate::println!("[DBG] synack sent={} ci={}", ok as u8, ci);
    }
    true
}

fn child_rx(n: &mut NetState, i: usize, seq: u32, ackn: u32, flags: u8, payload: &[u8]) -> bool {
    let mut wake = false;
    let mut reply: Option<(u32, u32, u8)> = None; // (seq, ack, flags)
    {
        let s = &mut n.socks[i];
        let t = &mut s.tcp;
        // ACK processing (not for pure SYN -- handled at handshake)
        if flags & F_ACK != 0 {
            let lo = t.snd_una;
            let hi = t.snd_nxt;
            let ok = ackn.wrapping_sub(lo) <= hi.wrapping_sub(lo);
            if ok && ackn != lo {
                let adv = ackn.wrapping_sub(lo) as usize;
                let adv = adv.min(t.unacked.len());
                t.unacked.drain(..adv);
                t.snd_una = ackn;
                t.rto_at = crate::timer::ticks() as u64 + RTO_TICKS;
                t.retries = 0;
                wake = true;
                match t.state {
                    TState::SynRcvd => {
                        t.state = TState::Est;
                        // accepted flag set by accept(); wake acceptor now
                    }
                    TState::FinW1 => {
                        if t.fin_sent && ackn == t.snd_nxt {
                            t.state = TState::FinW2;
                        }
                    }
                    TState::Closing => {
                        if ackn == t.snd_nxt {
                            t.state = TState::Closed;
                        }
                    }
                    TState::LastAck => {
                        if ackn == t.snd_nxt {
                            t.state = TState::Closed;
                        }
                    }
                    _ => {}
                }
            } else if !ok {
                // ack beyond snd_nxt: drop segment
                return false;
            }
        }
        if t.state == TState::SynRcvd {
            // duplicate SYN (no ACK yet): resend SYN+ACK
            if flags & F_SYN != 0 {
                reply = Some((t.iss, t.rcv_nxt, F_SYN | F_ACK));
            }
        } else if t.state == TState::Est || t.state == TState::FinW2 || t.state == TState::CloseWait {
            // data?
            if !payload.is_empty() {
                if seq == t.rcv_nxt {
                    let room = RX_CAP.saturating_sub(t.rx.len());
                    let k = room.min(payload.len());
                    t.rx.extend(payload[..k].iter());
                    t.rcv_nxt = t.rcv_nxt.wrapping_add(k as u32);
                    wake = true;
                    // pure ACK for what we took
                    reply = Some((t.snd_nxt, t.rcv_nxt, F_ACK));
                    if k < payload.len() {
                        // truncated (rx full): peer will retransmit; ack covers taken part
                    }
                } else {
                    // out-of-order: drop, re-ACK current (fast-ish recovery via peer RTO)
                    reply = Some((t.snd_nxt, t.rcv_nxt, F_ACK));
                }
            }
            // FIN?
            if flags & F_FIN != 0 {
                // FIN consumes one seq after any data taken above; accept it
                // only if it arrives at rcv_nxt (else it rides the next one)
                let fin_seq = seq.wrapping_add(payload.len() as u32);
                if fin_seq == t.rcv_nxt {
                    t.rcv_nxt = t.rcv_nxt.wrapping_add(1);
                    if t.state == TState::Est {
                        t.state = TState::CloseWait;
                    } else if t.state == TState::FinW2 {
                        t.state = TState::Closed;
                    }
                    wake = true;
                    reply = Some((t.snd_nxt, t.rcv_nxt, F_ACK));
                } else {
                    reply = Some((t.snd_nxt, t.rcv_nxt, F_ACK));
                }
            }
        } else if t.state == TState::FinW1 {
            if flags & F_FIN != 0 {
                let fin_seq = seq.wrapping_add(payload.len() as u32);
                if fin_seq == t.rcv_nxt {
                    t.rcv_nxt = t.rcv_nxt.wrapping_add(1);
                    t.state = TState::Closing;
                    wake = true;
                    reply = Some((t.snd_nxt, t.rcv_nxt, F_ACK));
                }
            }
        } else if t.state == TState::Closing || t.state == TState::LastAck {
            // absorb stray ACKs; nothing else to do
        }
        // capture reply params before releasing borrow
        if t.state == TState::Closed {
            wake = true;
        }
    }
    if let Some((sq, ak, fl)) = reply {
        let s = &n.socks[i];
        tx_seg(n, s.tcp.rip, s.tcp.rport, s.lport, sq, ak, fl, &[]);
    }
    // Closed-with-detached-fd (closing slots) get freed here when safe:
    // freed when slot is closing and state reached Closed.
    if n.socks[i].closing && n.socks[i].tcp.state == TState::Closed {
        n.socks[i].used = false;
        n.socks[i].closing = false;
        n.socks[i].tcp.reset();
    }
    wake
}

// ---- socket-level API (called with NET lock held by wrappers in mod.rs,
// but these take it themselves like the UDP API for symmetry) ----
pub fn tcp_bind(n: &mut NetState, idx: usize, port: u16) -> bool {
    if idx >= NSOCK || !n.socks[idx].used || n.socks[idx].kind != 1 || port == 0 {
        return false;
    }
    // port must be free across UDP + TCP (incl. closing holders)
    for (j, s) in n.socks.iter().enumerate() {
        if j != idx && s.used && s.lport == port {
            return false;
        }
    }
    n.socks[idx].lport = port;
    true
}

pub fn tcp_listen(n: &mut NetState, idx: usize) -> bool {
    if idx >= NSOCK || !n.socks[idx].used || n.socks[idx].kind != 1 {
        return false;
    }
    if n.socks[idx].lport == 0 || n.socks[idx].tcp.state != TState::Closed {
        return false;
    }
    n.socks[idx].tcp.state = TState::Listen;
    true
}

pub fn tcp_accept(n: &mut NetState, li: usize) -> Option<usize> {
    // Peek only (does NOT mark accepted): the caller commits after fd
    // allocation succeeds, so a full fd table leaks nothing.
    if li >= NSOCK || !n.socks[li].used || n.socks[li].tcp.state != TState::Listen {
        return None;
    }
    for (i, s) in n.socks.iter().enumerate() {
        if s.used && !s.closing && s.kind == 1 && s.tcp.parent == Some(li) && !s.tcp.accepted && s.tcp.state == TState::Est {
            return Some(i);
        }
    }
    None
}

/// Mark a peeked child accepted (after the caller's fd allocation).
pub fn tcp_accept_commit(n: &mut NetState, idx: usize) {
    if idx < NSOCK && n.socks[idx].used {
        n.socks[idx].tcp.accepted = true;
    }
}

pub fn tcp_send(n: &mut NetState, idx: usize, data: &[u8]) -> isize {
    if idx >= NSOCK || !n.socks[idx].used || n.socks[idx].kind != 1 {
        return -1;
    }
    let st = n.socks[idx].tcp.state;
    if st != TState::Est && st != TState::CloseWait {
        return -1;
    }
    if n.socks[idx].tcp.unacked.len() + data.len() > UNACK_CAP {
        return -2; // backpressure: drain first
    }
    let (rip, rport, lport, mut seq) = {
        let s = &n.socks[idx];
        (s.tcp.rip, s.tcp.rport, s.lport, s.tcp.snd_nxt)
    };
    let mut done = 0usize;
    let mut off = 0usize;
    while off < data.len() {
        let k = MSS.min(data.len() - off);
        if !tx_seg(n, rip, rport, lport, seq, n.socks[idx].tcp.rcv_nxt, F_ACK, &data[off..off + k]) {
            break;
        }
        n.socks[idx].tcp.unacked.extend(data[off..off + k].iter());
        seq = seq.wrapping_add(k as u32);
        off += k;
        done += k;
    }
    n.socks[idx].tcp.snd_nxt = seq;
    if done == 0 {
        return -1;
    }
    let now = crate::timer::ticks() as u64;
    n.socks[idx].tcp.rto_at = now + RTO_TICKS;
    n.socks[idx].tcp.retries = 0;
    done as isize
}

pub fn tcp_recv(n: &mut NetState, idx: usize, out: &mut [u8]) -> isize {
    if idx >= NSOCK || !n.socks[idx].used || n.socks[idx].kind != 1 {
        return -1;
    }
    let st = n.socks[idx].tcp.state;
    let t = &mut n.socks[idx].tcp;
    if !t.rx.is_empty() {
        let k = out.len().min(t.rx.len());
        for (o, b) in out.iter_mut().take(k).zip(t.rx.drain(..k)) {
            *o = b;
        }
        return k as isize;
    }
    match st {
        TState::CloseWait => 0, // EOF: peer FIN'd and we drained
        TState::Closed => -1,
        _ => -2, // WouldBlock
    }
}

// FIN + release. Returns true if fully done, false if a closing slot
// lingers (freed later by ACK/timeout in child_rx/tick).
pub fn tcp_close(n: &mut NetState, idx: usize) {
    if idx >= NSOCK || !n.socks[idx].used || n.socks[idx].kind != 1 {
        // UDP or empty: plain free (UDP path calls this too? no -- mod.rs
        // frees UDP directly; keep guard anyway)
        if idx < NSOCK {
            n.socks[idx].used = false;
        }
        return;
    }
    let st = n.socks[idx].tcp.state;
    match st {
        TState::Est | TState::CloseWait => {
            // send FIN, keep slot as closing until ACK/timeout
            let (rip, rport, lport, seq, ack) = {
                let s = &n.socks[idx];
                (s.tcp.rip, s.tcp.rport, s.lport, s.tcp.snd_nxt, s.tcp.rcv_nxt)
            };
            tx_seg(n, rip, rport, lport, seq, ack, F_FIN | F_ACK, &[]);
            n.socks[idx].tcp.fin_sent = true;
            n.socks[idx].tcp.snd_nxt = seq.wrapping_add(1);
            n.socks[idx].tcp.rto_at = crate::timer::ticks() as u64 + RTO_TICKS;
            n.socks[idx].tcp.retries = 0;
            n.socks[idx].tcp.state = if st == TState::Est { TState::FinW1 } else { TState::LastAck };
            n.socks[idx].closing = true;
        }
        TState::Listen => {
            // drop listener; live children survive on their own slots
            // (their fds may still be open in the server loop)
            n.socks[idx].used = false;
            n.socks[idx].tcp.reset();
        }
        _ => {
            // SynRcvd/FinW*/Closing/LastAck/Closed: RST if peer exists,
            // then free
            let has_peer = n.socks[idx].tcp.rip != 0;
            if has_peer {
                let (rip, rport, lport, ack) = {
                    let s = &n.socks[idx];
                    (s.tcp.rip, s.tcp.rport, s.lport, s.tcp.rcv_nxt)
                };
                send_rst(n, rip, rport, lport, ack);
            }
            n.socks[idx].used = false;
            n.socks[idx].closing = false;
            n.socks[idx].tcp.reset();
        }
    }
}

/// Per-tick retransmit/timeout scan (timer ISR, 10ms). Returns true if any
/// sleeper may need waking (state flipped to Closed / progress possible).
pub fn tcp_tick(n: &mut NetState, now: u64) -> bool {
    let mut wake = false;
    for i in 0..NSOCK {
        if !n.socks[i].used || n.socks[i].kind != 1 {
            continue;
        }
        let st = n.socks[i].tcp.state;
        match st {
            TState::SynRcvd => {
                if now >= n.socks[i].tcp.rto_at {
                    if n.socks[i].tcp.retries >= MAX_RETRY {
                        // half-open never completed: drop
                        n.socks[i].used = false;
                        n.socks[i].tcp.reset();
                        wake = true;
                        continue;
                    }
                    let (rip, rport, lport, iss, rnx) = {
                        let s = &n.socks[i];
                        (s.tcp.rip, s.tcp.rport, s.lport, s.tcp.iss, s.tcp.rcv_nxt)
                    };
                    tx_seg(n, rip, rport, lport, iss, rnx, F_SYN | F_ACK, &[]);
                    n.socks[i].tcp.retries += 1;
                    n.socks[i].tcp.rto_at = now + RTO_TICKS;
                }
            }
            TState::Est | TState::FinW1 | TState::FinW2 | TState::Closing | TState::LastAck | TState::CloseWait => {
                if !n.socks[i].tcp.unacked.is_empty() && now >= n.socks[i].tcp.rto_at {
                    if n.socks[i].tcp.retries >= MAX_RETRY {
                        let (rip, rport, lport, ack) = {
                            let s = &n.socks[i];
                            (s.tcp.rip, s.tcp.rport, s.lport, s.tcp.rcv_nxt)
                        };
                        send_rst(n, rip, rport, lport, ack);
                        n.socks[i].used = false;
                        n.socks[i].closing = false;
                        n.socks[i].tcp.reset();
                        wake = true;
                        continue;
                    }
                    // go-back-N lite: resend from snd_una, up to one window
                    // worth (bounded work per tick)
                    let (rip, rport, lport, una, ack) = {
                        let s = &n.socks[i];
                        (s.tcp.rip, s.tcp.rport, s.lport, s.tcp.snd_una, s.tcp.rcv_nxt)
                    };
                    let mut off = 0usize;
                    let total = n.socks[i].tcp.unacked.len().min(4 * MSS);
                    while off < total {
                        let k = MSS.min(total - off);
                        // clone chunk (borrow ends before tx)
                        let mut chunk = [0u8; MSS];
                        chunk[..k].copy_from_slice(&n.socks[i].tcp.unacked[off..off + k]);
                        if !tx_seg(n, rip, rport, lport, una.wrapping_add(off as u32), ack, F_ACK, &chunk[..k]) {
                            break;
                        }
                        off += k;
                    }
                    n.socks[i].tcp.retries += 1;
                    n.socks[i].tcp.rto_at = now + RTO_TICKS;
                }
                // closing-state absolute timeout (peer never ACKed FIN)
                if (st == TState::FinW2 || st == TState::LastAck || st == TState::Closing)
                    && n.socks[i].closing
                    && now >= n.socks[i].tcp.rto_at + CLOSE_TIMEOUT
                {
                    n.socks[i].used = false;
                    n.socks[i].closing = false;
                    n.socks[i].tcp.reset();
                    wake = true;
                }
            }
            _ => {}
        }
    }
    wake
}
