use crate::sync::SpinMutex;
use alloc::vec::Vec;

const CAP: usize = 1024;

struct Pipe {
    buf: [u8; CAP],
    r: usize,
    w: usize,
    n: usize,
    rclosed: bool,
    wclosed: bool,
}

impl Pipe {
    const fn empty() -> Self {
        Self {
            buf: [0; CAP],
            r: 0,
            w: 0,
            n: 0,
            rclosed: false,
            wclosed: false,
        }
    }
}

static mut PIPES: Option<SpinMutex<Vec<Option<Pipe>>>> = None;

pub fn init() {
    unsafe {
        PIPES = Some(SpinMutex::new(Vec::new()));
    }
}

fn pipes() -> &'static SpinMutex<Vec<Option<Pipe>>> {
    unsafe { PIPES.as_ref().unwrap() }
}

pub fn create() -> (usize, usize) {
    let mut ps = pipes().lock();
    let id = ps.len();
    ps.push(Some(Pipe::empty()));
    // return read-end id and write-end id encoded: use id*2 / id*2+1?
    // Simpler: both ends share same pipe id; fd layer distinguishes R/W.
    // We allocate two slots pointing to same? Instead single id, caller dup.
    // Allocate second placeholder mapping to same id via alias table? Simplify:
    // push two entries sharing? Use id as pipe index, ends implicit.
    (id, id)
}

pub fn write(id: usize, buf: &[u8]) -> usize {
    let mut ps = pipes().lock();
    if id >= ps.len() {
        return 0;
    }
    let p = ps[id].as_mut().unwrap();
    let mut n = 0;
    for &b in buf {
        if p.n >= CAP {
            break;
        }
        p.buf[p.w] = b;
        p.w = (p.w + 1) % CAP;
        p.n += 1;
        n += 1;
    }
    n
}

pub fn read(id: usize, buf: &mut [u8]) -> usize {
    let mut ps = pipes().lock();
    if id >= ps.len() {
        return 0;
    }
    let p = ps[id].as_mut().unwrap();
    let mut n = 0;
    while n < buf.len() && p.n > 0 {
        buf[n] = p.buf[p.r];
        p.r = (p.r + 1) % CAP;
        p.n -= 1;
        n += 1;
    }
    n
}

pub fn avail(id: usize) -> usize {
    let ps = pipes().lock();
    if id >= ps.len() {
        return 0;
    }
    ps[id].as_ref().unwrap().n
}
