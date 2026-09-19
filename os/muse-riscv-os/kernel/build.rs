fn main() {
    let target = std::env::var("TARGET").unwrap_or_default();
    if target.contains("riscv64") {
        let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
        println!("cargo:rustc-link-arg=-T{}/linker.ld", dir);
        println!("cargo:rerun-if-changed=linker.ld");
    }
}
