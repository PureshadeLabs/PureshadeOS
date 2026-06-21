//! **sysroot-builder** — assembles a Rust sysroot for `x86_64-lythos`.
//!
//! ## What it does
//!
//! 1. Locates the active Rust toolchain (`rustup show active-toolchain`).
//! 2. Clones / checks out the matching `rust-src` component.
//! 3. Applies the Lythos PAL patches to `library/std/src/sys/`.
//! 4. Builds `core`, `alloc`, `compiler_builtins`, `lythos-libc`,
//!    `lythos-unwind`, and `lythos-libstd` for the `x86_64-lythos` target.
//! 5. Installs the resulting rlibs into a `sysroot/` directory that
//!    `cargo` can use via `RUST_SYSROOT` or `.cargo/config.toml`.
//!
//! ## Usage
//!
//! ```sh
//! cargo run --manifest-path tools/lythos-toolchain/sysroot-builder/Cargo.toml -- \
//!     --toolchain-root "$(rustup show home)/toolchains/nightly-x86_64-unknown-linux-gnu" \
//!     --out-sysroot    ./lythos-sysroot
//! ```
//!
//! ## Stages
//!
//! | Stage | Input | Output |
//! |-------|-------|--------|
//! | 0 | host rustc + rust-src | `libcore.rlib`, `libcompiler_builtins.rlib` |
//! | 1 | stage-0 + lythos-libc, lythos-unwind | `libc.a`, `libunwind.a` |
//! | 2 | stage-1 + lythos-libstd | `libstd.rlib` |
//!
//! This is intentionally a *driver* script: it shells out to `cargo build`
//! with the correct `--target` / `-Z build-std` flags.  It does not reimplement
//! cargo or rustc internals.

use std::{
    env, fs,
    path::{Path, PathBuf},
    process::{Command, ExitCode},
};

// ── CLI ───────────────────────────────────────────────────────────────────────

struct Args {
    toolchain_root: PathBuf,
    out_sysroot:    PathBuf,
    target_spec:    PathBuf,
    verbose:        bool,
}

fn parse_args() -> Result<Args, String> {
    let mut args = env::args().skip(1);
    let mut toolchain_root = None;
    let mut out_sysroot    = PathBuf::from("lythos-sysroot");
    let mut target_spec    = PathBuf::from("tools/lythos-toolchain/target-specs/x86_64-lythos-sysroot.json");
    let mut verbose        = false;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--toolchain-root" => {
                toolchain_root = Some(PathBuf::from(args.next().ok_or("missing value for --toolchain-root")?));
            }
            "--out-sysroot" => {
                out_sysroot = PathBuf::from(args.next().ok_or("missing value for --out-sysroot")?);
            }
            "--target-spec" => {
                target_spec = PathBuf::from(args.next().ok_or("missing value for --target-spec")?);
            }
            "--verbose" | "-v" => verbose = true,
            other => return Err(format!("unknown argument: {}", other)),
        }
    }

    Ok(Args {
        toolchain_root: toolchain_root.ok_or("--toolchain-root is required")?,
        out_sysroot,
        target_spec,
        verbose,
    })
}

// ── Build helpers ─────────────────────────────────────────────────────────────

fn run(cmd: &mut Command, verbose: bool) -> Result<(), String> {
    if verbose {
        eprintln!("[sysroot-builder] {:?}", cmd);
    }
    let status = cmd.status().map_err(|e| e.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("command failed with status {}", status))
    }
}

/// Stage 0: build core + compiler_builtins via `-Z build-std`.
fn stage0(args: &Args) -> Result<(), String> {
    eprintln!("==> Stage 0: core + compiler_builtins");
    run(
        Command::new("cargo")
            .args([
                "+nightly",
                "build",
                "--release",
                "-Z", "build-std=core,compiler_builtins",
                "-Z", "build-std-features=compiler-builtins-mem",
                "--target", args.target_spec.to_str().unwrap(),
                "--manifest-path", "tools/lythos-toolchain/lythos-libc/Cargo.toml",
            ])
            .env("RUSTFLAGS", "-C panic=abort"),
        args.verbose,
    )
}

/// Stage 1: build lythos-libc + lythos-unwind.
fn stage1(args: &Args) -> Result<(), String> {
    eprintln!("==> Stage 1: lythos-libc + lythos-unwind");

    run(
        Command::new("cargo")
            .args([
                "+nightly",
                "build",
                "--release",
                "-Z", "build-std=core,alloc,compiler_builtins",
                "--target", args.target_spec.to_str().unwrap(),
                "--manifest-path", "tools/lythos-toolchain/lythos-libc/Cargo.toml",
            ])
            .env("RUSTFLAGS", "-C panic=abort"),
        args.verbose,
    )?;

    run(
        Command::new("cargo")
            .args([
                "+nightly",
                "build",
                "--release",
                "-Z", "build-std=core,compiler_builtins",
                "--target", args.target_spec.to_str().unwrap(),
                "--manifest-path", "tools/lythos-toolchain/lythos-unwind/Cargo.toml",
            ])
            .env("RUSTFLAGS", "-C panic=abort"),
        args.verbose,
    )
}

/// Stage 2: build lythos-libstd.
fn stage2(args: &Args) -> Result<(), String> {
    eprintln!("==> Stage 2: lythos-libstd");
    run(
        Command::new("cargo")
            .args([
                "+nightly",
                "build",
                "--release",
                "-Z", "build-std=core,alloc,compiler_builtins",
                "--target", args.target_spec.to_str().unwrap(),
                "--manifest-path", "userspace/lib/lythos-libstd/Cargo.toml",
            ])
            .env("RUSTFLAGS", "-C panic=abort"),
        args.verbose,
    )
}

/// Install rlibs into the output sysroot tree expected by rustc.
fn install_sysroot(args: &Args) -> Result<(), String> {
    eprintln!("==> Installing sysroot to {:?}", args.out_sysroot);
    let lib_dir = args.out_sysroot.join("lib/rustlib/x86_64-lythos/lib");
    fs::create_dir_all(&lib_dir).map_err(|e| e.to_string())?;

    let target_dir = PathBuf::from("target/x86_64-lythos/release/deps");
    if !target_dir.exists() {
        return Err(format!("target dir not found: {:?}", target_dir));
    }

    for entry in fs::read_dir(&target_dir).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let path  = entry.path();
        let name  = path.file_name().unwrap_or_default().to_string_lossy();
        // Copy rlibs and static archives.
        if name.ends_with(".rlib") || name.ends_with(".a") {
            let dst = lib_dir.join(&*name);
            fs::copy(&path, &dst).map_err(|e| e.to_string())?;
            if args.verbose { eprintln!("  installed {:?}", dst); }
        }
    }

    // Write a CARGO_HOME-compatible sysroot marker.
    fs::write(
        args.out_sysroot.join("lib/rustlib/x86_64-lythos/rust-installer-version"),
        "3\n",
    ).map_err(|e| e.to_string())?;

    Ok(())
}

// ── Entry point ───────────────────────────────────────────────────────────────

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a)  => a,
        Err(e) => { eprintln!("error: {}", e); return ExitCode::FAILURE; }
    };

    let steps: &[(&str, fn(&Args) -> Result<(), String>)] = &[
        ("stage0", stage0),
        ("stage1", stage1),
        ("stage2", stage2),
        ("install", install_sysroot),
    ];

    for (name, step) in steps {
        if let Err(e) = step(&args) {
            eprintln!("error in {}: {}", name, e);
            return ExitCode::FAILURE;
        }
    }

    eprintln!("==> Sysroot built at {:?}", args.out_sysroot);
    eprintln!("    Add to .cargo/config.toml:");
    eprintln!("    [build]");
    eprintln!("    rustflags = [\"--sysroot\", \"{}\"]", args.out_sysroot.display());
    ExitCode::SUCCESS
}
