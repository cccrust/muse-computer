// Embedded user ELF binaries (built first by run.sh / test.sh).
#[cfg(not(test))]
use alloc::vec::Vec;
#[cfg(not(test))]
mod real {
    pub const INIT_ELF: &[u8] =
        include_bytes!("../../target/riscv64gc-unknown-none-elf/release/init");
    pub const SH_ELF: &[u8] =
        include_bytes!("../../target/riscv64gc-unknown-none-elf/release/sh");
    pub const LS_ELF: &[u8] =
        include_bytes!("../../target/riscv64gc-unknown-none-elf/release/ls");
    pub const CAT_ELF: &[u8] =
        include_bytes!("../../target/riscv64gc-unknown-none-elf/release/cat");
    pub const ECHO_ELF: &[u8] =
        include_bytes!("../../target/riscv64gc-unknown-none-elf/release/echo");
    pub const GREP_ELF: &[u8] =
        include_bytes!("../../target/riscv64gc-unknown-none-elf/release/grep");
    pub const FORK_ELF: &[u8] =
        include_bytes!("../../target/riscv64gc-unknown-none-elf/release/fork_test");
    pub const PIPE_ELF: &[u8] =
        include_bytes!("../../target/riscv64gc-unknown-none-elf/release/pipe_test");
    pub const USERTESTS_ELF: &[u8] =
        include_bytes!("../../target/riscv64gc-unknown-none-elf/release/usertests");
    pub const PERSIST_ELF: &[u8] =
        include_bytes!("../../target/riscv64gc-unknown-none-elf/release/persist");
    pub const PRINTENV_ELF: &[u8] =
        include_bytes!("../../target/riscv64gc-unknown-none-elf/release/printenv");
    pub const SMPTEST_ELF: &[u8] =
        include_bytes!("../../target/riscv64gc-unknown-none-elf/release/smp_test");
    pub const RECLAIM_ELF: &[u8] =
        include_bytes!("../../target/riscv64gc-unknown-none-elf/release/reclaim_test");
    pub const STRESS_ELF: &[u8] =
        include_bytes!("../../target/riscv64gc-unknown-none-elf/release/stress");
    pub const UDP_ELF: &[u8] =
        include_bytes!("../../target/riscv64gc-unknown-none-elf/release/udpping");
    pub const WEB_ELF: &[u8] =
        include_bytes!("../../target/riscv64gc-unknown-none-elf/release/webserver");
    pub const CRASH_ELF: &[u8] =
        include_bytes!("../../target/riscv64gc-unknown-none-elf/release/crashwrite");
    pub const NSLOOKUP_ELF: &[u8] =
        include_bytes!("../../target/riscv64gc-unknown-none-elf/release/nslookup");
    pub const WGET_ELF: &[u8] =
        include_bytes!("../../target/riscv64gc-unknown-none-elf/release/wget");
    pub const CURL_ELF: &[u8] =
        include_bytes!("../../target/riscv64gc-unknown-none-elf/release/curl");
    pub const CTR_ELF: &[u8] =
        include_bytes!("../../target/riscv64gc-unknown-none-elf/release/ctr");
    pub const CHROOTT_ELF: &[u8] =
        include_bytes!("../../target/riscv64gc-unknown-none-elf/release/chroot_test");
    pub const NSTEST_ELF: &[u8] =
        include_bytes!("../../target/riscv64gc-unknown-none-elf/release/nstest");
    pub const CGTEST_ELF: &[u8] =
        include_bytes!("../../target/riscv64gc-unknown-none-elf/release/cgtest");
    pub const PING_ELF: &[u8] =
        include_bytes!("../../target/riscv64gc-unknown-none-elf/release/ping");
}
#[cfg(not(test))]
pub use real::*;

#[cfg(not(test))]
pub fn get_by_name(name: &str) -> Option<Vec<u8>> {
    let b = match name {
        "init" => INIT_ELF,
        "sh" => SH_ELF,
        "ls" => LS_ELF,
        "cat" => CAT_ELF,
        "echo" => ECHO_ELF,
        "grep" => GREP_ELF,
        "fork_test" => FORK_ELF,
        "pipe_test" => PIPE_ELF,
        "usertests" => USERTESTS_ELF,
        "persist" => PERSIST_ELF,
        "printenv" => PRINTENV_ELF,
        "smp_test" => SMPTEST_ELF,
        "reclaim_test" => RECLAIM_ELF,
        "stress" => STRESS_ELF,
        "udpping" => UDP_ELF,
        "webserver" => WEB_ELF,
        "crashwrite" => CRASH_ELF,
        "nslookup" => NSLOOKUP_ELF,
        "wget" => WGET_ELF,
        "curl" => CURL_ELF,
        "ctr" => CTR_ELF,
        "chroot_test" => CHROOTT_ELF,
        "nstest" => NSTEST_ELF,
        "cgtest" => CGTEST_ELF,
        "ping" => PING_ELF,
        _ => return None,
    };
    Some(Vec::from(b))
}

#[cfg(test)]
pub fn get_by_name(_name: &str) -> Option<Vec<u8>> {
    None
}

#[cfg(test)]
pub const INIT_ELF: &[u8] = b"test";
#[cfg(test)]
pub const SH_ELF: &[u8] = b"test";
#[cfg(test)]
pub const LS_ELF: &[u8] = b"test";
#[cfg(test)]
pub const CAT_ELF: &[u8] = b"test";
#[cfg(test)]
pub const ECHO_ELF: &[u8] = b"test";
#[cfg(test)]
pub const GREP_ELF: &[u8] = b"test";
#[cfg(test)]
pub const FORK_ELF: &[u8] = b"test";
#[cfg(test)]
pub const PIPE_ELF: &[u8] = b"test";
#[cfg(test)]
pub const USERTESTS_ELF: &[u8] = b"test";
#[cfg(test)]
pub const PERSIST_ELF: &[u8] = b"test";
#[cfg(test)]
pub const PRINTENV_ELF: &[u8] = b"test";
#[cfg(test)]
pub const SMPTEST_ELF: &[u8] = b"test";
#[cfg(test)]
pub const RECLAIM_ELF: &[u8] = b"test";
#[cfg(test)]
pub const STRESS_ELF: &[u8] = b"test";
#[cfg(test)]
pub const UDP_ELF: &[u8] = b"test";
#[cfg(test)]
pub const WEB_ELF: &[u8] = b"test";
#[cfg(test)]
pub const CRASH_ELF: &[u8] = b"test";
#[cfg(test)]
pub const NSLOOKUP_ELF: &[u8] = b"test";
#[cfg(test)]
pub const WGET_ELF: &[u8] = b"test";
#[cfg(test)]
pub const CURL_ELF: &[u8] = b"test";
#[cfg(test)]
pub const CTR_ELF: &[u8] = b"test";
#[cfg(test)]
pub const CHROOTT_ELF: &[u8] = b"test";
#[cfg(test)]
pub const NSTEST_ELF: &[u8] = b"test";
#[cfg(test)]
pub const CGTEST_ELF: &[u8] = b"test";
#[cfg(test)]
pub const PING_ELF: &[u8] = b"test";
