use std::fs;
use std::path::Path;

fn main() {
    // Packs target user ELFs into fs.img (raw concatenation with simple header).
    // Kernel currently embeds ELFs directly, so fs.img is used as virtio-blk
    // presence image + future EasyFS. We still produce it for QEMU -drive.
    let out = std::env::args().nth(1).unwrap_or("fs.img".into());
    let names = [
        "init", "sh", "ls", "cat", "echo", "grep", "fork_test", "pipe_test", "usertests",
    ];
    let mut blob = Vec::new();
    blob.extend_from_slice(b"MUSEFS01");
    for n in names {
        let p = Path::new("target/riscv64gc-unknown-none-elf/release").join(n);
        let data = fs::read(&p).unwrap_or_else(|_| {
            eprintln!("warn: missing {:?}, pad empty", p);
            Vec::new()
        });
        let len = data.len() as u32;
        blob.extend_from_slice(n.as_bytes());
        blob.extend_from_slice(&[0u8; 32 - 0][..(32 - n.len().min(32))]);
        blob.extend_from_slice(&len.to_le_bytes());
        blob.extend_from_slice(&data);
        println!("mkfs: {} ({} bytes)", n, len);
    }
    while blob.len() < 4 * 1024 * 1024 {
        blob.extend_from_slice(&[0u8; 4096][..(4 * 1024 * 1024 - blob.len()).min(4096)]);
    }
    fs::write(&out, &blob).unwrap();
    println!("mkfs: wrote {} ({} bytes)", out, blob.len());
}
