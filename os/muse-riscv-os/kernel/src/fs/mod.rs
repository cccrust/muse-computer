pub mod ramfs;
pub mod pipe;
pub mod virtio;

use alloc::vec::Vec;

pub fn init() {
    ramfs::init();
    pipe::init();
    // populate /bin with embedded ELFs
    ramfs::write_file("/bin/init", crate::embed::INIT_ELF);
    ramfs::write_file("/bin/sh", crate::embed::SH_ELF);
    ramfs::write_file("/bin/ls", crate::embed::LS_ELF);
    ramfs::write_file("/bin/cat", crate::embed::CAT_ELF);
    ramfs::write_file("/bin/echo", crate::embed::ECHO_ELF);
    ramfs::write_file("/bin/grep", crate::embed::GREP_ELF);
    ramfs::write_file("/bin/fork_test", crate::embed::FORK_ELF);
    ramfs::write_file("/bin/pipe_test", crate::embed::PIPE_ELF);
    ramfs::write_file("/bin/usertests", crate::embed::USERTESTS_ELF);
    ramfs::write_file("/README", b"muse-riscv-os Unix-v6 like\ntry: ls cat echo grep fork_test pipe_test usertests\n");
}

pub fn read_file(path: &str) -> Option<Vec<u8>> {
    ramfs::read_file(path)
}

pub fn virtio_probe() {
    virtio::probe();
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
