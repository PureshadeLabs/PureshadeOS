//! shade-build — host CLI for `shade build` (the seed vehicle,
//! docs/shade-pkg/09-bootstrap.md §2, alongside the host `shadec` binary).
//!
//!   shade-build [--store-root DIR] [--build-root DIR] [--log-root DIR]
//!               [--toolchain ID] [--jobs N] [--keep-failed] [--dry-run]
//!               <file-or-expr>
//!
//! Evaluates the recipe, orders its derivation closure, and satisfies every
//! derivation LOOKUP-THEN-BUILD via the permissive sandbox. Prints the root
//! output store path on success (shade-pkg 07 §`shade build`). `--dry-run`
//! plans and prints the path without touching build/store state (07 §1).
//!
//! In the target system this dispatch lives behind `shade build` in the
//! unified OROS `shade` binary — blocked on argv plumbing through the ABI
//! (see pkg/shade/src/main.rs).

use std::path::PathBuf;
use std::process::ExitCode;

use shade_build::{
    Built, DbRegistrar, Executor, LocalStore, PermissiveSandbox, RecipeRef, Resolver,
    CANONICAL_BUILD_ROOT, CANONICAL_LOG_ROOT,
};
use shade_store::CANONICAL_STORE_ROOT;
use shadec::io::HostIo;

struct Opts {
    store_root: PathBuf,
    build_root: PathBuf,
    log_root: PathBuf,
    toolchain: Option<String>,
    jobs: u32,
    keep_failed: bool,
    dry_run: bool,
    target: String,
}

fn usage() -> ! {
    eprintln!(
        "usage: shade-build [--store-root DIR] [--build-root DIR] [--log-root DIR]\n\
         \x20                  [--toolchain ID] [--jobs N] [--keep-failed] [--dry-run]\n\
         \x20                  <file-or-expr>"
    );
    std::process::exit(2);
}

fn main() -> ExitCode {
    let mut opts = Opts {
        store_root: PathBuf::from(CANONICAL_STORE_ROOT),
        build_root: PathBuf::from(CANONICAL_BUILD_ROOT),
        log_root: PathBuf::from(CANONICAL_LOG_ROOT),
        toolchain: None,
        jobs: 1,
        keep_failed: false,
        dry_run: false,
        target: String::new(),
    };
    let mut positional: Vec<String> = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let path_opt = |args: &mut dyn Iterator<Item = String>| -> PathBuf {
            match args.next() {
                Some(v) => PathBuf::from(v),
                None => usage(),
            }
        };
        match a.as_str() {
            "--store-root" => opts.store_root = path_opt(&mut args),
            "--build-root" => opts.build_root = path_opt(&mut args),
            "--log-root" => opts.log_root = path_opt(&mut args),
            "--toolchain" => match args.next() {
                Some(t) => opts.toolchain = Some(t),
                None => usage(),
            },
            "--jobs" => match args.next().and_then(|n| n.parse().ok()) {
                Some(n) => opts.jobs = n,
                None => usage(),
            },
            "--keep-failed" => opts.keep_failed = true,
            "--dry-run" => opts.dry_run = true,
            _ => positional.push(a),
        }
    }
    if positional.len() != 1 {
        usage();
    }
    opts.target = positional.remove(0);

    // Big worker stack: the evaluator's MAX_DEPTH resource guard must trip
    // before the OS stack does (same as the shadec host binary).
    let handle = std::thread::Builder::new()
        .stack_size(512 * 1024 * 1024)
        .spawn(move || run(opts))
        .expect("spawn worker");
    match handle.join() {
        Ok(code) => code,
        Err(_) => ExitCode::from(101),
    }
}

fn run(opts: Opts) -> ExitCode {
    let io = HostIo;

    // File reference vs inline expression — same heuristic as the shadec
    // host binary: explicit path syntax or an existing .shade file is a
    // file; anything else evaluates with the cwd as base directory.
    let t = &opts.target;
    let is_file_ref = t.starts_with("./")
        || t.starts_with("../")
        || t.starts_with('/')
        || (t.ends_with(".shade") && std::fs::metadata(t).is_ok());
    let cwd = match std::env::current_dir() {
        Ok(d) => d.to_string_lossy().into_owned(),
        Err(e) => {
            eprintln!("shade-build: cannot determine working directory: {e}");
            return ExitCode::from(1);
        }
    };
    let recipe = if is_file_ref {
        let abs = if t.starts_with('/') { t.clone() } else { format!("{cwd}/{t}") };
        RecipeRef::File(abs)
    } else {
        RecipeRef::Expr { src: t.clone(), base_dir: cwd }
    };

    if opts.dry_run {
        return match shade_build::plan(&recipe, &opts.store_root, opts.toolchain.as_deref(), &io)
        {
            Ok(p) => {
                println!("{}", p.paths.out_path);
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("shade-build: {e} (errno {})", e.errno() as i64);
                ExitCode::from(1)
            }
        };
    }

    let local = LocalStore;
    let resolvers: [&dyn Resolver; 1] = [&local];
    let sandbox = PermissiveSandbox;
    // Real registration (06 §5): each realization lands in /shade/db/ so
    // `shade gc` can compute the reference closure.
    let registrar = DbRegistrar::for_store_root(&opts.store_root);
    let mut exec = Executor::new(
        &opts.store_root,
        &opts.build_root,
        &opts.log_root,
        &resolvers,
        &sandbox,
        &registrar,
    );
    exec.keep_failed = opts.keep_failed;
    exec.jobs = opts.jobs;

    match exec.run(&recipe, opts.toolchain.as_deref(), &io) {
        Ok(outcome) => {
            for (store_name, built) in &outcome.results {
                match built {
                    Built::Resolved { source, .. } => {
                        eprintln!("shade-build: {store_name} — hit ({source})")
                    }
                    Built::Realized { .. } => eprintln!("shade-build: {store_name} — built"),
                }
            }
            println!("{}", outcome.root_result().out_path().display());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("shade-build: {e} (errno {})", e.errno() as i64);
            ExitCode::from(1)
        }
    }
}
