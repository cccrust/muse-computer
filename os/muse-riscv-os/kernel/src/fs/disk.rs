// MUSEFS disk filesystem (block-backed, via blk cache).
// Layout MUST match tools/mkfs/src/main.rs:
//   LBA0: superblock magic[8]=MUSEFS01 bs u32 nblocks u32 bmap_lba u32
//         bmap_blocks u32 ino_lba u32 ino_blocks u32 ninodes u32
//         data_lba u32 root_ino u32
//         [v1.6] journal_lba u32@52 journal_blocks u32@56 (both zero = none)
//   bitmap: 1 bit/block. inodes: 64B (kind u8, pad[3], size u32,
//         direct[8] u32, indirect u32, nlink u32 [v2+], dind u32 [v2.5+]).
//         dirent: name[28] + ino u32.

use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};

const BLOCK: usize = 512;
const MAGIC: &[u8; 8] = b"MUSEFS01";
const KIND_FILE: u8 = 1;
const KIND_DIR: u8 = 2;
const NAME_LEN: usize = 28;
const DENT: usize = 32;
const INO_SIZE: usize = 64;

static MOUNTED: AtomicBool = AtomicBool::new(false);
static mut NBLOCKS: u32 = 0;
static mut BMAP_LBA: u32 = 0;
static mut INO_LBA: u32 = 0;
static mut NINODES: u32 = 0;
static mut ROOT_INO: u32 = 1;
static mut DATA_LBA: u32 = 0;
// v1.6: journal area (disk tail); zero = no journal on this image.
static mut JLBA: u32 = 0;
static mut JB: u32 = 0;

pub fn journal_lba() -> u32 {
    unsafe { JLBA }
}
pub fn journal_blocks() -> u32 {
    unsafe { JB }
}
fn in_journal(lba: u32) -> bool {
    let (j, n) = unsafe { (JLBA, JB) };
    n != 0 && lba >= j && lba < j + n
}

fn r32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}
fn w32(b: &mut [u8], o: usize, v: u32) {
    b[o..o + 4].copy_from_slice(&v.to_le_bytes());
}

pub fn is_mounted() -> bool {
    MOUNTED.load(Ordering::Acquire)
}

pub fn mount() -> bool {
    let mut sb = [0u8; 512];
    crate::fs::blk::read(0, &mut sb);
    if &sb[..8] != MAGIC || r32(&sb, 8) != BLOCK as u32 {
        return false;
    }    unsafe {
        NBLOCKS = r32(&sb, 12);
        BMAP_LBA = r32(&sb, 16);
        INO_LBA = r32(&sb, 24);
        NINODES = r32(&sb, 32);
        DATA_LBA = r32(&sb, 36);
        ROOT_INO = r32(&sb, 40);
        if ROOT_INO == 0 {
            ROOT_INO = 1;
        }
        // v1.6: journal area (zero = none, graceful on old images)
        JLBA = r32(&sb, 52);
        JB = r32(&sb, 56);
        if JLBA == 0 || JB < 3 || JLBA + JB > NBLOCKS {
            JLBA = 0;
            JB = 0;
        }
    }
    // verify root is a dir
    if get_ino(root()).is_none() {
        return false;
    }
    MOUNTED.store(true, Ordering::Release);
    crate::println!("[FS] disk mount ok (MUSEFS01, {} blocks)", nblocks());
    // v1.6: journal-first recovery (before fsck-lite; replay is idempotent).
    // init() also seeds the record sequence past surviving records.
    if journal_blocks() != 0 {
        crate::fs::jnl::init(journal_lba(), journal_blocks());
    }
    // fsck-lite: rebuild bitmap if dirty, then clear flag
    let mut sb2 = [0u8; 512];
    crate::fs::blk::read(0, &mut sb2);
    if r32(&sb2, 48) != 0 {
        if journal_blocks() != 0 {
            let (n, _) = crate::fs::jnl::replay();
            crate::println!("[FS] journal replayed {}", n);
        }
        rebuild_bitmap();
        set_dirty(false);
        crate::println!("[FS] fsck: bitmap rebuilt");
    }
    // v1.6: mark dirty AFTER the recovery above (a mount that never cleanly
    // shuts down must replay next time; a clean halt clears it). Without
    // this, kill -9 mounts "clean" and replays nothing (observed).
    set_dirty(true);
    true
}

/// Set superblock dirty flag (false = clean shutdown).
pub fn set_dirty(v: bool) {
    let mut sb = [0u8; 512];
    crate::fs::blk::read(0, &mut sb);
    w32(&mut sb, 48, if v { 1 } else { 0 });
    crate::fs::blk::write(0, &sb);
}

/// Rebuild block bitmap from inode scan (fsck-lite).
fn rebuild_bitmap() {
    let nb = nblocks();
    // clear bitmap area
    let b0 = bmap_lba();
    let b1 = b0 + ((nb / 8 + BLOCK as u32 - 1) / BLOCK as u32);
    let zero = [0u8; 512];
    let mut lb = b0;
    while lb < b1 {
        crate::fs::blk::write(lb, &zero);
        lb += 1;
    }
    let mut mark = |lba: u32| {
        let byte = (lba / 8) as usize;
        let lb = bmap_lba() + (byte / BLOCK) as u32;
        let off = byte % BLOCK;
        let mut blk = [0u8; 512];
        crate::fs::blk::read(lb, &mut blk);
        blk[off] |= 1 << (lba % 8);
        crate::fs::blk::write(lb, &blk);
    };
    // reserved: superblock + bitmap + inode table
    let mut b = 0u32;
    while b < data_start() {
        mark(b);
        b += 1;
    }
    // v1.6: journal area is always used (keeps bitmap honest; balloc and
    // free_blocks also exclude it by range as belt-and-braces)
    {
        let (j, n) = unsafe { (JLBA, JB) };
        let mut k = 0u32;
        while k < n {
            mark(j + k);
            k += 1;
        }
    }
    // all inode data blocks (v2.5: must cover the dind tree too --
    // unmarked dind blocks get re-balloc'd on first write and clobber
    // big files; observed as "cannot open /bin/sh" right after pull).
    let mut ino = 1u32;
    while ino <= ninodes() {
        if let Some(rec) = get_ino_raw(ino) {
            for i in 0..8 {
                if rec.direct[i] != 0 {
                    mark(rec.direct[i]);
                }
            }
            if rec.indirect != 0 {
                mark(rec.indirect);
                let mut ib = [0u8; 512];
                crate::fs::blk::read(rec.indirect, &mut ib);
                let mut k = 0;
                while k < BLOCK / 4 {
                    let db = r32(&ib, k * 4);
                    if db != 0 {
                        mark(db);
                    }
                    k += 1;
                }
            }
            if rec.dind != 0 {
                mark(rec.dind);
                let mut lb = [0u8; 512];
                crate::fs::blk::read(rec.dind, &mut lb);
                let mut k = 0;
                while k < BLOCK / 4 {
                    let l1b = r32(&lb, k * 4);
                    if l1b != 0 {
                        mark(l1b);
                        let mut l2b = [0u8; 512];
                        crate::fs::blk::read(l1b, &mut l2b);
                        let mut j = 0;
                        while j < BLOCK / 4 {
                            let db = r32(&l2b, j * 4);
                            if db != 0 {
                                mark(db);
                            }
                            j += 1;
                        }
                    }
                    k += 1;
                }
            }
        }
        ino += 1;
    }
}

// raw inode read (no kind validation; used by rebuild)
fn get_ino_raw(ino: u32) -> Option<Ino> {
    if ino == 0 || ino > ninodes() {
        return None;
    }
    let (lba, off) = ino_pos(ino);
    let mut b = [0u8; 512];
    crate::fs::blk::read(lba, &mut b);
    if b[off] != KIND_FILE && b[off] != KIND_DIR {
        return None;
    }
    let mut direct = [0u32; 8];
    for i in 0..8 {
        direct[i] = r32(&b, off + 8 + i * 4);
    }
    let nl = r32(&b, off + 44);
    Some(Ino {
        kind: b[off],
        size: r32(&b, off + 4),
        direct,
        indirect: r32(&b, off + 40),
        nlink: if nl == 0 { 1 } else { nl },
        dind: r32(&b, off + 48),
    })
}

fn nblocks() -> u32 {
    unsafe { NBLOCKS }
}

/// v0.5: public block stats for SYS_FSSTAT/`df`.
pub fn total_blocks() -> u32 {
    nblocks()
}

/// Count free blocks by scanning the bitmap (4MB image = 8K blocks; fine on demand).
pub fn free_blocks() -> u32 {
    let nb = nblocks();
    if nb == 0 {
        return 0;
    }
    let mut free = 0u32;
    let mut b = 0u32;
    while b < nb {
        // v1.6: journal blocks are never free (even if bitmap says so)
        if in_journal(b) {
            b += 1;
            continue;
        }
        let byte = (b / 8) as usize;
        let lb = bmap_lba() + (byte / BLOCK) as u32;
        let off = byte % BLOCK;
        let mut blk = [0u8; 512];
        crate::fs::blk::read(lb, &mut blk);
        let v = blk[off];
        let mut bit = 0u32;
        while bit < 8 && b + bit < nb {
            if v & (1 << bit) == 0 {
                free += 1;
            }
            bit += 1;
        }
        b += bit;
    }
    free
}
fn bmap_lba() -> u32 {
    unsafe { BMAP_LBA }
}
fn ino_lba() -> u32 {
    unsafe { INO_LBA }
}
fn ninodes() -> u32 {
    unsafe { NINODES }
}
fn root() -> u32 {
    unsafe { ROOT_INO }
}
fn data_start() -> u32 {
    unsafe { DATA_LBA }
}

struct Ino {
    kind: u8,
    size: u32,
    direct: [u32; 8],
    indirect: u32,
    nlink: u32,
    // v2.5: double-indirect root (on-disk +48, was spare; 0 = none).
    // Old images read 0 here (single-indirect behavior, bit-for-bit).
    dind: u32,
}

fn ino_pos(ino: u32) -> (u32, usize) {
    (ino_lba() + (ino - 1) / 8, ((ino - 1) % 8) as usize * INO_SIZE)
}

fn get_ino(ino: u32) -> Option<Ino> {
    if ino == 0 || ino > ninodes() {
        return None;
    }
    let (lba, off) = ino_pos(ino);
    let mut b = [0u8; 512];
    crate::fs::blk::read(lba, &mut b);
    let kind = b[off];
    if kind != KIND_FILE && kind != KIND_DIR {
        return None;
    }
    let mut direct = [0u32; 8];
    for i in 0..8 {
        direct[i] = r32(&b, off + 8 + i * 4);
    }
    // nlink at +44 (v2+); old images read 0 -> treat as 1.
    // dind at +48 (v2.5+); old images read 0 -> no second level.
    let nl = r32(&b, off + 44);
    Some(Ino {
        kind,
        size: r32(&b, off + 4),
        direct,
        indirect: r32(&b, off + 40),
        nlink: if nl == 0 { 1 } else { nl },
        dind: r32(&b, off + 48),
    })
}

fn put_ino(ino: u32, rec: &Ino) {
    let (lba, off) = ino_pos(ino);
    let mut b = [0u8; 512];
    crate::fs::blk::read(lba, &mut b);
    b[off] = rec.kind;
    b[off + 1..off + 4].copy_from_slice(&[0; 3]);
    w32(&mut b, off + 4, rec.size);
    for i in 0..8 {
        w32(&mut b, off + 8 + i * 4, rec.direct[i]);
    }
    w32(&mut b, off + 40, rec.indirect);
    w32(&mut b, off + 44, rec.nlink);
    w32(&mut b, off + 48, rec.dind);
    crate::fs::blk::write(lba, &b);
}

/// data block lba for file-block idx; alloc=true creates missing blocks.
fn data_block(rec: &mut Ino, ino: u32, idx: u32, alloc: bool) -> Option<u32> {
    if idx < 8 {
        if rec.direct[idx as usize] == 0 && alloc {
            let b = balloc()?;
            rec.direct[idx as usize] = b;
            put_ino(ino, rec);
        }
        let b = rec.direct[idx as usize];
        if b == 0 {
            return None;
        }
        Some(b)
    } else {
        let ii = idx - 8;
        if (ii as usize) < BLOCK / 4 {
            if rec.indirect == 0 {
                if !alloc {
                    return None;
                }
                let b = balloc()?;
                rec.indirect = b;
                // zero new indirect block (balloc zeroes? device blocks may hold
                // stale data from previous image use: explicitly zero)
                crate::fs::blk::write(b, &[0u8; 512]);
                put_ino(ino, rec);
            }
            let mut ib = [0u8; 512];
            crate::fs::blk::read(rec.indirect, &mut ib);
            let mut b = r32(&ib, ii as usize * 4);
            if b == 0 {
                if !alloc {
                    return None;
                }
                b = balloc()?;
                w32(&mut ib, ii as usize * 4, b);
                crate::fs::blk::write(rec.indirect, &ib);
            }
            Some(b)
        } else {
            // v2.5: double-indirect. ii2 < 128*128 (bigger than the disk).
            let ii2 = ii - (BLOCK / 4) as u32;
            if ii2 as usize >= (BLOCK / 4) * (BLOCK / 4) {
                return None;
            }
            let l1 = (ii2 as usize) / (BLOCK / 4);
            let l2 = (ii2 as usize) % (BLOCK / 4);
            if rec.dind == 0 {
                if !alloc {
                    return None;
                }
                let b = balloc()?;
                rec.dind = b;
                crate::fs::blk::write(b, &[0u8; 512]);
                put_ino(ino, rec);
            }
            let mut lb = [0u8; 512];
            crate::fs::blk::read(rec.dind, &mut lb);
            let mut l1b = r32(&lb, l1 * 4);
            if l1b == 0 {
                if !alloc {
                    return None;
                }
                l1b = balloc()?;
                w32(&mut lb, l1 * 4, l1b);
                crate::fs::blk::write(rec.dind, &lb);
            }
            let mut l2b = [0u8; 512];
            crate::fs::blk::read(l1b, &mut l2b);
            let mut b = r32(&l2b, l2 * 4);
            if b == 0 {
                if !alloc {
                    return None;
                }
                b = balloc()?;
                w32(&mut l2b, l2 * 4, b);
                crate::fs::blk::write(l1b, &l2b);
            }
            Some(b)
        }
    }
}

fn balloc() -> Option<u32> {
    let nb = nblocks();
    for b in 0..nb {
        // v1.6: never hand out journal blocks (log corruption)
        if in_journal(b) {
            continue;
        }
        let byte = (b / 8) as usize;
        let lb = bmap_lba() + (byte / BLOCK) as u32;
        let off = byte % BLOCK;
        let mut blk = [0u8; 512];
        crate::fs::blk::read(lb, &mut blk);
        if blk[off] & (1 << (b % 8)) == 0 {
            blk[off] |= 1 << (b % 8);
            crate::fs::blk::write(lb, &blk);
            // zero data block (caller may rely on zeroed tail)
            crate::fs::blk::write(b, &[0u8; 512]);
            return Some(b);
        }
    }
    None
}

fn bfree(lba: u32) {
    let byte = (lba / 8) as usize;
    let lb = bmap_lba() + (byte / BLOCK) as u32;
    let off = byte % BLOCK;
    let mut blk = [0u8; 512];
    crate::fs::blk::read(lb, &mut blk);
    blk[off] &= !(1 << (lba % 8));
    crate::fs::blk::write(lb, &blk);
}

fn free_ino_blocks(rec: &Ino) {
    for i in 0..8 {
        if rec.direct[i] != 0 {
            bfree(rec.direct[i]);
        }
    }
    if rec.indirect != 0 {
        let mut ib = [0u8; 512];
        crate::fs::blk::read(rec.indirect, &mut ib);
        for i in 0..BLOCK / 4 {
            let b = r32(&ib, i * 4);
            if b != 0 {
                bfree(b);
            }
        }
        bfree(rec.indirect);
    }
    // v2.5: free the double-indirect tree (leaves, then l1 blocks,
    // then the root). Zero entries are skipped, like above.
    if rec.dind != 0 {
        let mut lb = [0u8; 512];
        crate::fs::blk::read(rec.dind, &mut lb);
        for i in 0..BLOCK / 4 {
            let l1b = r32(&lb, i * 4);
            if l1b != 0 {
                let mut l2b = [0u8; 512];
                crate::fs::blk::read(l1b, &mut l2b);
                for j in 0..BLOCK / 4 {
                    let b = r32(&l2b, j * 4);
                    if b != 0 {
                        bfree(b);
                    }
                }
                bfree(l1b);
            }
        }
        bfree(rec.dind);
    }
}

/// Truncate file to zero length (keep inode).
pub fn truncate_path(path: &str) -> bool {
    let ino = match walk(path) {
        Some(i) => i,
        None => return false,
    };
    let mut rec = match get_ino(ino) {
        Some(r) => r,
        None => return false,
    };
    if rec.kind != KIND_FILE {
        return false;
    }
    free_ino_blocks(&rec);
    rec.size = 0;
    rec.direct = [0; 8];
    rec.indirect = 0;
    rec.dind = 0;
    put_ino(ino, &rec);
    true
}

pub fn lookup(parent: u32, name: &str) -> Option<u32> {
    if name.len() > NAME_LEN {
        return None;
    }
    let rec = get_ino(parent)?;
    if rec.kind != KIND_DIR {
        return None;
    }
    let n = rec.size as usize / DENT;
    let mut tmp = rec;
    for i in 0..n {
        let bi = (i * DENT / BLOCK) as u32;
        let b = data_block(&mut tmp, parent, bi, false)?;
        let mut blk = [0u8; 512];
        crate::fs::blk::read(b, &mut blk);
        let o = (i * DENT) % BLOCK;
        if blk[o] == 0 {
            continue;
        }
        let mut nb = 0;
        while nb < NAME_LEN && blk[o + nb] != 0 {
            nb += 1;
        }
        if &blk[o..o + nb] == name.as_bytes() {
            return Some(r32(&blk, o + NAME_LEN));
        }
    }
    None
}

pub fn walk(path: &str) -> Option<u32> {
    if !path.starts_with('/') {
        return None;
    }
    let mut cur = root();
    for comp in path.split('/').filter(|s| !s.is_empty()) {
        if comp.len() > NAME_LEN {
            return None;
        }
        // lookup() requires the parent to be a dir, so intermediate
        // components are enforced as dirs structurally.
        cur = lookup(cur, comp)?;
        get_ino(cur)?;
    }
    Some(cur)
}

fn split_parent(path: &str) -> Option<(u32, &str)> {
    let path = path.trim_end_matches('/');
    let (pp, name) = path.rsplit_once('/')?;
    let parent = if pp.is_empty() { root() } else { walk(pp)? };
    if name.is_empty() || name.len() > NAME_LEN {
        return None;
    }
    Some((parent, name))
}

pub fn read_file(path: &str) -> Option<Vec<u8>> {
    let ino = walk(path)?;
    let rec = get_ino(ino)?;
    if rec.kind != KIND_FILE {
        return None;
    }
    let mut out = Vec::with_capacity(rec.size as usize);
    let mut tmp = rec;
    let mut got = 0u32;
    let mut idx = 0u32;
    while got < tmp.size {
        let b = data_block(&mut tmp, ino, idx, false)?;
        let mut blk = [0u8; 512];
        crate::fs::blk::read(b, &mut blk);
        let n = core::cmp::min(BLOCK as u32, tmp.size - got) as usize;
        out.extend_from_slice(&blk[..n]);
        got += n as u32;
        idx += 1;
    }
    Some(out)
}

pub fn read_at(path: &str, off: usize, buf: &mut [u8]) -> usize {
    let ino = match walk(path) {
        Some(i) => i,
        None => return 0,
    };
    let mut rec = match get_ino(ino) {
        Some(r) => r,
        None => return 0,
    };
    if rec.kind != KIND_FILE {
        return 0;
    }
    let mut n = 0;
    while n < buf.len() && off + n < rec.size as usize {
        let pos = off + n;
        let b = match data_block(&mut rec, ino, (pos / BLOCK) as u32, false) {
            Some(x) => x,
            None => break,
        };
        let mut blk = [0u8; 512];
        crate::fs::blk::read(b, &mut blk);
        let bo = pos % BLOCK;
        let chunk = core::cmp::min(buf.len() - n, BLOCK - bo);
        let chunk = core::cmp::min(chunk, rec.size as usize - pos);
        buf[n..n + chunk].copy_from_slice(&blk[bo..bo + chunk]);
        n += chunk;
    }
    n
}

pub fn write_at(path: &str, off: usize, buf: &[u8]) -> usize {
    let ino = match walk(path) {
        Some(i) => i,
        None => return 0,
    };
    let mut rec = match get_ino(ino) {
        Some(r) => r,
        None => return 0,
    };
    if rec.kind != KIND_FILE {
        return 0;
    }
    let mut n = 0;
    while n < buf.len() {
        let pos = off + n;
        let b = match data_block(&mut rec, ino, (pos / BLOCK) as u32, true) {
            Some(x) => x,
            None => break,
        };
        let mut blk = [0u8; 512];
        crate::fs::blk::read(b, &mut blk);
        let bo = pos % BLOCK;
        let chunk = core::cmp::min(buf.len() - n, BLOCK - bo);
        blk[bo..bo + chunk].copy_from_slice(&buf[n..n + chunk]);
        crate::fs::blk::write(b, &blk);
        n += chunk;
    }
    if (off + n) as u32 > rec.size {
        rec.size = (off + n) as u32;
        put_ino(ino, &rec);
    }
    n
}

/// Create empty file (for O_CREATE). Parent must exist.
pub fn create_empty(path: &str) -> bool {
    if walk(path).is_some() {
        return true;
    }
    let (parent, name) = match split_parent(path) {
        Some(x) => x,
        None => return false,
    };
    // alloc inode
    let mut new_ino = 0u32;
    for i in 1..=ninodes() {
        let (lba, off) = ino_pos(i);
        let mut b = [0u8; 512];
        crate::fs::blk::read(lba, &mut b);
        if b[off] == 0 {
            // v2.3: reset the whole record, not just kind/nlink. unlink
            // frees data blocks but leaves stale size/direct/indirect;
            // reusing them would alias blocks across files (observed:
            // `ctr pull` unlinks a 60KB tar, the next create inherited
            // its size + freed blocks). Fresh inodes must be empty.
            b[off] = KIND_FILE;
            w32(&mut b, off + 4, 0); // size
            for k in 0..8 {
                w32(&mut b, off + 8 + k * 4, 0); // direct
            }
            w32(&mut b, off + 40, 0); // indirect
            w32(&mut b, off + 44, 1); // nlink
            w32(&mut b, off + 48, 0); // dind (v2.5: stale 2nd-level root
                                      // would alias like v2.3's indirect)
            crate::fs::blk::write(lba, &b);
            new_ino = i;
            break;
        }
    }
    if new_ino == 0 {
        return false;
    }
    dir_insert(parent, name, new_ino)
}

fn dir_insert(parent: u32, name: &str, child: u32) -> bool {
    let mut rec = match get_ino(parent) {
        Some(r) => r,
        None => return false,
    };
    if rec.kind != KIND_DIR {
        return false;
    }
    // find free slot across existing blocks
    let n = rec.size as usize / DENT;
    for i in 0..n {
        let bi = (i * DENT / BLOCK) as u32;
        let b = match data_block(&mut rec, parent, bi, false) {
            Some(x) => x,
            None => return false,
        };
        let mut blk = [0u8; 512];
        crate::fs::blk::read(b, &mut blk);
        let o = (i * DENT) % BLOCK;
        if blk[o] == 0 {
            blk[o..o + name.len()].copy_from_slice(name.as_bytes());
            w32(&mut blk, o + NAME_LEN, child);
            crate::fs::blk::write(b, &blk);
            return true;
        }
    }
    // append new block
    let bi = (n * DENT / BLOCK) as u32;
    let b = match data_block(&mut rec, parent, bi, true) {
        Some(x) => x,
        None => return false,
    };
    let mut blk = [0u8; 512];
    crate::fs::blk::read(b, &mut blk);
    let o = (n * DENT) % BLOCK;
    blk[o..o + name.len()].copy_from_slice(name.as_bytes());
    w32(&mut blk, o + NAME_LEN, child);
    crate::fs::blk::write(b, &blk);
    rec.size += DENT as u32;
    put_ino(parent, &rec);
    true
}

pub fn list_dir(path: &str) -> Vec<String> {
    let mut out = Vec::new();
    let ino = match walk(path) {
        Some(i) => i,
        None => return out,
    };
    let mut rec = match get_ino(ino) {
        Some(r) => r,
        None => return out,
    };
    if rec.kind != KIND_DIR {
        return out;
    }
    let n = rec.size as usize / DENT;
    for i in 0..n {
        let bi = (i * DENT / BLOCK) as u32;
        let b = match data_block(&mut rec, ino, bi, false) {
            Some(x) => x,
            None => break,
        };
        let mut blk = [0u8; 512];
        crate::fs::blk::read(b, &mut blk);
        let o = (i * DENT) % BLOCK;
        if blk[o] == 0 {
            continue;
        }
        let mut nb = 0;
        while nb < NAME_LEN && blk[o + nb] != 0 {
            nb += 1;
        }
        let name = String::from_utf8_lossy(&blk[o..o + nb]).into_owned();
        let child = r32(&blk, o + NAME_LEN);
        match get_ino(child) {
            Some(c) if c.kind == KIND_DIR => out.push(alloc::format!("{}/", name)),
            _ => out.push(name),
        }
    }
    out
}

pub fn mkdir(path: &str) -> bool {
    if walk(path).is_some() {
        return false;
    }
    let (parent, name) = match split_parent(path) {
        Some(x) => x,
        None => return false,
    };
    let mut new_ino = 0u32;
    for i in 1..=ninodes() {
        let (lba, off) = ino_pos(i);
        let mut b = [0u8; 512];
        crate::fs::blk::read(lba, &mut b);
        if b[off] == 0 {
            // v2.3: full reset like create_empty (stale size/blocks on
            // reuse alias data blocks across files).
            b[off] = KIND_DIR;
            w32(&mut b, off + 4, 0); // size
            for k in 0..8 {
                w32(&mut b, off + 8 + k * 4, 0); // direct
            }
            w32(&mut b, off + 40, 0); // indirect
            w32(&mut b, off + 44, 1); // nlink
            w32(&mut b, off + 48, 0); // dind (v2.5, same reason)
            crate::fs::blk::write(lba, &b);
            new_ino = i;
            break;
        }
    }
    if new_ino == 0 {
        return false;
    }
    dir_insert(parent, name, new_ino)
}

pub fn unlink(path: &str) -> bool {
    let (parent, name) = match split_parent(path) {
        Some(x) => x,
        None => return false,
    };
    let mut prec = match get_ino(parent) {
        Some(r) => r,
        None => return false,
    };
    if prec.kind != KIND_DIR {
        return false;
    }
    let n = prec.size as usize / DENT;
    for i in 0..n {
        let bi = (i * DENT / BLOCK) as u32;
        let b = match data_block(&mut prec, parent, bi, false) {
            Some(x) => x,
            None => return false,
        };
        let mut blk = [0u8; 512];
        crate::fs::blk::read(b, &mut blk);
        let o = (i * DENT) % BLOCK;
        if blk[o] == 0 {
            continue;
        }
        let mut nb = 0;
        while nb < NAME_LEN && blk[o + nb] != 0 {
            nb += 1;
        }
        if &blk[o..o + nb] == name.as_bytes() {
            let child = r32(&blk, o + NAME_LEN);
            // clear dirent
            for x in &mut blk[o..o + DENT] {
                *x = 0;
            }
            crate::fs::blk::write(b, &blk);
            // drop one link; free blocks+inode only at last link
            if let Some(mut crec) = get_ino(child) {
                if crec.nlink > 1 {
                    crec.nlink -= 1;
                    put_ino(child, &crec);
                } else if crec.kind == KIND_FILE {
                    free_ino_blocks(&crec);
                    let (lba, off) = ino_pos(child);
                    let mut ib = [0u8; 512];
                    crate::fs::blk::read(lba, &mut ib);
                    ib[off] = 0;
                    crate::fs::blk::write(lba, &ib);
                } else {
                    // dir with nlink<=1: free data blocks (v2.4: used to
                    // leak them -- matters now that `ctr rm` deletes whole
                    // image trees), then clear inode.
                    free_ino_blocks(&crec);
                    let (lba, off) = ino_pos(child);
                    let mut ib = [0u8; 512];
                    crate::fs::blk::read(lba, &mut ib);
                    ib[off] = 0;
                    crate::fs::blk::write(lba, &ib);
                }
            }
            return true;
        }
    }
    false
}

/// Hard link: new path points at old's inode. Old must be a file.
pub fn link(old: &str, new: &str) -> bool {
    let oino = match walk(old) {
        Some(i) => i,
        None => return false,
    };
    let mut orec = match get_ino(oino) {
        Some(r) => r,
        None => return false,
    };
    if orec.kind != KIND_FILE {
        return false;
    }
    if walk(new).is_some() {
        return false;
    }
    let (parent, name) = match split_parent(new) {
        Some(x) => x,
        None => return false,
    };
    if !dir_insert(parent, name, oino) {
        return false;
    }
    orec.nlink += 1;
    put_ino(oino, &orec);
    true
}

pub fn exists(path: &str) -> bool {
    if path == "/" {
        return true;
    }
    walk(path).is_some()
}

pub fn file_len(path: &str) -> Option<usize> {
    let ino = walk(path)?;
    let rec = get_ino(ino)?;
    if rec.kind != KIND_FILE {
        return None;
    }
    Some(rec.size as usize)
}

/// (kind, size, nlink): kind 1=file 2=dir 0=missing
pub fn stat(path: &str) -> (u8, u32, u32) {
    if path == "/" {
        return (2, 0, 1);
    }
    match walk(path).and_then(get_ino) {
        Some(r) if r.kind == KIND_FILE => (1, r.size, r.nlink),
        Some(r) if r.kind == KIND_DIR => (2, r.size, r.nlink),
        _ => (0, 0, 0),
    }
}

/// v2.0: directory check for chroot validation.
pub fn is_dir(path: &str) -> bool {
    stat(path).0 == KIND_DIR
}
