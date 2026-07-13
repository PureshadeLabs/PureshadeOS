//! shade-gen — host CLI for generations, profiles, and activation (the seed
//! vehicle, docs/shade-pkg/09-bootstrap.md §2, alongside `shade-build` and
//! `shade-gc`).
//!
//!   shade-gen [--shade-root DIR] [--cfg-root DIR] [--build-root DIR]
//!             [--log-root DIR] [--toolchain ID] [--jobs N] [--user NAME]
//!             [--lth-bin PATH] <command> [args]
//!
//! Commands (07 §2 verbs, flattened for the seed binary):
//!   os-rebuild [<prism>[#<sel>]]   build + activate a system generation
//!                                  (default source: pointer, else the
//!                                  bootstrap default — 10 §4)
//!   home-rebuild <prism>[#<sel>]   build + switch the --user profile line
//!   list                           list generations (--user for a user line)
//!   rollback [N]                   new generation copying N's manifest
//!                                  (default: previous), then activate
//!   boot                           activate the pre-built pinned system
//!                                  generation; NEVER builds (10 §6)
//!
//! In the target system these live behind `shade os rebuild`,
//! `shade home rebuild`, `shade generations`, `shade rollback` in the unified
//! OROS `shade` binary — blocked on argv plumbing (pkg/shade/src/main.rs).

use std::path::PathBuf;
use std::process::ExitCode;

use shade_build::{CANONICAL_BUILD_ROOT, CANONICAL_LOG_ROOT};
use shade_gen::{
    boot_activate, home_rebuild, os_rebuild, BuildRoots, GenLine, CANONICAL_CFG_ROOT,
};
use shade_store::CANONICAL_STORE_ROOT;
use shadec::io::HostIo;

struct Opts {
    shade_root: PathBuf,
    cfg_root: PathBuf,
    build_root: PathBuf,
    log_root: PathBuf,
    toolchain: Option<String>,
    jobs: u32,
    user: Option<String>,
    lth_bin: Option<PathBuf>,
    command: String,
    args: Vec<String>,
}

fn usage() -> ! {
    eprintln!(
        "usage: shade-gen [--shade-root DIR] [--cfg-root DIR] [--build-root DIR]\n\
         \x20                [--log-root DIR] [--toolchain ID] [--jobs N] [--user NAME]\n\
         \x20                [--lth-bin PATH] <command> [args]\n\
         commands:\n\
         \x20 os-rebuild [<prism>[#<sel>]]   build + activate a system generation\n\
         \x20 home-rebuild <prism>[#<sel>]   build + switch the --user profile\n\
         \x20 list                           list generations (--user for a user line)\n\
         \x20 rollback [N]                   append rollback generation, activate\n\
         \x20 boot                           activate pinned pre-built generation (never builds)"
    );
    std::process::exit(2);
}

fn main() -> ExitCode {
    let mut opts = Opts {
        shade_root: PathBuf::from("/shade"),
        cfg_root: PathBuf::from(CANONICAL_CFG_ROOT),
        build_root: PathBuf::from(CANONICAL_BUILD_ROOT),
        log_root: PathBuf::from(CANONICAL_LOG_ROOT),
        toolchain: None,
        jobs: 1,
        user: None,
        lth_bin: None,
        command: String::new(),
        args: Vec::new(),
    };
    // Default store root is derived from --shade-root, not CANONICAL_STORE_ROOT,
    // so one flag repoints the whole tree; assert the constants agree.
    debug_assert_eq!(CANONICAL_STORE_ROOT, "/shade/store");

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
            "--shade-root" => opts.shade_root = path_opt(&mut args),
            "--cfg-root" => opts.cfg_root = path_opt(&mut args),
            "--build-root" => opts.build_root = path_opt(&mut args),
            "--log-root" => opts.log_root = path_opt(&mut args),
            "--lth-bin" => opts.lth_bin = Some(path_opt(&mut args)),
            "--toolchain" => match args.next() {
                Some(t) => opts.toolchain = Some(t),
                None => usage(),
            },
            "--jobs" => match args.next().and_then(|n| n.parse().ok()) {
                Some(n) => opts.jobs = n,
                None => usage(),
            },
            "--user" => match args.next() {
                Some(u) => opts.user = Some(u),
                None => usage(),
            },
            _ => positional.push(a),
        }
    }
    if positional.is_empty() {
        usage();
    }
    opts.command = positional.remove(0);
    opts.args = positional;

    // Big worker stack: the evaluator recurses (same as shade-build's binary).
    let handle = std::thread::Builder::new()
        .stack_size(512 * 1024 * 1024)
        .spawn(move || run(opts))
        .expect("spawn worker");
    match handle.join() {
        Ok(code) => code,
        Err(_) => ExitCode::from(101),
    }
}

fn run(mut opts: Opts) -> ExitCode {
    let io = HostIo;
    // Roots must be absolute: build phases run with the scratch dir as cwd,
    // so a relative `$out`/store path would resolve against the wrong base.
    if let Ok(cwd) = std::env::current_dir() {
        for p in [&mut opts.shade_root, &mut opts.cfg_root, &mut opts.build_root, &mut opts.log_root]
        {
            if !p.is_absolute() {
                *p = cwd.join(&*p);
            }
        }
        if let Some(l) = &mut opts.lth_bin {
            if !l.is_absolute() {
                *l = cwd.join(&*l);
            }
        }
    }
    let store_root = opts.shade_root.join("store");
    let roots = BuildRoots {
        store: &store_root,
        build: &opts.build_root,
        log: &opts.log_root,
    };
    let line = || match &opts.user {
        Some(u) => GenLine::user(&opts.shade_root, u),
        None => GenLine::system(&opts.shade_root),
    };

    let r: Result<(), Box<dyn std::error::Error>> = (|| {
        match opts.command.as_str() {
            "os-rebuild" => {
                if opts.args.len() > 1 {
                    usage();
                }
                let out = os_rebuild(
                    &opts.shade_root,
                    &opts.cfg_root,
                    opts.args.first().map(String::as_str),
                    &roots,
                    opts.toolchain.as_deref(),
                    opts.jobs,
                    opts.lth_bin.as_deref(),
                    &io,
                )?;
                // 07 §4: every activation prints the generation number.
                println!(
                    "system generation {} activated ({} package(s), prism {}{}{})",
                    out.generation,
                    out.packages,
                    out.prism,
                    if out.selector.is_empty() { "" } else { "#" },
                    out.selector
                );
            }
            "home-rebuild" => {
                let Some(user) = opts.user.as_deref() else {
                    eprintln!("shade-gen: home-rebuild requires --user");
                    usage();
                };
                if opts.args.len() != 1 {
                    usage();
                }
                let (n, pkgs) = home_rebuild(
                    &opts.shade_root,
                    user,
                    &opts.args[0],
                    &roots,
                    opts.toolchain.as_deref(),
                    opts.jobs,
                    &io,
                )?;
                println!("user {user} generation {n} activated ({pkgs} package(s))");
            }
            "list" => {
                if !opts.args.is_empty() {
                    usage();
                }
                for g in line().list()? {
                    println!(
                        "{}{}\t{}\t{} package(s)\t{}",
                        g.number,
                        if g.current { "*" } else { "" },
                        shade_gen::rfc3339_utc(g.manifest.created),
                        g.manifest.packages.len(),
                        g.manifest.reason
                    );
                }
            }
            "rollback" => {
                if opts.args.len() > 1 {
                    usage();
                }
                let target = match opts.args.first() {
                    Some(s) => Some(s.parse().map_err(|_| "rollback target must be a number")?),
                    None => None,
                };
                let n = line().rollback(target)?;
                // System-line rollback re-pins boot (line 3 of the pointer)
                // so a reboot stays on the rolled-back-to configuration.
                if opts.user.is_none() {
                    shade_gen::repin_generation(&opts.cfg_root, n)?;
                }
                println!("generation {n} activated (rollback)");
            }
            "boot" => {
                if !opts.args.is_empty() {
                    usage();
                }
                let out =
                    boot_activate(&opts.shade_root, &opts.cfg_root, opts.lth_bin.as_deref())?;
                println!(
                    "boot: system generation {} activated{}",
                    out.generation,
                    if out.fell_back { " (last-good fallback)" } else { "" }
                );
            }
            _ => usage(),
        }
        Ok(())
    })();

    match r {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("shade-gen: {e}");
            ExitCode::from(1)
        }
    }
}
