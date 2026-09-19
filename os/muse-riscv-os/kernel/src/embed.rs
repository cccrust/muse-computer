// Embedded user ELF binaries (built first by run.sh / test.sh).
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
}
#[cfg(not(test))]
pub use real::*;

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
