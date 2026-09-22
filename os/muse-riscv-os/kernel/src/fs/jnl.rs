// v1.6: full-data journal for MUSEFS (metadata + data, write-ahead).
// Every blk::write appends a record BEFORE the home write; dirty mounts
// replay committed records. Replay is idempotent (same lba+data), so no
// per-block version tracking is needed; torn records fail magic/crc and
// are skipped, which is exactly the un-acked-write case.
//
// v1.8: transactions. Records carry a tx id; a commit record closes each
// syscall's TX. Replay applies a TX whole or not at all (per-syscall
// atomic durability). tx=0 = legacy/unbracketed (old images + fsck-time
// writes), applied unconditionally as before.
//
// Layout (disk tail, reserved by mkfs; superblock journal_lba@52,
// journal_blocks@56; both zero = no journal, graceful):
//   slot s (0..NREC): sector A = {MAGIC_A, gen, seq, lba, crc(seq,lba,tx,data),
//                                  tx @20},
//                      sector B = 512B data.
//   commit slot: sector A = {MAGIC_C, gen, seq, tx, count, crc(tx,count,zero)},
//                sector B unused (zeroed; never read).
//   header sector (JLBA+0): {MAGIC_H, gen}.
// Base = JLBA+1; slot s occupies [base+2s, base+2s+2).
// NREC = (JB-1)/2 records per generation.
//
// Crash cuts (all correct by construction):
// - torn A/B            -> magic/crc fail -> skip (home kept old = un-acked)
// - A+B done, gen stale -> skip (clean shutdown invalidated the generation)
// - record complete     -> replay iff legacy, or its TX committed whole
// - commit torn/absent  -> whole TX skipped (home kept old = un-acked)
// - commit present      -> members applied in seq order (idempotent)
// Circular overwrite is safe: a record is only overwritten after 256 later
// jwrites, each of which completed its own home write synchronously, so the
// victim's home was already applied (write-through ordering). A TX bigger
// than the ring can never complete whole: commit() chunks it (see below).
//
// TX chunking (safety valve, never triggers in-suite): a single TX holding
// more than CHUNK records is force-committed in 200-record pieces, each a
// complete sub-TX under the same id... no -- simpler: a fresh id per chunk
// (chunk atomicity, documented). Largest in-suite single write is 64KB
// (128 records), far below the valve.

use alloc::vec::Vec;

pub const NREC: usize = 256;
// Max records per commit chunk (see above; suite max ~128).
const CHUNK: u32 = 200;

const MAGIC_A: u32 = 0x4a52_4e41; // "JRNa"
const MAGIC_C: u32 = 0x4a52_4e43; // "JRNc"
const MAGIC_H: u32 = 0x4a52_4e31; // "JRN1"

fn r32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}
fn w32(b: &mut [u8], o: usize, v: u32) {
    b[o..o + 4].copy_from_slice(&v.to_le_bytes());
}

/// crc32 (IEEE, bitwise; 512B in ~4k cycles -- fine at journal rates).
/// Binds (seq, lba, tx, data) so torn mixes of slot generations can never
/// validate: any partial overwrite fails. (v1.8: tx added to the bind.)
pub fn jnl_crc(seq: u32, lba: u32, tx: u32, data: &[u8; 512]) -> u32 {
    let mut crc: u32 = 0xffff_ffff;
    for &b in seq
        .to_le_bytes()
        .iter()
        .chain(lba.to_le_bytes().iter())
        .chain(tx.to_le_bytes().iter())
        .chain(data.iter())
    {
        crc ^= b as u32;
        for _ in 0..8 {
            let m = if crc & 1 != 0 { 0xedb8_8320 } else { 0 };
            crc = (crc >> 1) ^ m;
        }
    }
    !crc
}

struct JLog {
    next: u32,
    gen: u32,
    // v1.8: open syscall transaction (all records between begin/commit).
    depth: u32,
    tx: u32,
    tx_count: u32,
    tx_next: u32,
}

static JLOG: crate::sync::SpinMutex<JLog> = crate::sync::SpinMutex::new(JLog {
    next: 0,
    gen: 0,
    depth: 0,
    tx: 0,
    tx_count: 0,
    tx_next: 1,
});
static mut JLBA: u32 = 0;
static mut JB: u32 = 0;
static mut ENABLED: bool = false;

fn jlba() -> u32 {
    unsafe { JLBA }
}
fn nrec() -> usize {
    (unsafe { JB } as usize).saturating_sub(1) / 2
}
pub fn enabled() -> bool {
    unsafe { ENABLED }
}

fn raw_read(lba: u32, out: &mut [u8; 512]) {
    // Bypass cache+journal (recovery/boot paths; cache is empty then).
    // blk::read would consult the (empty) cache then device -- same result,
    // but raw keeps recovery independent of cache state. Use device direct.
    let mut tmp = [0u8; 512];
    if crate::fs::virtio::read_block(lba, &mut tmp) {
        out.copy_from_slice(&tmp);
    }
}

fn raw_write(lba: u32, data: &[u8; 512]) {
    crate::fs::virtio::write_block(lba, data);
}

/// Mount-time init: locate journal from superblock fields, adopt or create
/// the header generation. Returns max committed seq (caller seeds next).
pub fn init(jlba: u32, jb: u32) -> u32 {
    unsafe {
        JLBA = jlba;
        JB = jb;
    }
    if jlba == 0 || jb < 3 {
        return 0; // no journal on this image: degrade gracefully
    }
    let mut h = [0u8; 512];
    raw_read(jlba, &mut h);
    let gen = if r32(&h, 0) == MAGIC_H {
        let g = r32(&h, 4);
        if g == 0 {
            1
        } else {
            g
        }
    } else {
        // fresh area: stamp generation 1
        let mut nh = [0u8; 512];
        w32(&mut nh, 0, MAGIC_H);
        w32(&mut nh, 4, 1);
        raw_write(jlba, &nh);
        1
    };
    // continue seq past any surviving records (ordering across mounts)
    let max = max_seq(gen);
    {
        let mut j = JLOG.lock();
        j.gen = gen;
        j.next = max.wrapping_add(1);
        if j.next == 0 {
            j.next = 1; // seq 0 reserved (fresh-slot marker value)
        }
    }
    unsafe {
        ENABLED = true;
    }
    max
}

fn slot_base(slot: usize) -> u32 {
    jlba() + 1 + (slot as u32) * 2
}

fn max_seq(gen: u32) -> u32 {
    let n = nrec().min(NREC);
    let mut max = 0u32;
    let mut seen = false;
    for s in 0..n {
        let mut a = [0u8; 512];
        raw_read(slot_base(s), &mut a);
        // v1.8: data and commit records share the seq space; both count
        // (a reused seq would collide with a live slot otherwise).
        let magic = r32(&a, 0);
        if magic != MAGIC_A && magic != MAGIC_C {
            continue;
        }
        if r32(&a, 4) != gen {
            continue;
        }
        let seq = r32(&a, 8);
        if seq == 0 {
            continue;
        }
        if !seen || seq.wrapping_sub(max) < 0x8000_0000 {
            // seq order with wrap tolerance: take max in seq-space
            if !seen || seq > max {
                max = seq;
            }
            seen = true;
        }
    }
    if seen {
        max
    } else {
        0
    }
}

/// Append one record for (lba, data). Called from blk::write; the home
/// write happens after we return (caller). Complete in ~2 raw sector
/// writes; holds JLOG across them (short, never blocks: raw virtio polls).
/// Tags the open syscall TX, if any (tx=0 = legacy).
pub fn record(lba: u32, data: &[u8; 512]) {
    if !enabled() {
        return;
    }
    let n = nrec().min(NREC);
    if n == 0 {
        return;
    }
    let (seq, gen, tx) = {
        let mut j = JLOG.lock();
        let seq = j.next;
        j.next = j.next.wrapping_add(1);
        if j.next == 0 {
            j.next = 1;
        }
        if j.depth == 0 {
            (seq, j.gen, 0)
        } else {
            // v1.8: chunk valve -- force-commit before the ring could
            // overflow inside one TX (keeps every committed unit whole).
            // The commit itself runs after we drop the guard (it allocates
            // its own higher seq; replay orders by seq, so grouping holds).
            let mut pending: Option<(u32, u32, u32)> = None;
            if j.tx_count >= CHUNK {
                pending = Some((j.gen, j.tx, j.tx_count));
                // fresh chunk id (documented chunk-atomic semantics)
                j.tx = j.tx_next;
                j.tx_next = j.tx_next.wrapping_add(1);
                if j.tx_next == 0 {
                    j.tx_next = 1;
                }
                j.tx_count = 0;
            }
            j.tx_count += 1; // this record counts toward the (new) chunk
            let out = (seq, j.gen, j.tx, pending);
            drop(j);
            if let Some((g, t, c)) = out.3 {
                write_commit(g, t, c);
            }
            (out.0, out.1, out.2)
        }
    };
    let slot = (seq as usize) % n;
    let base = slot_base(slot);
    let mut a = [0u8; 512];
    w32(&mut a, 0, MAGIC_A);
    w32(&mut a, 4, gen);
    w32(&mut a, 8, seq);
    w32(&mut a, 12, lba);
    w32(&mut a, 16, jnl_crc(seq, lba, tx, data));
    w32(&mut a, 20, tx);
    raw_write(base, &a);
    raw_write(base + 1, data);
}

/// Open a syscall transaction (pairs with commit(); syscalls don't nest,
/// depth is belt-and-braces). Records until commit share one tx id.
pub fn begin() {
    if !enabled() {
        return;
    }
    let mut j = JLOG.lock();
    if j.depth == 0 {
        j.tx = j.tx_next;
        j.tx_next = j.tx_next.wrapping_add(1);
        if j.tx_next == 0 {
            j.tx_next = 1;
        }
        j.tx_count = 0;
    }
    j.depth += 1;
}

/// Close a syscall transaction. Writes one commit record iff the TX holds
/// any records (console/pipe writes cost nothing extra).
pub fn commit() {
    if !enabled() {
        return;
    }
    let (tx, count, gen) = {
        let mut j = JLOG.lock();
        if j.depth == 0 {
            return; // unbalanced: never happens; fail open (legacy path)
        }
        j.depth -= 1;
        if j.depth > 0 {
            return; // nested (never happens); outer commit writes
        }
        let (tx, count, gen) = (j.tx, j.tx_count, j.gen);
        j.tx = 0;
        j.tx_count = 0;
        (tx, count, gen)
    };
    if count > 0 {
        write_commit(gen, tx, count);
    }
}

fn write_commit(gen: u32, tx: u32, count: u32) {
    let n = nrec().min(NREC);
    if n == 0 || tx == 0 {
        return;
    }
    let seq = {
        let mut j = JLOG.lock();
        let s = j.next;
        j.next = j.next.wrapping_add(1);
        if j.next == 0 {
            j.next = 1;
        }
        s
    };
    let slot = (seq as usize) % n;
    let base = slot_base(slot);
    let mut a = [0u8; 512];
    w32(&mut a, 0, MAGIC_C);
    w32(&mut a, 4, gen);
    w32(&mut a, 8, seq);
    w32(&mut a, 12, tx);
    w32(&mut a, 16, count);
    w32(&mut a, 20, jnl_crc(tx, count, 0xffff_ffff, &[0u8; 512]));
    raw_write(base, &a);
    let zero = [0u8; 512];
    raw_write(base + 1, &zero);
}

/// Replay the current generation in seq order. Groups by TX: legacy
/// (tx=0) records apply unconditionally; a TX applies whole or not at
/// all (commit present + member count matches). Idempotent: safe on every
/// dirty mount. Returns (applied, max).
pub fn replay() -> (usize, u32) {
    let n = nrec().min(NREC);
    let gen = JLOG.lock().gen;
    // (seq, lba, data) for legacy; TX members staged per tx first.
    let mut legacy: Vec<(u32, u32, [u8; 512])> = Vec::new();
    // tx -> (members, committed?, count)
    let mut txs: Vec<(u32, Vec<(u32, u32, [u8; 512])>, bool, u32)> = Vec::new();
    for s in 0..n {
        let mut a = [0u8; 512];
        raw_read(slot_base(s), &mut a);
        let magic = r32(&a, 0);
        if magic != MAGIC_A && magic != MAGIC_C {
            continue;
        }
        if r32(&a, 4) != gen {
            continue;
        }
        let seq = r32(&a, 8);
        if seq == 0 {
            continue;
        }
        if magic == MAGIC_C {
            // commit: {tx @12, count @16, crc(tx,count,zero) @20}
            let tx = r32(&a, 12);
            let count = r32(&a, 16);
            if tx == 0 || jnl_crc(tx, count, 0xffff_ffff, &[0u8; 512]) != r32(&a, 20) {
                continue; // torn commit: whole TX skipped
            }
            let mut found = false;
            for t in txs.iter_mut() {
                if t.0 == tx {
                    t.2 = true;
                    t.3 = count;
                    found = true;
                    break;
                }
            }
            if !found {
                txs.push((tx, Vec::new(), true, count));
            }
            continue;
        }
        // data record
        let lba = r32(&a, 12);
        let crc = r32(&a, 16);
        let tx = r32(&a, 20);
        let mut b = [0u8; 512];
        raw_read(slot_base(s) + 1, &mut b);
        if jnl_crc(seq, lba, tx, &b) != crc {
            continue; // torn record: home kept the un-acked old version
        }
        if tx == 0 {
            legacy.push((seq, lba, b));
            continue;
        }
        let mut found = false;
        for t in txs.iter_mut() {
            if t.0 == tx {
                t.1.push((seq, lba, b));
                found = true;
                break;
            }
        }
        if !found {
            let mut v = Vec::new();
            v.push((seq, lba, b));
            txs.push((tx, v, false, 0));
        }
    }
    // flatten: legacy + complete TXs only (count must match exactly)
    let mut recs: Vec<(u32, u32, [u8; 512])> = legacy;
    for (_, members, committed, count) in txs.iter() {
        if *committed && members.len() as u32 == *count {
            recs.extend(members.iter().cloned());
        }
    }
    recs.sort_by(|x, y| x.0.cmp(&y.0));
    let max = recs.last().map(|r| r.0).unwrap_or(0);
    let count = recs.len();
    for (_, lba, data) in recs.iter() {
        raw_write(*lba, data);
    }
    // continue the sequence past replayed records
    if max != 0 {
        let mut j = JLOG.lock();
        // only move forward (a concurrent record() may have advanced further)
        if max.wrapping_sub(j.next) < 0x8000_0000 && max >= j.next {
            j.next = max.wrapping_add(1);
            if j.next == 0 {
                j.next = 1;
            }
        }
    }
    (count, max)
}

/// Clean shutdown: bump the generation (all old records instantly void,
/// no wipe needed), then the caller clears the dirty flag.
pub fn reset() {
    if !enabled() {
        return;
    }
    let mut j = JLOG.lock();
    j.gen = j.gen.wrapping_add(1);
    if j.gen == 0 {
        j.gen = 1;
    }
    j.next = 1;
    let gen = j.gen;
    drop(j);
    let mut h = [0u8; 512];
    w32(&mut h, 0, MAGIC_H);
    w32(&mut h, 4, gen);
    raw_write(jlba(), &h);
}
// NOTE (house convention, see lib.rs header): host-testable pure logic is
// mirrored in kernel/src/lib.rs with #[cfg(test)] tests there. This file's
// device-coupled parts are covered by the §11 power-loss integration test.
// The lib.rs mirror covers: jnl_crc field binding + record layout roundtrip
// + torn-data rejection (same assertions, mirrored code).
