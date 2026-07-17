//! shade-gc — host CLI for `shade gc` and GC-root management (the seed
//! vehicle, docs/shade-pkg/09-bootstrap.md §2, alongside the host `shade-build`
//! binary). In the target system this dispatch lives behind `shade gc` in the
//! unified OROS `shade` binary — blocked on argv plumbing through the ABI (see
//! pkg/shade/src/main.rs).
//!
//!   shade-gc [--store-root DIR] [gc] [--force] [--dry-run]
//!   shade-gc [--store-root DIR] add-root <name> <store-path>
//!   shade-gc [--store-root DIR] del-root <name>
//!   shade-gc [--store-root DIR] list-roots

use std::path::PathBuf;
use std::process::ExitCode;

use shade_store::CANONICAL_STORE_ROOT;
use shade_store_db::{GcOptions, StoreDb};

fn usage() -> ! {
    eprintln!(
        "usage: shade-gc [--store-root DIR] [gc] [--force] [--dry-run]\n\
         \x20      shade-gc [--store-root DIR] add-root <name> <store-path>\n\
         \x20      shade-gc [--store-root DIR] del-root <name>\n\
         \x20      shade-gc [--store-root DIR] list-roots"
    );
    std::process::exit(2);
}

fn main() -> ExitCode {
    let mut store_root = PathBuf::from(CANONICAL_STORE_ROOT);
    let mut force = false;
    let mut dry_run = false;
    let mut positional: Vec<String> = Vec::new();

    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--store-root" => match args.next() {
                Some(v) => store_root = PathBuf::from(v),
                None => usage(),
            },
            "--force" => force = true,
            "--dry-run" => dry_run = true,
            "-h" | "--help" => usage(),
            _ => positional.push(a),
        }
    }

    let db = StoreDb::for_store_root(&store_root);
    let cmd = positional.first().map(String::as_str).unwrap_or("gc");

    match cmd {
        "gc" => run_gc(&db, GcOptions { force, dry_run }),
        "add-root" => {
            if positional.len() != 3 {
                usage();
            }
            match db.add_root(&positional[1], &positional[2]) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("shade-gc: add-root: {e}");
                    ExitCode::from(1)
                }
            }
        }
        "del-root" => {
            if positional.len() != 2 {
                usage();
            }
            match db.remove_root(&positional[1]) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("shade-gc: del-root: {e}");
                    ExitCode::from(1)
                }
            }
        }
        "list-roots" => match db.list_roots() {
            Ok(roots) => {
                for (name, target) in roots {
                    println!("{name}\t{target}");
                }
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("shade-gc: list-roots: {e}");
                ExitCode::from(1)
            }
        },
        other => {
            eprintln!("shade-gc: unknown command {other:?}");
            usage();
        }
    }
}

fn run_gc(db: &StoreDb<shade_store::HostFs>, opts: GcOptions) -> ExitCode {
    match db.gc(&opts) {
        Ok(report) => {
            let verb = if report.dry_run { "would collect" } else { "collected" };
            eprintln!(
                "shade-gc: {verb} {} path(s), kept {}, {} freed{}",
                report.collected.len(),
                report.kept,
                human_bytes(report.freed_bytes),
                if report.pruned_roots > 0 {
                    format!(", pruned {} dangling root(s)", report.pruned_roots)
                } else {
                    String::new()
                },
            );
            for name in &report.collected {
                println!("{name}");
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("shade-gc: {e}");
            ExitCode::from(1)
        }
    }
}

fn human_bytes(n: u64) -> String {
    const UNITS: &[&str] = &["B", "KiB", "MiB", "GiB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}
