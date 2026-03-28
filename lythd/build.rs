// lythd/build.rs — expose the pre-built lythdist ELF path for include_bytes!
//
// lythdist must be built *before* lythd:
//   cargo build -p lythdist --release
//   cargo build -p lythd   --release
//
// Invoking `cargo build` recursively from a build script deadlocks (the inner
// cargo waits on the outer cargo's file lock).  Instead we just resolve the
// artifact path and let Cargo's rerun-if-changed invalidate lythd when
// lythdist's sources change.

use std::path::PathBuf;

fn main() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let workspace    = PathBuf::from(&manifest_dir).parent().unwrap().to_path_buf();

    let elf = workspace.join("target/x86_64-raptoros/release/lythdist");

    if !elf.exists() {
        eprintln!();
        eprintln!("error: lythdist has not been built yet.");
        eprintln!("       Run first:  cargo build -p lythdist --release");
        eprintln!();
        std::process::exit(1);
    }

    println!("cargo:rustc-env=LYTHDIST_ELF={}", elf.display());

    // Rebuild lythd whenever lythdist sources change.
    println!("cargo:rerun-if-changed={}", workspace.join("lythdist/src/main.rs").display());
    println!("cargo:rerun-if-changed={}", workspace.join("lythdist/Cargo.toml").display());
    println!("cargo:rerun-if-changed={}", elf.display());
}
