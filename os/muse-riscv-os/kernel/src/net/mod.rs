// v1.3: virtio-net (legacy MMIO) + minimal static IPv4/UDP stack.
// Scope: one NIC, static config (guest 10.0.2.15/24, gw 10.0.2.2 -- QEMU
// user-net/SLIRP defaults), ARP + UDP only. No TCP, DHCP, DNS.
//
// v1.5: TCP submodule (passive open, enough for the static web server).

mod tcp;
//
// Concurrency contract (SMP): ONE lock (NET) guards queue indices, the RX
// staging ring, ARP state and sockets. The TX completion wait is a pure
// MMIO poll (like blk's submit) -- never block_current while holding it.
// The ISR harvests under NET, then drops it BEFORE wake_net() (which takes
// the sched lock). Syscall paths never hold sched and NET together:
// with_current copies out first (guard dropped), then NET. So no lock
// order exists at all -- nesting is impossible by construction.
// Syscalls never sleep mid-call (house style): empty recv / missing ARP
// return -2 (WouldBlock); userspace retries with its own deadline.

use crate::fs::virtio as V;
use alloc::collections::VecDeque;
use alloc::vec::Vec;

// ---- config ----
const DEVID_NET: u32 = 1;
const N_RXDESC: usize = 8;
const RXBUF: usize = 2048;
const NSTAGE: usize = 16; // staged RX packets
const NSOCK: usize = 8;
const MAXUDP: usize = 1472; // 1500 - 20 IP - 8 UDP

const GUEST_IP: u32 = 0x0a00_020f; // 10.0.2.15, BE (wire bytes 0A 00 02 0F)
const GW_IP: u32 = 0x0a00_0202; // 10.0.2.2, BE

// ---- device state (write-once at boot init, before APs start) ----
static mut NET_BASE: usize = 0;
static mut READY: bool = false;
static mut MAC: [u8; 6] = [0; 6];
// queue rings (PA)
static mut RX_Q: usize = 0;
static mut RX_USED_PA: usize = 0;
static mut TX_Q: usize = 0;
static mut TX_USED_PA: usize = 0;
// RX buffers (device-writable) + TX staging (built under NET lock)
static mut RXB: [[u8; RXBUF]; N_RXDESC] = [[0; RXBUF]; N_RXDESC];
static mut TXH: [u8; 16] = [0; 16]; // virtio_net_hdr (10B used)
static mut TXP: [u8; 2048] = [0; 2048];

struct Sock {
    used: bool,
    kind: u8, // 0 = UDP, 1 = TCP
    closing: bool, // v1.5: fd detached, TCB draining (FIN_WAIT/LAST_ACK)
    lport: u16,
    peer_ip: u32,   // BE, set by connect
    peer_port: u16, // BE order value (as passed)
    rx: VecDeque<(u32, u16, Vec<u8>)>, // UDP: (src ip BE, src port, payload)
    tcp: tcp::Tcb,
}

impl Sock {
    const fn new() -> Self {
        Self {
            used: false,
            kind: 0,
            closing: false,
            lport: 0,
            peer_ip: 0,
            peer_port: 0,
            rx: VecDeque::new(),
            tcp: tcp::Tcb::new(),
        }
    }
}

struct NetState {
    rx_avail: u16,
    rx_used: u16,
    tx_avail: u16,
    tx_used: u16,
    // staged complete RX packets (raw ether frames, parsed on recv path
    // or parsed here -- parse here, stage per-socket directly)
    arp_ok: bool,
    gw_mac: [u8; 6],
    arp_req_at: u64, // ticks of last ARP request (rate limit)
    socks: [Sock; NSOCK],
    next_port: u16, // ephemeral allocator
}

impl NetState {
    const fn new() -> Self {
        Self {
            rx_avail: 0,
            rx_used: 0,
            tx_avail: 0,
            tx_used: 0,
            arp_ok: false,
            gw_mac: [0; 6],
            arp_req_at: 0,
            socks: [
                Sock::new(),
                Sock::new(),
                Sock::new(),
                Sock::new(),
                Sock::new(),
                Sock::new(),
                Sock::new(),
                Sock::new(),
            ],
            next_port: 49152,
        }
    }
}

static NET: crate::sync::SpinMutex<NetState> = crate::sync::SpinMutex::new(NetState::new());

fn ready() -> bool {
    unsafe { READY }
}
fn base() -> usize {
    unsafe { NET_BASE }
}

// ---- endian helpers (BE on the wire) ----
fn be16(b: &[u8]) -> u16 {
    ((b[0] as u16) << 8) | b[1] as u16
}
fn be32(b: &[u8]) -> u32 {
    ((b[0] as u32) << 24) | ((b[1] as u32) << 16) | ((b[2] as u32) << 8) | b[3] as u32
}
fn wbe16(o: &mut [u8], v: u16) {
    o[0] = (v >> 8) as u8;
    o[1] = v as u8;
}
fn wbe32(o: &mut [u8], v: u32) {
    o[0] = (v >> 24) as u8;
    o[1] = (v >> 16) as u8;
    o[2] = (v >> 8) as u8;
    o[3] = v as u8;
}
fn ip_to_str(ip_be: u32) -> (u8, u8, u8, u8) {
    (
        (ip_be >> 24) as u8,
        (ip_be >> 16) as u8,
        (ip_be >> 8) as u8,
        ip_be as u8,
    )
}

// ---- guarded MMIO probe (absent slots fault; must not die) ----
//
// Reading an unmapped virtio-mmio slot raises a sync fault (load access
// fault or load page fault). This runs once at boot (single hart, SIE=0,
// so only sync faults are possible here) with a temporary stvec: the stub
// below records HIT + skips the faulting 4-byte `lw`. t0/t1 are saved via
// stval juggling (no trusted scratch exists in kernel context); the probe
// load itself is a naked `lw` (never compressed, so sepc+=4 is exact).
// Any other trap parks (never happens: SIE=0 admits no interrupts).

#[no_mangle]
static mut PROBE_SAVE: [usize; 2] = [0; 2];
#[no_mangle]
static mut PROBE_HIT: bool = false;

core::arch::global_asm!(
    r#"
    .section .text
    .globl probe_vec
    .align 2
probe_vec:
    csrrw t0, stval, t0   // stval:=orig t0; t0:=fault addr (scratch)
    la t0, PROBE_SAVE
    sw t1, 4(t0)          // SAVE.t1 = orig t1
    csrr t1, stval        // t1 = orig t0
    sw t1, 0(t0)          // SAVE.t0 = orig t0
    csrr t1, scause
    li t0, 5
    beq t1, t0, probe_hit
    li t0, 13
    beq t1, t0, probe_hit
    li t0, 12
    beq t1, t0, probe_hit
    // unexpected trap during probe: park (loud hang at a known spot)
probe_park:
    wfi
    j probe_park
probe_hit:
    la t0, PROBE_HIT
    li t1, 1
    sb t1, 0(t0)
    csrr t0, sepc
    addi t0, t0, 4
    csrw sepc, t0
    la t0, PROBE_SAVE
    lw t1, 4(t0)
    lw t0, 0(t0)
    sret
"#
);

/// 4-byte load that is never compressed (so the probe stub's sepc+=4 is
/// exact). Only called under the probe stvec.
#[unsafe(naked)]
unsafe extern "C" fn probe_lw(addr: usize) -> u32 {
    core::arch::naked_asm!("lw a0, 0(a0)", "ret")
}

/// Guarded 32-bit MMIO read for probing. None = access faulted (slot
/// absent). Boot-only: swaps stvec around a single load.
fn probe_r32(addr: usize) -> Option<u32> {
    unsafe {
        PROBE_HIT = false;
        let old: usize;
        core::arch::asm!("csrr {0}, stvec", out(reg) old);
        extern "C" {
            fn probe_vec();
        }
        core::arch::asm!("csrw stvec, {0}", in(reg) probe_vec as usize);
        let v = probe_lw(addr);
        let hit = PROBE_HIT;
        core::arch::asm!("csrw stvec, {0}", in(reg) old);
        if hit {
            None
        } else {
            Some(v)
        }
    }
}

// ---- probe + init (boot hart, pre-AP) ----
fn probe() -> Option<usize> {
    // QEMU virt: virtio-mmio slots at 0x10000000 + i*0x1000. Auto-attach
    // fills from the TOP (observed: net lands at .8 when blk takes .0
    // explicitly), so scan wide (0..16); match by DEVID, never by slot.
    // Absent slots are tolerated via the guarded probe_r32 above.
    unsafe {
        for i in 0..16usize {
            let b = 0x1000_0000 + i * 0x1000;
            let m = match probe_r32(b) {
                Some(v) => v,
                None => continue,
            };
            if m != V::MAGIC {
                continue;
            }
            let ver = probe_r32(b + V::R_VER).unwrap_or(0);
            if ver != 1 && ver != 2 {
                continue;
            }
            if probe_r32(b + V::R_DEVID).unwrap_or(0xffff_ffff) == DEVID_NET {
                return Some(b);
            }
        }
        None
    }
}

fn qsetup(base: usize, qsel: usize) -> Option<(usize, usize)> {
    // legacy queue bring-up, mirrored from blk init. Returns (q_base, q_used).
    unsafe {
        V::w32(base, V::R_QSEL, qsel as u32);
        let max = V::r32(base, V::R_QMAX) as usize;
        if max == 0 {
            return None;
        }
        let q = core::cmp::min(max, V::QDEPTH);
        V::w32(base, V::R_QNUM, q as u32);
        let f0 = crate::mem::frame::alloc_frame()?;
        let f1 = crate::mem::frame::alloc_frame()?;
        V::w16(f0, 0);
        V::w16(f0 + 2, 0);
        V::w16(f1, 0);
        V::w16(f1 + 2, 0);
        V::fence();
        V::w32(base, V::R_PAGESZ, 4096);
        V::w32(base, V::R_QALIGN, 4096);
        V::w32(base, V::R_QPFN, (f0 >> 12) as u32);
        V::fence();
        Some((f0, f1))
    }
}

fn desc_set(q: usize, i: usize, addr: usize, len: u32, flags: u16, next: u16) {
    unsafe {
        let d = q + i * 16;
        V::w64pa(d, addr as u64);
        V::w32pa(d + 8, len);
        V::w16(d + 12, flags);
        V::w16(d + 14, next);
    }
}

fn avail_pa(q: usize) -> usize {
    q + V::QDEPTH * 16
}

/// Probe + 2-queue setup + MAC read. Called once (fs::init, boot hart).
pub fn init() {
    let b = match probe() {
        Some(b) => b,
        None => {
            crate::println!("[NET] no device, SKIP");
            return;
        }
    };
    unsafe {
        NET_BASE = b;
        // reset + ack + driver, no features (no offload/GSO/MAC-filtering)
        V::w32(b, V::R_STATUS, 0);
        V::w32(b, V::R_STATUS, V::ST_ACK | V::ST_DRIVER);
        V::w32(b, V::R_DEVFEATSEL, 0);
        let _feat = V::r32(b, V::R_DEVFEAT);
        V::w32(b, V::R_DRVFEATSEL, 0);
        V::w32(b, V::R_DRVFEAT, 0);
        V::w32(b, V::R_STATUS, V::r32(b, V::R_STATUS) | V::ST_FEAT_OK);
        if V::r32(b, V::R_STATUS) & V::ST_FEAT_OK == 0 {
            crate::println!("[NET] FEATURES_OK rejected");
            return;
        }
        let (rxq, rxu) = match qsetup(b, 0) {
            Some(q) => q,
            None => {
                crate::println!("[NET] rx queue unavailable");
                return;
            }
        };
        let (txq, txu) = match qsetup(b, 1) {
            Some(q) => q,
            None => {
                crate::println!("[NET] tx queue unavailable");
                return;
            }
        };
        RX_Q = rxq;
        RX_USED_PA = rxu;
        TX_Q = txq;
        TX_USED_PA = txu;
        // post all RX buffers (single-desc each, device-writable)
        for i in 0..N_RXDESC {
            desc_set(rxq, i, RXB[i].as_ptr() as usize, RXBUF as u32, V::D_WRITE, 0);
        }
        {
            let mut n = NET.lock();
            // prime avail entries first, then publish idx (device order)
            for i in 0..N_RXDESC {
                V::w16(avail_pa(rxq) + 4 + i * 2, i as u16);
            }
            n.rx_avail = N_RXDESC as u16;
            V::fence();
            V::w16(avail_pa(rxq) + 2, n.rx_avail);
            V::fence();
            V::w32(b, V::R_QNOTIFY, 0); // kick rx queue
        }
        V::w32(b, V::R_STATUS, V::r32(b, V::R_STATUS) | V::ST_OK);
        for i in 0..6 {
            MAC[i] = V::r8(b, V::R_CFG_MAC + i);
        }
        READY = true;
        let (a, bb, c, d) = ip_to_str(GUEST_IP);
        crate::println!(
            "[NET] net @ {:#x} MAC {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x} ip {}.{}.{}.{}",
            b, MAC[0], MAC[1], MAC[2], MAC[3], MAC[4], MAC[5], a, bb, c, d
        );
        crate::println!("[TEST] net-dev PASS");
    }
}

// ---- TX (single-flight under NET lock, pure-MMIO poll like blk) ----
fn tx_submit_locked(n: &mut NetState, total: usize) -> bool {
    // chain: desc0 = virtio_net_hdr (10B) -> desc1 = packet. Caller built
    // TXH/TXP already. Returns true on device completion.
    unsafe {
        let b = base();
        V::w32(b, V::R_QSEL, 1);
        desc_set(TX_Q, 0, TXH.as_ptr() as usize, 10, V::D_NEXT, 1);
        desc_set(TX_Q, 1, TXP.as_ptr() as usize, total as u32, 0, 0);
        let a = avail_pa(TX_Q);
        V::w16(a + 4 + (n.tx_avail as usize % V::QDEPTH) * 2, 0);
        V::fence();
        n.tx_avail = n.tx_avail.wrapping_add(1);
        V::w16(a + 2, n.tx_avail);
        V::fence();
        V::w32(b, V::R_QNOTIFY, 1);
        // pure poll (SIE=0 here; see blk submit note -- never block_current
        // in this loop). Deadline from global ticks + local bound in case
        // all harts spin and ticks freeze.
        let u = TX_USED_PA;
        let deadline = crate::timer::ticks().wrapping_add(500);
        let mut local = 0u32;
        loop {
            V::fence();
            if V::r16(u + 2) != n.tx_used {
                break;
            }
            if crate::timer::ticks() >= deadline {
                break;
            }
            local = local.wrapping_add(1);
            if local > 100_000_000 {
                break;
            }
        }
        if V::r16(u + 2) == n.tx_used {
            return false;
        }
        V::fence();
        n.tx_used = n.tx_used.wrapping_add(1);
        V::w32(b, V::R_INTACK, V::r32(b, V::R_INTSTAT));
        V::fence();
        true
    }
}

fn eth_build(dst: &[u8; 6], etype: u16, out: &mut [u8]) {
    out[0..6].copy_from_slice(dst);
    unsafe {
        out[6..12].copy_from_slice(&MAC);
    }
    wbe16(&mut out[12..14], etype);
}

// ---- ARP ----
fn arp_request_locked(n: &mut NetState) {
    // broadcast ARP request for GW_IP. Rate-limited by caller.
    let mut f = [0u8; 42];
    eth_build(&[0xff; 6], 0x0806, &mut f);
    wbe16(&mut f[14..16], 1); // htype ethernet
    wbe16(&mut f[16..18], 0x0800); // ptype IPv4
    f[18] = 6;
    f[19] = 4;
    wbe16(&mut f[20..22], 1); // request
    unsafe {
        f[22..28].copy_from_slice(&MAC);
    }
    wbe32(&mut f[28..32], GUEST_IP);
    f[32..38].copy_from_slice(&[0; 6]);
    wbe32(&mut f[38..42], GW_IP);
    unsafe {
        TXH[..10].copy_from_slice(&[0; 10]);
        TXP[..42].copy_from_slice(&f);
    }
    n.arp_req_at = crate::timer::ticks() as u64;
    tx_submit_locked(n, 42);
}

// ---- UDP/IP transmit ----
fn udp_send_locked(n: &mut NetState, dst_ip: u32, dport: u16, sport: u16, data: &[u8]) -> bool {
    let total = 14 + 20 + 8 + data.len();
    if total > 2048 {
        return false;
    }
    let mut f = [0u8; 2048];
    eth_build(&n.gw_mac, 0x0800, &mut f);
    // IP
    f[14] = 0x45;
    f[15] = 0;
    wbe16(&mut f[16..18], (20 + 8 + data.len()) as u16);
    wbe16(&mut f[18..20], 0);
    wbe16(&mut f[20..22], 0);
    f[22] = 64;
    f[23] = 17;
    wbe16(&mut f[24..26], 0); // checksum placeholder
    wbe32(&mut f[26..30], GUEST_IP);
    wbe32(&mut f[30..34], dst_ip);
    let mut cks = ip_cksum(&f[14..34]);
    if cks == 0 {
        cks = 0xffff;
    }
    wbe16(&mut f[24..26], cks);
    // UDP (checksum 0 = none for IPv4, legal)
    wbe16(&mut f[34..36], sport);
    wbe16(&mut f[36..38], dport);
    wbe16(&mut f[38..40], (8 + data.len()) as u16);
    wbe16(&mut f[40..42], 0);
    f[42..42 + data.len()].copy_from_slice(data);
    unsafe {
        TXH[..10].copy_from_slice(&[0; 10]);
        TXP[..total].copy_from_slice(&f[..total]);
    }
    tx_submit_locked(n, total)
}

fn ip_cksum(hdr: &[u8]) -> u16 {
    let mut sum: u32 = 0;
    let mut i = 0;
    while i < hdr.len() {
        sum += be16(&hdr[i..i + 2]) as u32;
        i += 2;
    }
    while sum >> 16 != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

// ---- RX: ISR harvest + parse (under NET lock) ----
fn repost_rx(n: &mut NetState, id: usize) {
    unsafe {
        let a = avail_pa(RX_Q);
        V::w16(a + 4 + (n.rx_avail as usize % V::QDEPTH) * 2, id as u16);
        V::fence();
        n.rx_avail = n.rx_avail.wrapping_add(1);
        V::w16(a + 2, n.rx_avail);
        V::fence();
        V::w32(base(), V::R_QNOTIFY, 0);
    }
}

fn harvest_locked(n: &mut NetState) -> bool {
    // drain the RX used ring; parse + stage; repost. Returns true if any
    // packet/ARP arrived (caller wakes net sleepers after unlock).
    unsafe {
        let u = RX_USED_PA;
        let mut got = false;
        loop {
            V::fence();
            if V::r16(u + 2) == n.rx_used {
                break;
            }
            let slot = (n.rx_used as usize) % V::QDEPTH;
            let id = V::r32pa(u + 4 + slot * 8) as usize;
            let len = V::r32pa(u + 4 + slot * 8 + 4) as usize;
            n.rx_used = n.rx_used.wrapping_add(1);
            if id < N_RXDESC && len >= 10 && len <= RXBUF {
                // strip the 10B virtio_net_hdr the device prepended
                parse_frame(n, &RXB[id][10..len]);
                got = true;
            }
            if id < N_RXDESC {
                repost_rx(n, id);
            }
        }
        // TX used ring: advance (the single-flight waiter consumes via its
        // own poll; this just keeps the ring consistent + acks the IRQ).
        let tu = TX_USED_PA;
        V::fence();
        while V::r16(tu + 2) != n.tx_used {
            n.tx_used = n.tx_used.wrapping_add(1);
            got = true;
        }
        got
    }
}

fn parse_frame(n: &mut NetState, f: &[u8]) {
    if f.len() < 14 {
        return;
    }
    let etype = be16(&f[12..14]);
    if etype == 0x0806 {
        // ARP
        if f.len() < 42 {
            return;
        }
        if be16(&f[20..22]) != 2 {
            return; // not a reply
        }
        if be32(&f[28..32]) != GW_IP {
            return; // not from our gateway
        }
        if be32(&f[38..42]) != GUEST_IP {
            return; // not for us
        }
        n.gw_mac.copy_from_slice(&f[22..28]);
        n.arp_ok = true;
        return;
    }
    if etype != 0x0800 {
        return;
    }
    // IPv4
    if f.len() < 34 || f[14] >> 4 != 4 {
        return;
    }
    let ihl = ((f[14] & 0x0f) as usize) * 4;
    if ihl < 20 || f.len() < 14 + ihl + 8 {
        return;
    }
    if f[23] != 17 && f[23] != tcp::TCP_PROTO {
        return; // neither UDP nor TCP
    }
    if be32(&f[30..34]) != GUEST_IP {
        return; // not for us
    }
    if f[23] == tcp::TCP_PROTO {
        // v1.5: TCP segment length comes from the IP total-length field,
        // NOT the frame length (Ethernet pads short frames; checksumming
        // the padding breaks validation -- observed off-by-2 vs host).
        let ip_len = be16(&f[16..18]) as usize;
        if ip_len < ihl || f.len() < 14 + ip_len {
            return;
        }
        tcp::tcp_rx(n, be32(&f[26..30]), &f[14 + ihl..14 + ip_len]);
        return;
    }
    let src_ip = be32(&f[26..30]);
    let uo = 14 + ihl;
    let sport = be16(&f[uo..uo + 2]);
    let dport = be16(&f[uo + 2..uo + 4]);
    let ulen = be16(&f[uo + 4..uo + 6]) as usize;
    if ulen < 8 || f.len() < uo + ulen {
        return;
    }
    let payload = &f[uo + 8..uo + ulen];
    for s in n.socks.iter_mut() {
        if s.used && s.lport == dport {
            if s.rx.len() < 8 {
                s.rx.push_back((src_ip, sport, Vec::from(payload)));
            }
            break;
        }
    }
}

/// RX completion ISR entry (trap dispatch). No-op without a device.
/// Harvests under NET, wakes AFTER unlock (never nest NET -> sched).
pub fn on_irq() {
    if !ready() {
        return;
    }
    unsafe {
        let b = base();
        let st = V::r32(b, V::R_INTSTAT);
        if st & 1 != 0 {
            V::w32(b, V::R_INTACK, st);
            V::fence();
        }
    }
    let got = {
        let mut n = NET.lock();
        harvest_locked(&mut n)
    };
    if got {
        crate::task::wake_net();
    }
}

// ---- socket API (each takes NET internally; never hold sched across) ----
fn alloc_port(n: &mut NetState) -> u16 {
    for _ in 0..1024 {
        let p = n.next_port;
        n.next_port = n.next_port.wrapping_add(1);
        if n.next_port < 49152 {
            n.next_port = 49152;
        }
        let mut clash = false;
        for s in n.socks.iter() {
            if s.used && s.lport == p {
                clash = true;
                break;
            }
        }
        if !clash {
            return p;
        }
    }
    0
}

/// v1.5: open with kind (0 = UDP, 1 = TCP).
pub fn sock_open_kind(kind: u8) -> Option<usize> {
    let mut n = NET.lock();
    let mut free = None;
    for (i, s) in n.socks.iter().enumerate() {
        if !s.used {
            free = Some(i);
            break;
        }
    }
    let i = free?;
    let port = alloc_port(&mut n);
    if port == 0 {
        return None;
    }
    n.socks[i].used = true;
    n.socks[i].kind = kind;
    n.socks[i].closing = false;
    n.socks[i].lport = port;
    n.socks[i].peer_ip = 0;
    n.socks[i].peer_port = 0;
    n.socks[i].rx.clear();
    n.socks[i].tcp.reset();
    Some(i)
}

pub fn sock_close(idx: usize) {
    let mut n = NET.lock();
    if idx >= NSOCK || !n.socks[idx].used {
        return;
    }
    if n.socks[idx].kind == 1 {
        // v1.5: TCP close runs FIN + lingering close-state (slot retained
        // until ACK/timeout; fd is detached immediately by the caller).
        tcp::tcp_close(&mut n, idx);
        return;
    }
    n.socks[idx].used = false;
    n.socks[idx].rx.clear();
}

/// v1.5: bind a fixed local port (UDP sticky port / TCP listen prerequisite).
/// Port must be free across UDP + TCP (incl. lingering close holders).
pub fn sock_bind(idx: usize, port: u16) -> bool {
    let mut n = NET.lock();
    if idx >= NSOCK || !n.socks[idx].used || port == 0 {
        return false;
    }
    if n.socks[idx].kind == 1 {
        return tcp::tcp_bind(&mut n, idx, port);
    }
    for (j, s) in n.socks.iter().enumerate() {
        if j != idx && s.used && s.lport == port {
            return false;
        }
    }
    n.socks[idx].lport = port;
    true
}

/// v1.5: TCP listen.
pub fn sock_listen(idx: usize) -> bool {
    let mut n = NET.lock();
    tcp::tcp_listen(&mut n, idx)
}

/// v1.5: TCP accept. Returns the child sock idx (caller allocates an fd).
pub fn sock_accept(idx: usize) -> Option<usize> {
    let mut n = NET.lock();
    tcp::tcp_accept(&mut n, idx)
}

/// v1.5: commit a peeked accept (after fd allocation; see tcp_accept).
pub fn sock_accept_commit(idx: usize) {
    let mut n = NET.lock();
    tcp::tcp_accept_commit(&mut n, idx);
}

/// v1.5: per-tick TCP retransmit/timeout scan (timer ISR, 10ms). Wakes net
/// sleepers after unlock if anything completed.
pub fn tick() {
    if !ready() {
        return;
    }
    let wake = {
        let mut n = NET.lock();
        tcp::tcp_tick(&mut n, crate::timer::ticks() as u64)
    };
    if wake {
        crate::task::wake_net();
    }
}

pub fn sock_connect(idx: usize, ip_be: u32, port: u16) -> bool {
    let mut n = NET.lock();
    if idx >= NSOCK || !n.socks[idx].used {
        return false;
    }
    n.socks[idx].peer_ip = ip_be;
    n.socks[idx].peer_port = port;
    true
}

pub fn sock_send(idx: usize, data: &[u8]) -> isize {
    // -2 WouldBlock (UDP: no ARP yet; TCP: backpressure. Userspace
    // retries), -1 error, else len.
    let mut n = NET.lock();
    if idx >= NSOCK || !n.socks[idx].used {
        return -1;
    }
    if n.socks[idx].kind == 1 {
        return tcp::tcp_send(&mut n, idx, data);
    }
    drop(n);
    if data.len() > MAXUDP {
        return -1;
    }
    let mut n = NET.lock();
    if idx >= NSOCK || !n.socks[idx].used || n.socks[idx].kind != 0 {
        return -1;
    }
    if !n.arp_ok {
        // (re)send ARP request at most once/sec; caller retries
        let now = crate::timer::ticks() as u64;
        if now.wrapping_sub(n.arp_req_at) > 100 {
            arp_request_locked(&mut n);
        }
        return -2;
    }
    let (ip, port, sport) = {
        let s = &n.socks[idx];
        (s.peer_ip, s.peer_port, s.lport)
    };
    if ip == 0 || port == 0 {
        return -1; // not connected
    }
    if udp_send_locked(&mut n, ip, port, sport, data) {
        data.len() as isize
    } else {
        -1
    }
}

pub fn sock_recv(idx: usize, out: &mut [u8]) -> isize {
    // payload length, -2 WouldBlock (empty), 0 EOF (TCP peer FIN + drained),
    // -1 error. Userspace retries.
    let mut n = NET.lock();
    if idx >= NSOCK || !n.socks[idx].used {
        return -1;
    }
    if n.socks[idx].kind == 1 {
        return tcp::tcp_recv(&mut n, idx, out);
    }
    match n.socks[idx].rx.pop_front() {
        Some((_ip, _port, pkt)) => {
            let k = core::cmp::min(out.len(), pkt.len());
            out[..k].copy_from_slice(&pkt[..k]);
            k as isize
        }
        None => -2,
    }
}
