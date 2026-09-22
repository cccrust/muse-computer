fn main() {
    let target = std::env::var("TARGET").unwrap_or_default();
    if target.contains("riscv64") {
        let m = std::env::var("CARGO_MANIFEST_DIR").unwrap();
        println!("cargo:rustc-link-arg=-T{}/../../user-linker.ld", m);
    }
    println!("cargo:rerun-if-changed=build.rs");
}
