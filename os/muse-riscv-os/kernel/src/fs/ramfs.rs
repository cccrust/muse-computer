use crate::sync::SpinMutex;
use alloc::string::String;
use alloc::vec::Vec;

struct File {
    path: String,
    data: Vec<u8>,
}

static mut FILES: Option<SpinMutex<Vec<File>>> = None;
static mut DIRS: Option<SpinMutex<Vec<String>>> = None;

pub fn init() {
    unsafe {
        FILES = Some(SpinMutex::new(Vec::new()));
        DIRS = Some(SpinMutex::new(Vec::from([
            String::from("/"),
            String::from("/bin"),
            String::from("/etc"),
        ])));
    }
}

fn files() -> &'static SpinMutex<Vec<File>> {
    unsafe { FILES.as_ref().unwrap() }
}
fn dirs() -> &'static SpinMutex<Vec<String>> {
    unsafe { DIRS.as_ref().unwrap() }
}

fn norm(path: &str, cwd: &str) -> String {
    if path.starts_with('/') {
        String::from(path)
    } else if cwd.ends_with('/') {
        alloc::format!("{}{}", cwd, path)
    } else {
        alloc::format!("{}/{}", cwd, path)
    }
}

pub fn cwd_of(pid: usize) -> String {
    String::from("/")
}

pub fn write_file(path: &str, data: &[u8]) {
    let mut fs = files().lock();
    for f in fs.iter_mut() {
        if f.path == path {
            f.data.clear();
            f.data.extend_from_slice(data);
            return;
        }
    }
    fs.push(File {
        path: String::from(path),
        data: Vec::from(data),
    });
}

pub fn read_file(path: &str) -> Option<Vec<u8>> {
    let fs = files().lock();
    for f in fs.iter() {
        if f.path == path {
            return Some(f.data.clone());
        }
    }
    None
}

pub fn file_len(path: &str) -> Option<usize> {
    let fs = files().lock();
    for f in fs.iter() {
        if f.path == path {
            return Some(f.data.len());
        }
    }
    None
}

pub fn read_at(path: &str, off: usize, buf: &mut [u8]) -> usize {
    let fs = files().lock();
    for f in fs.iter() {
        if f.path == path {
            let mut n = 0;
            while n < buf.len() && off + n < f.data.len() {
                buf[n] = f.data[off + n];
                n += 1;
            }
            return n;
        }
    }
    0
}

pub fn write_at(path: &str, off: usize, buf: &[u8]) -> usize {
    let mut fs = files().lock();
    for f in fs.iter_mut() {
        if f.path == path {
            if off > f.data.len() {
                f.data.resize(off, 0);
            }
            let mut n = 0;
            while n < buf.len() {
                if off + n < f.data.len() {
                    f.data[off + n] = buf[n];
                } else {
                    f.data.push(buf[n]);
                }
                n += 1;
            }
            return n;
        }
    }
    0
}

pub fn list_dir(path: &str) -> Vec<String> {
    let mut out = Vec::new();
    let fs = files().lock();
    let prefix = if path.ends_with('/') {
        String::from(path)
    } else {
        alloc::format!("{}/", path)
    };
    for f in fs.iter() {
        if f.path.starts_with(&prefix) {
            let rest = &f.path[prefix.len()..];
            if !rest.is_empty() && !rest.contains('/') {
                out.push(String::from(rest));
            }
        }
    }
    let ds = dirs().lock();
    for d in ds.iter() {
        if d.starts_with(&prefix) {
            let rest = &d[prefix.len()..];
            if !rest.is_empty() && !rest.contains('/') {
                out.push(alloc::format!("{}/", rest));
            }
        }
    }
    out
}

pub fn mkdir(path: &str) -> bool {
    let mut ds = dirs().lock();
    for d in ds.iter() {
        if d == path {
            return false;
        }
    }
    ds.push(String::from(path));
    true
}

pub fn unlink(path: &str) -> bool {
    let mut fs = files().lock();
    if let Some(i) = fs.iter().position(|f| f.path == path) {
        fs.remove(i);
        return true;
    }
    false
}

pub fn exists(path: &str) -> bool {
    if path == "/" {
        return true;
    }
    {
        let ds = dirs().lock();
        for d in ds.iter() {
            if d == path {
                return true;
            }
        }
    }
    let fs = files().lock();
    fs.iter().any(|f| f.path == path)
}
