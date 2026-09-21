pub mod ramfs;
pub mod pipe;
pub mod virtio;
pub mod blk;
pub mod disk;

use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};

static USE_DISK: AtomicBool = AtomicBool::new(false);

pub fn use_disk() -> bool {
    USE_DISK.load(Ordering::Acquire)
}

pub fn init() {
    pipe::init();
    virtio::init();
    // v1.3: net device probe (SKIP without one; independent of disk)
    crate::net::init();
    if disk::mount() {
        USE_DISK.store(true, Ordering::Release);
        return;
    }
    // fallback: in-memory fs populated from embedded ELFs
    crate::println!("[FS] disk mount failed, ramfs fallback");
    ramfs::init();
    ramfs::write_file("/bin/init", crate::embed::INIT_ELF);
    ramfs::write_file("/bin/sh", crate::embed::SH_ELF);
    ramfs::write_file("/bin/ls", crate::embed::LS_ELF);
    ramfs::write_file("/bin/cat", crate::embed::CAT_ELF);
    ramfs::write_file("/bin/echo", crate::embed::ECHO_ELF);
    ramfs::write_file("/bin/grep", crate::embed::GREP_ELF);
    ramfs::write_file("/bin/fork_test", crate::embed::FORK_ELF);
    ramfs::write_file("/bin/pipe_test", crate::embed::PIPE_ELF);
    ramfs::write_file("/bin/usertests", crate::embed::USERTESTS_ELF);
    ramfs::write_file("/bin/persist", crate::embed::PERSIST_ELF);
    ramfs::write_file("/bin/printenv", crate::embed::PRINTENV_ELF);
    ramfs::write_file("/bin/smp_test", crate::embed::SMPTEST_ELF);
    ramfs::write_file("/README", b"muse-riscv-os Unix-v6 like\ntry: ls cat echo grep fork_test pipe_test usertests\n");
}

pub fn read_file(path: &str) -> Option<Vec<u8>> {
    if use_disk() {
        // embed fallback for /bin when disk lacks the file
        match disk::read_file(path) {
            Some(d) => Some(d),
            None => crate::embed::get_by_name(basename(path)),
        }
    } else {
        ramfs::read_file(path)
    }
}

fn basename(path: &str) -> &str {
    match path.rsplit_once('/') {
        Some((_, n)) => n,
        None => path,
    }
}

pub fn write_file(path: &str, data: &[u8]) {
    if use_disk() {
        if disk::read_file(path).is_none() {
            disk::create_empty(path);
        } else {
            disk::truncate_path(path);
        }
        disk::write_at(path, 0, data);
    } else {
        ramfs::write_file(path, data);
    }
}

pub fn read_at(path: &str, off: usize, buf: &mut [u8]) -> usize {
    if use_disk() {
        disk::read_at(path, off, buf)
    } else {
        ramfs::read_at(path, off, buf)
    }
}

pub fn write_at(path: &str, off: usize, buf: &[u8]) -> usize {
    if use_disk() {
        disk::write_at(path, off, buf)
    } else {
        ramfs::write_at(path, off, buf)
    }
}

pub fn truncate(path: &str) -> bool {
    if use_disk() {
        disk::truncate_path(path)
    } else {
        ramfs::truncate(path)
    }
}

pub fn list_dir(path: &str) -> Vec<alloc::string::String> {
    if use_disk() {
        disk::list_dir(path)
    } else {
        ramfs::list_dir(path)
    }
}

pub fn mkdir(path: &str) -> bool {
    if use_disk() {
        disk::mkdir(path)
    } else {
        ramfs::mkdir(path)
    }
}

pub fn unlink(path: &str) -> bool {
    if use_disk() {
        disk::unlink(path)
    } else {
        ramfs::unlink(path)
    }
}

pub fn exists(path: &str) -> bool {
    if use_disk() {
        disk::exists(path)
    } else {
        ramfs::exists(path)
    }
}

pub fn file_len(path: &str) -> Option<usize> {
    if use_disk() {
        disk::file_len(path)
    } else {
        ramfs::file_len(path)
    }
}

pub fn stat(path: &str) -> (u8, u32, u32) {
    if use_disk() {
        disk::stat(path)
    } else {
        match ramfs::read_file(path) {
            Some(d) => (1, d.len() as u32, 1),
            None => {
                if ramfs::exists(path) {
                    (2, 0, 1)
                } else {
                    (0, 0, 0)
                }
            }
        }
    }
}

/// v0.5: (total_blocks, free_blocks) for SYS_FSSTAT/`df`.
pub fn blocks_stat() -> (u32, u32) {
    if use_disk() {
        (disk::total_blocks(), disk::free_blocks())
    } else {
        (8192, 8192)
    }
}

pub fn link(old: &str, new: &str) -> bool {    if use_disk() {
        disk::link(old, new)
    } else {
        // ramfs: copy content (no shared inode)
        if ramfs::read_file(new).is_some() || ramfs::exists(new) {
            return false;
        }
        match ramfs::read_file(old) {
            Some(d) => {
                ramfs::write_file(new, &d);
                true
            }
            None => false,
        }
    }
}

// user-memory helpers (SUM=1 so direct deref works)
pub unsafe fn user_str(ptr: usize) -> Option<alloc::string::String> {
    if ptr == 0 {
        return None;
    }
    let mut v = Vec::new();
    let mut p = ptr as *const u8;
    for _ in 0..4096 {
        let b = *p;
        if b == 0 {
            break;
        }
        v.push(b);
        p = p.add(1);
    }
    alloc::string::String::from_utf8(v).ok()
}

pub unsafe fn user_slice_mut(ptr: usize, len: usize) -> &'static mut [u8] {
    core::slice::from_raw_parts_mut(ptr as *mut u8, len)
}
pub unsafe fn user_slice(ptr: usize, len: usize) -> &'static [u8] {
    core::slice::from_raw_parts(ptr as *const u8, len)
}
