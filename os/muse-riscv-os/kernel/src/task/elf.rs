// Minimal ELF64 loader (no external crate).
pub struct Prog {
    pub vaddr: usize,
    pub filesz: usize,
    pub memsz: usize,
    pub flags: u32,
    pub offset: usize,
}

pub struct ElfInfo {
    pub entry: usize,
    pub progs: [Prog; 8],
    pub nprog: usize,
}

fn u16(b: &[u8], o: usize) -> u16 {
    (b[o] as u16) | ((b[o + 1] as u16) << 8)
}
fn u32(b: &[u8], o: usize) -> u32 {
    (b[o] as u32)
        | ((b[o + 1] as u32) << 8)
        | ((b[o + 2] as u32) << 16)
        | ((b[o + 3] as u32) << 24)
}
fn u64(b: &[u8], o: usize) -> u64 {
    u32(b, o) as u64 | ((u32(b, o + 4) as u64) << 32)
}

pub fn parse(elf: &[u8]) -> Option<ElfInfo> {
    if elf.len() < 64 {
        return None;
    }
    if elf[0] != 0x7f || elf[1] != b'E' || elf[2] != b'L' || elf[3] != b'F' {
        return None;
    }
    if elf[4] != 2 || elf[5] != 1 {
        return None; // 64bit LE only
    }
    let phoff = u64(elf, 0x20) as usize;
    let phentsz = u16(elf, 0x36) as usize;
    let phnum = u16(elf, 0x38) as usize;
    let entry = u64(elf, 0x18) as usize;
    if phnum > 8 {
        return None;
    }
    let mut info = ElfInfo {
        entry,
        progs: [
            Prog { vaddr: 0, filesz: 0, memsz: 0, flags: 0, offset: 0 },
            Prog { vaddr: 0, filesz: 0, memsz: 0, flags: 0, offset: 0 },
            Prog { vaddr: 0, filesz: 0, memsz: 0, flags: 0, offset: 0 },
            Prog { vaddr: 0, filesz: 0, memsz: 0, flags: 0, offset: 0 },
            Prog { vaddr: 0, filesz: 0, memsz: 0, flags: 0, offset: 0 },
            Prog { vaddr: 0, filesz: 0, memsz: 0, flags: 0, offset: 0 },
            Prog { vaddr: 0, filesz: 0, memsz: 0, flags: 0, offset: 0 },
            Prog { vaddr: 0, filesz: 0, memsz: 0, flags: 0, offset: 0 },
        ],
        nprog: 0,
    };
    for i in 0..phnum {
        let o = phoff + i * phentsz;
        if o + 56 > elf.len() {
            return None;
        }
        let ptype = u32(elf, o);
        if ptype != 1 {
            continue; // LOAD only
        }
        let flags = u32(elf, o + 4);
        let off = u64(elf, o + 8) as usize;
        let vaddr = u64(elf, o + 16) as usize;
        let filesz = u64(elf, o + 32) as usize;
        let memsz = u64(elf, o + 40) as usize;
        if info.nprog >= 8 {
            return None;
        }
        info.progs[info.nprog] = Prog {
            vaddr,
            filesz,
            memsz,
            flags,
            offset: off,
        };
        info.nprog += 1;
    }
    Some(info)
}

/// flags: PF_R=4 PF_W=2 PF_X=1 -> PTE bits
pub fn pte_flags_for(pflags: u32) -> u64 {
    use crate::mem::pagetable as pt;
    let mut f = 0u64;
    if pflags & 4 != 0 {
        f |= pt::PTE_R;
    }
    if pflags & 2 != 0 {
        f |= pt::PTE_W;
    }
    if pflags & 1 != 0 {
        f |= pt::PTE_X;
    }
    f
}
