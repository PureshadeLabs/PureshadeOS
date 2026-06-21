// Build script: compile mkrfs and create disk.img for QEMU.
//
// The disk image is written to disk.img in the workspace root.
// Pass it to QEMU with:
//   -drive file=disk.img,format=raw,if=none,id=hd0
//   -device virtio-blk-pci,drive=hd0

fn main() {
    // CARGO_MANIFEST_DIR = kernel/; workspace root is one level up.
    let workspace = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_owned();

    let mkrfs_dir = workspace.join("tools/mkrfs");
    let mkrfs_bin = workspace.join("tools/mkrfs/mkrfs");
    let rootfs_dir = workspace.join("rootfs");
    let disk_img   = workspace.join("disk.img");

    // Build mkrfs using its own Makefile (rustc directly — avoids build-std config clash).
    let status = std::process::Command::new("make")
        .args(["-C", mkrfs_dir.to_str().unwrap()])
        .status()
        .expect("failed to invoke make for mkrfs");
    assert!(status.success(), "mkrfs build failed");

    // Create disk.img from rootfs/ if it exists, or an empty 64 MiB image.
    let mut cmd = std::process::Command::new(&mkrfs_bin);
    cmd.args([disk_img.to_str().unwrap(), "64M"]);
    if rootfs_dir.is_dir() {
        cmd.arg(rootfs_dir.to_str().unwrap());
    }
    let status = cmd.status().expect("failed to run mkrfs");
    assert!(status.success(), "disk image creation failed");

    println!("cargo:rerun-if-changed={}", mkrfs_dir.join("src/main.rs").display());
    watch_dir(rootfs_dir.to_str().unwrap());
}

fn watch_dir(dir: &str) {
    println!("cargo:rerun-if-changed={}", dir);
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            println!("cargo:rerun-if-changed={}", path.display());
            if path.is_dir() {
                watch_dir(&path.to_string_lossy());
            }
        }
    }
}
