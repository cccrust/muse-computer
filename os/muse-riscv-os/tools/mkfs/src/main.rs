use std::fs;
use std::path::Path;

// ---- MUSEFS layout (MUST match kernel/src/fs/disk.rs) ----
pub const BLOCK: usize = 512;
pub const NBLOCKS: u32 = 8192; // 4MB image
// v1.6: journal area (disk tail): 512 sectors = 256 records of 2 sectors.
pub const JLBA: u32 = NBLOCKS - 512;
pub const JB: u32 = 512;
pub const MAGIC: &[u8; 8] = b"MUSEFS01";
pub const BMAP_LBA: u32 = 1;
pub const BMAP_BLOCKS: u32 = 2; // 8192 bits = 1024B
pub const INO_LBA: u32 = 3;
pub const INO_SIZE: usize = 64;
pub const INODES_PER_BLOCK: u32 = 8;
pub const ROOT_INO: u32 = 1;
pub const KIND_FILE: u8 = 1;
pub const KIND_DIR: u8 = 2;
pub const NAME_LEN: usize = 28;
pub const DIRENT_SIZE: usize = 32;

fn w32(b: &mut [u8], off: usize, v: u32) {
    b[off..off + 4].copy_from_slice(&v.to_le_bytes());
}
fn r32(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

pub struct Image {
    pub data: Vec<u8>,
    next_free: u32,
}

impl Image {
    pub fn new(nfiles: usize) -> Self {
        // inodes: root + bin + files + spare for runtime creates.
        // v2.4: spare 16 -> 64. Steady-state runtime files (~14: /tmp,
        // /ctr, testimg x4, ctrtest x2, dl.txt, WD x3, CRASHDAT, TESTDATA)
        // plus per-boot transients (ctr suite: life, bin, .pid) left ZERO
        // headroom -- the 17th create (.pid) failed on used images
        // ("detached state failed"; inode exhaustion, not corruption).
        // All consumers read ninodes from the superblock; no hardcoding.
        let ninodes = (2 + nfiles + 64) as u32;
        let ino_blocks = (ninodes + INODES_PER_BLOCK - 1) / INODES_PER_BLOCK;
        let data_lba = INO_LBA + ino_blocks;
        let mut img = Self {
            data: vec![0u8; NBLOCKS as usize * BLOCK],
            next_free: data_lba,
        };
        // superblock LBA0
        let sb = img.block_mut(0);
        sb[..8].copy_from_slice(MAGIC);
        w32(sb, 8, BLOCK as u32);
        w32(sb, 12, NBLOCKS);
        w32(sb, 16, BMAP_LBA);
        w32(sb, 20, BMAP_BLOCKS);
        w32(sb, 24, INO_LBA);
        w32(sb, 28, ino_blocks);
        w32(sb, 32, ninodes);
        w32(sb, 36, data_lba);
        w32(sb, 40, ROOT_INO);
        w32(sb, 44, 2); // version 2: nlink field present
        w32(sb, 48, 1); // dirty: needs bitmap check on first mount
        // v1.6: journal area (disk tail; zeroed content = no valid records)
        w32(sb, 52, JLBA);
        w32(sb, 56, JB);
        // mark reserved blocks used: 0..data_lba
        for b in 0..data_lba {
            img.set_used(b);
        }
        // v1.6: journal blocks are never data (mark used + keep zeroed)
        for b in JLBA..JLBA + JB {
            img.set_used(b);
        }
        // root + bin dirs
        img.inode_set(ROOT_INO, KIND_DIR, 0);
        let bin = img.inode_alloc(KIND_DIR);
        assert_eq!(bin, 2);
        img.dir_add(ROOT_INO, "bin", bin);
        img
    }

    fn block_mut(&mut self, lba: u32) -> &mut [u8] {
        let o = lba as usize * BLOCK;
        &mut self.data[o..o + BLOCK]
    }
    fn block(&self, lba: u32) -> &[u8] {
        let o = lba as usize * BLOCK;
        &self.data[o..o + BLOCK]
    }
    fn set_used(&mut self, lba: u32) {
        let bit = lba as usize;
        let o = BMAP_LBA as usize * BLOCK + bit / 8;
        self.data[o] |= 1 << (bit % 8);
    }
    pub fn is_used(&self, lba: u32) -> bool {
        let bit = lba as usize;
        let o = BMAP_LBA as usize * BLOCK + bit / 8;
        self.data[o] & (1 << (bit % 8)) != 0
    }
    fn balloc(&mut self) -> u32 {
        for b in 0..NBLOCKS {
            // v1.6: journal area is never data
            if b >= JLBA {
                continue;
            }
            if !self.is_used(b) {
                self.set_used(b);
                let o = b as usize * BLOCK;
                for x in &mut self.data[o..o + BLOCK] {
                    *x = 0;
                }
                return b;
            }
        }
        panic!("mkfs: out of blocks");
    }

    fn ino_off(ino: u32) -> usize {
        INO_LBA as usize * BLOCK + (ino - 1) as usize * INO_SIZE
    }
    fn inode_set(&mut self, ino: u32, kind: u8, size: u32) {
        let o = Self::ino_off(ino);
        self.data[o] = kind;
        self.data[o + 1..o + 4].copy_from_slice(&[0; 3]);
        w32(&mut self.data, o + 4, size);
        w32(&mut self.data, o + 44, 1); // nlink = 1
        // direct/indirect already zero
    }
    fn inode_alloc(&mut self, kind: u8) -> u32 {
        // find free inode slot by scanning table region
        let o0 = INO_LBA as usize * BLOCK;
        let ninodes = r32(&self.data, 32);
        for i in 1..=ninodes {
            let o = o0 + (i - 1) as usize * INO_SIZE;
            if self.data[o] == 0 {
                self.inode_set(i, kind, 0);
                return i;
            }
        }
        panic!("mkfs: out of inodes");
    }
    fn inode_grow(&mut self, ino: u32, data: &[u8]) {
        // allocate blocks: direct[0..8], then single-indirect (128),
        // then double-indirect (128x128). v2.5: the old code wrote past
        // the single indirect block for files > 69632B, clobbering
        // whatever followed it in the image (sh/ctr/wget did).
        let mut off = 0usize;
        let mut di = 0usize;
        let mut indirect_lba = 0u32;
        let mut indirect_used = 0usize;
        let mut dind_lba = 0u32;
        let mut dind_n = 0usize; // data blocks placed via dind so far
        while off < data.len() {
            let b = self.balloc();
            let n = core::cmp::min(BLOCK, data.len() - off);
            let o = b as usize * BLOCK;
            self.data[o..o + n].copy_from_slice(&data[off..off + n]);
            let io = Self::ino_off(ino);
            if di < 8 {
                w32(&mut self.data, io + 8 + di * 4, b);
                di += 1;
            } else if indirect_used < 128 {
                if indirect_lba == 0 {
                    indirect_lba = self.balloc();
                    w32(&mut self.data, io + 40, indirect_lba);
                }
                let o = indirect_lba as usize * BLOCK + indirect_used * 4;
                w32(&mut self.data, o, b);
                indirect_used += 1;
            } else {
                // double-indirect: l1 = dind_n / 128, l2 = dind_n % 128.
                if dind_lba == 0 {
                    dind_lba = self.balloc();
                    w32(&mut self.data, io + 48, dind_lba);
                }
                let l1 = dind_n / 128;
                let l2 = dind_n % 128;
                assert!(l1 < 128, "mkfs: file too big for dind");
                let o = dind_lba as usize * BLOCK + l1 * 4;
                let mut l1b = r32(&self.data, o);
                if l1b == 0 {
                    l1b = self.balloc();
                    w32(&mut self.data, o, l1b);
                }
                w32(&mut self.data, l1b as usize * BLOCK + l2 * 4, b);
                dind_n += 1;
            }
            off += n;
        }
        let io = Self::ino_off(ino);
        w32(&mut self.data, io + 4, data.len() as u32);
    }
    fn dir_add(&mut self, dir_ino: u32, name: &str, child: u32) {
        assert!(name.len() <= NAME_LEN);
        // scan existing slots first
        let io = Self::ino_off(dir_ino);
        let n = r32(&self.data, io + 4) as usize / DIRENT_SIZE;
        for i in 0..n {
            let b = r32(&self.data, io + 8 + (i * DIRENT_SIZE / BLOCK) as usize * 4);
            if b == 0 {
                continue;
            }
            let o = b as usize * BLOCK + (i * DIRENT_SIZE % BLOCK);
            if self.data[o] == 0 {
                self.data[o..o + name.len()].copy_from_slice(name.as_bytes());
                w32(&mut self.data, o + NAME_LEN, child);
                return;
            }
        }
        // append new block (all slots full)
        let bi = (n * DIRENT_SIZE / BLOCK) as u32;
        assert!(bi < 8);
        let b = self.balloc();
        let io = Self::ino_off(dir_ino);
        w32(&mut self.data, io + 8 + bi as usize * 4, b);
        w32(&mut self.data, io + 4, (n * DIRENT_SIZE + BLOCK) as u32);
        let o = b as usize * BLOCK + (n * DIRENT_SIZE % BLOCK);
        self.data[o..o + name.len()].copy_from_slice(name.as_bytes());
        w32(&mut self.data, o + NAME_LEN, child);
    }

    pub fn add_file(&mut self, parent: u32, name: &str, data: &[u8]) {
        let ino = self.inode_alloc(KIND_FILE);
        self.inode_grow(ino, data);
        self.dir_add(parent, name, ino);
    }

    // ---- readback (also used by tests) ----
    pub fn check_magic(&self) -> bool {
        &self.data[..8] == MAGIC
    }
    pub fn read_file(&self, parent: u32, name: &str) -> Option<Vec<u8>> {
        let ino = self.lookup(parent, name)?;
        let io = Self::ino_off(ino);
        if self.data[io] != KIND_FILE {
            return None;
        }
        let size = r32(&self.data, io + 4) as usize;
        let mut out = Vec::with_capacity(size);
        let mut got = 0usize;
        for di in 0..8 {
            if got >= size {
                break;
            }
            let b = r32(&self.data, io + 8 + di * 4);
            if b == 0 {
                break;
            }
            let n = core::cmp::min(BLOCK, size - got);
            out.extend_from_slice(&self.data[b as usize * BLOCK..][..n]);
            got += n;
        }
        let ind = r32(&self.data, io + 40);
        let mut ii = 0usize;
        while got < size && ind != 0 && ii < 128 {
            let b = r32(&self.data, ind as usize * BLOCK + ii * 4);
            if b == 0 {
                break;
            }
            let n = core::cmp::min(BLOCK, size - got);
            out.extend_from_slice(&self.data[b as usize * BLOCK..][..n]);
            got += n;
            ii += 1;
        }
        // v2.5: double-indirect (mirrors kernel data_block order).
        let dind = r32(&self.data, io + 48);
        let mut di = 0usize;
        while got < size && dind != 0 {
            let l1b = r32(&self.data, dind as usize * BLOCK + (di / 128) * 4);
            if l1b == 0 {
                break;
            }
            let b = r32(&self.data, l1b as usize * BLOCK + (di % 128) * 4);
            if b == 0 {
                break;
            }
            let n = core::cmp::min(BLOCK, size - got);
            out.extend_from_slice(&self.data[b as usize * BLOCK..][..n]);
            got += n;
            di += 1;
        }
        Some(out)
    }
    pub fn lookup(&self, parent: u32, name: &str) -> Option<u32> {
        let io = Self::ino_off(parent);
        if self.data[io] != KIND_DIR {
            return None;
        }
        let size = r32(&self.data, io + 4) as usize;
        for i in 0..size / DIRENT_SIZE {
            let b = r32(&self.data, io + 8 + (i * DIRENT_SIZE / BLOCK) as usize * 4);
            if b == 0 {
                continue;
            }
            let o = b as usize * BLOCK + (i * DIRENT_SIZE % BLOCK);
            let mut nb = 0;
            while nb < NAME_LEN && self.data[o + nb] != 0 {
                nb += 1;
            }
            if &self.data[o..o + nb] == name.as_bytes() {
                return Some(r32(&self.data, o + NAME_LEN));
            }
        }
        None
    }
    pub fn list(&self, parent: u32) -> Vec<(String, u32)> {
        let mut v = Vec::new();
        let io = Self::ino_off(parent);
        let size = r32(&self.data, io + 4) as usize;
        for i in 0..size / DIRENT_SIZE {
            let b = r32(&self.data, io + 8 + (i * DIRENT_SIZE / BLOCK) as usize * 4);
            if b == 0 {
                continue;
            }
            let o = b as usize * BLOCK + (i * DIRENT_SIZE % BLOCK);
            if self.data[o] == 0 {
                continue;
            }
            let mut nb = 0;
            while nb < NAME_LEN && self.data[o + nb] != 0 {
                nb += 1;
            }
            v.push((
                String::from_utf8_lossy(&self.data[o..o + nb]).into_owned(),
                r32(&self.data, o + NAME_LEN),
            ));
        }
        v
    }
}

fn main() {
    let out = std::env::args().nth(1).unwrap_or("fs.img".into());
    let names = [
        "init", "sh", "ls", "cat", "echo", "grep", "fork_test", "pipe_test", "usertests",
        "persist", "printenv", "smp_test", "reclaim_test", "stress", "udpping", "webserver",
        "crashwrite", "nslookup", "wget", "curl", "ctr", "sleeper", "chroot_test", "nstest", "cgtest", "ping",
    ];
    let mut img = Image::new(names.len() + 1); // + README
    for n in names {
        let p = Path::new("target/riscv64gc-unknown-none-elf/release").join(n);
        let data = fs::read(&p).unwrap_or_else(|_| {
            eprintln!("warn: missing {:?}, pad empty", p);
            Vec::new()
        });
        img.add_file(2, n, &data);
        println!("mkfs: /bin/{} ({} bytes)", n, data.len());
    }
    let readme = b"muse-riscv-os Unix-v6 like\ntry: ls cat echo grep fork_test pipe_test usertests\n";
    img.add_file(1, "README", readme);
    println!("mkfs: /README ({} bytes)", readme.len());
    // self-verify
    assert!(img.check_magic());
    for n in names {
        assert!(img.lookup(2, n).is_some(), "missing /bin/{}", n);
    }
    fs::write(&out, &img.data).unwrap();
    println!("mkfs: wrote {} ({} bytes)", out, img.data.len());
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn roundtrip() {
        let mut img = Image::new(3);
        img.add_file(2, "hello", b"hello world");
        img.add_file(1, "README", b"readme!");
        let big = vec![0xabu8; 3000]; // multi-block incl. still direct
        img.add_file(2, "big", &big);
        assert!(img.check_magic());
        // version 2 + nlink present
        assert_eq!(r32(&img.data, 44), 2);
        assert_eq!(r32(&img.data, 48), 1); // dirty: first mount rebuilds
        let hello_ino = img.lookup(2, "hello").unwrap();
        let ho = Image::ino_off(hello_ino);
        assert_eq!(r32(&img.data, ho + 44), 1);
        assert_eq!(img.read_file(2, "hello").unwrap(), b"hello world");
        assert_eq!(img.read_file(1, "README").unwrap(), b"readme!");
        assert_eq!(img.read_file(2, "big").unwrap(), big);
        assert!(img.read_file(2, "nope").is_none());
        let mut names: Vec<String> = img.list(2).into_iter().map(|(n, _)| n).collect();
        names.sort();
        assert_eq!(names, vec!["big", "hello"]);
        // bitmap consistency: blocks referenced by files are marked
        assert!(img.is_used(0)); // superblock
    }
    #[test]
    fn indirect_file() {
        // >8 blocks forces indirect
        let mut img = Image::new(1);
        let big = vec![0x5au8; 8 * 512 + 100];
        img.add_file(2, "huge", &big);
        assert_eq!(img.read_file(2, "huge").unwrap(), big);
    }
}
