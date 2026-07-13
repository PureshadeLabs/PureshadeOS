// Build script: compile mkrfs2 and create the RFS V2 disk.img for QEMU.
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

    let mkrfs2_dir = workspace.join("tools/mkrfs2");
    let mkrfs2_bin = workspace.join("tools/mkrfs2/mkrfs2");
    let rootfs_dir = workspace.join("rootfs");
    let disk_img   = workspace.join("disk.img");

    // Build mkrfs2 using its own Makefile (rustc directly — avoids the
    // build-std config clash and the cargo-in-cargo target-dir lock).
    let status = std::process::Command::new("make")
        .args(["-C", mkrfs2_dir.to_str().unwrap()])
        .status()
        .expect("failed to invoke make for mkrfs2");
    assert!(status.success(), "mkrfs2 build failed");

    // Create disk.img from rootfs/ if it exists, or an empty 64 MiB image.
    let mut cmd = std::process::Command::new(&mkrfs2_bin);
    cmd.args([disk_img.to_str().unwrap(), "64M"]);
    if rootfs_dir.is_dir() {
        cmd.arg(rootfs_dir.to_str().unwrap());
    }
    let status = cmd.status().expect("failed to run mkrfs2");
    assert!(status.success(), "disk image creation failed");

    println!("cargo:rerun-if-changed={}", mkrfs2_dir.join("src/main.rs").display());
    // The image is produced by fs/rfs2; regenerate when the FS crate changes.
    watch_dir(workspace.join("fs/rfs2/src").to_str().unwrap());
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
