// lythd/build.rs — expose pre-built service ELF paths for include_bytes!
//
// Build order (deadlock-safe — no recursive cargo invocations):
//
//   cargo build -p lythdist      --release
//   cargo build -p lysh          --release
//   cargo build -p lythd         --release
//
// This script resolves artifact paths and sets LYTHDIST_ELF / LYSH_ELF env
// vars so lythd/src/main.rs can embed the binaries with include_bytes!.
// Rerun-if-changed lines ensure lythd is rebuilt whenever a service changes.

use std::path::PathBuf;

fn main() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let workspace    = PathBuf::from(&manifest_dir).parent().unwrap().to_path_buf();
    let target_dir   = workspace.join("target/x86_64-oros/release");

    // ── lythdist ─────────────────────────────────────────────────────────────
    let lythdist_elf = target_dir.join("lythdist");
    if !lythdist_elf.exists() {
        eprintln!();
        eprintln!("error: lythdist has not been built yet.");
        eprintln!("       Run first:  cargo build -p lythdist --release");
        eprintln!();
        std::process::exit(1);
    }
    println!("cargo:rustc-env=LYTHDIST_ELF={}", lythdist_elf.display());
    println!("cargo:rerun-if-changed={}", workspace.join("lythdist/src/main.rs").display());
    println!("cargo:rerun-if-changed={}", workspace.join("lythdist/Cargo.toml").display());
    println!("cargo:rerun-if-changed={}", lythdist_elf.display());

    // ── lysh ─────────────────────────────────────────────────────────────────
    let lysh_elf = target_dir.join("lysh");
    if !lysh_elf.exists() {
        eprintln!();
        eprintln!("error: lysh has not been built yet.");
        eprintln!("       Run first:  cargo build -p lysh --release");
        eprintln!();
        std::process::exit(1);
    }
    println!("cargo:rustc-env=LYSH_ELF={}", lysh_elf.display());
    println!("cargo:rerun-if-changed={}", workspace.join("userspace/lysh/src/main.rs").display());
    println!("cargo:rerun-if-changed={}", workspace.join("userspace/lysh/Cargo.toml").display());
    println!("cargo:rerun-if-changed={}", lysh_elf.display());
}
