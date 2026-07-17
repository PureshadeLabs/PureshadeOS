//! shade — Lythos package manager (on-target CLI).
//!
//! Dispatches into the no_std shade crates over the OROS seams: the
//! evaluator through [`shadec::io::OrosIo`], every filesystem operation
//! through [`shade_store::OrosFs`]. The full chain runs on target:
//! eval → CDF → build → input-addressed store → generation → activation → gc.
//!
//! ## Bringup phase interpreter
//!
//! There is no shell on OROS and the native capability-restricted
//! `BuildSandbox` (shade-pkg 06 §3.2) is still deferred on a kernel per-task
//! fs namespace, so phases run through a tiny built-in interpreter instead of
//! `sh -c`. It supports exactly the forms the bringup derivations use —
//! `true`, `mkdir -p <dir>`, `printf <text> > <file>` (with `$out`/`$OUT`
//! substitution and `\n`/`\t`/`\\` escapes) — and rejects anything else
//! loudly. Dependency closures are likewise deferred to the native sandbox:
//! only derivations whose `dep.*` inputs are already in the store build.
//! The derivation contract (cwd, `$out`, verify-then-realize, log, errno) is
//! the executor's; this is a `BuildSandbox`-shaped vehicle, not a new model.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use lythos_rt::println;
use shade_build::{plan_graph, BuildError, BuildPlan, RecipeRef};
use shade_gen::{boot_activate_on, GenLine, PackageEntry};
use shade_store::backend::{self, join};
use shade_store::{OrosFs, StoreFs};
use shade_store_db::{GcOptions, StoreDb};
use shadec::io::OrosIo;

/// Canonical roots (docs/shade-pkg/02-store.md §1, docs/spec/fhs.md).
const SHADE_ROOT: &str = "/shade";
const STORE_ROOT: &str = "/shade/store";
const CFG_ROOT: &str = "/cfg/shade";
const BUILD_ROOT: &str = shade_build::CANONICAL_BUILD_ROOT;
const LOG_ROOT: &str = shade_build::CANONICAL_LOG_ROOT;
/// Ambient toolchain identity folded into every derivation's CDF
/// (input-addressing, shade-pkg 06 §4). A real driver resolves this from the
/// active prism; bringup pins a fixed id so store paths are reproducible.
const TOOLCHAIN: &str = "bringup-tc-1";

lythos_rt::entry!(main);

fn main() {
    let args: Vec<&str> = lythos_rt::args::args().collect();
    let code = match args.get(1).copied() {
        Some("--version") => {
            println!("shade 0.2 — Lythos package manager");
            0
        }
        Some("build") => match args.get(2).copied() {
            Some(path) => cmd_build(&RecipeRef::File(path.to_string())),
            None => {
                println!("shade build: missing <recipe.shade>");
                1
            }
        },
        Some("generations") | Some("list") => cmd_generations(),
        Some("rollback") => cmd_rollback(),
        Some("gc") => cmd_gc(args.get(2).copied() == Some("--dry-run")),
        Some("boot") => cmd_boot(),
        Some("e2e") => cmd_e2e(),
        Some("help") | Some("--help") | None => {
            print_usage();
            0
        }
        Some(cmd) => {
            println!("shade: unknown command `{}`", cmd);
            print_usage();
            1
        }
    };
    if code != 0 {
        // No exit-status plumbing in SYS_TASK_EXIT yet; the nonzero path is
        // visible through the printed error.
        println!("shade: exited with status {}", code);
    }
}

fn print_usage() {
    println!("shade 0.2 — Lythos package manager");
    println!();
    println!("USAGE:");
    println!("  shade <command> [args]");
    println!();
    println!("COMMANDS:");
    println!("  build <recipe>   evaluate + build into /shade/store (lookup-then-build)");
    println!("  generations      list the system generation line");
    println!("  rollback         roll the system line back one generation");
    println!("  gc [--dry-run]   mark-and-sweep unreachable store paths");
    println!("  boot             activate the pre-built system generation (no build)");
    println!("  e2e              bringup probe: build->store->gen->activate->rollback->gc");
}

// ---- build ----------------------------------------------------------------------

struct BuiltOne {
    plan: BuildPlan,
    out_path: String,
    /// False = pure lookup (store hit), true = built + realized this run.
    built: bool,
}

fn cmd_build(recipe: &RecipeRef) -> i32 {
    match build_recipe(recipe) {
        Ok(b) => {
            println!(
                "shade: {} — {}",
                b.plan.paths.store_name,
                if b.built { "built" } else { "hit (local)" }
            );
            println!("{}", b.out_path);
            0
        }
        Err(e) => {
            println!("shade build: {}", e);
            1
        }
    }
}

/// The eval → address → LOOKUP-THEN-BUILD → realize → register pipeline for
/// one root derivation, over the OROS seams.
fn build_recipe(recipe: &RecipeRef) -> Result<BuiltOne, String> {
    let graph = plan_graph(recipe, STORE_ROOT, Some(TOOLCHAIN), &OrosIo)
        .map_err(|e| format!("{e} (errno {})", err_no(&e)))?;
    let plan = graph.root;
    let mut fs = OrosFs;

    let entries = shade_cdf::parse(&plan.cdf).map_err(|e| format!("unreadable CDF: {e}"))?;

    // LOOKUP first: the immutable store's "exists => complete" contract.
    if fs.exists(&plan.paths.out_path) {
        return Ok(BuiltOne { out_path: plan.paths.out_path.clone(), plan, built: false });
    }

    // Inputs must already be realized — dep closure scheduling is deferred
    // with the native sandbox (module docs).
    for (k, v) in entries.iter() {
        if k.starts_with("dep.") && !fs.exists(v) {
            return Err(format!(
                "input {v} is not in the store (dep closures are deferred with the native sandbox)"
            ));
        }
    }
    if entries.get("builder").map(String::as_str) == Some("fetch") {
        return Err("source derivation miss; the fetcher is not implemented (errno ENOSYS)".into());
    }

    let phases = indexed(&entries, "phase.");
    let outputs = indexed(&entries, "output.");

    let (_scratch, staging) =
        shade_build::prepare_scratch(&mut fs, BUILD_ROOT, &plan.paths.store_name)
            .map_err(|e| format!("prepare scratch: {e:?}"))?;

    let mut log = String::new();
    for (i, phase) in phases.iter().enumerate() {
        log.push_str("phase ");
        log.push_str(&i.to_string());
        log.push_str(": ");
        log.push_str(phase);
        log.push('\n');
        if let Err(e) = run_phase(&mut fs, &staging, phase) {
            log.push_str(&format!("FAILED: {e}\n"));
            let _ =
                shade_build::write_build_log(&mut fs, LOG_ROOT, &plan.paths.store_name, log.as_bytes());
            // Scratch kept for autopsy on failure (--keep-failed is implied
            // until argv gains flags); the store is untouched.
            return Err(format!("phase {i} failed: {e} (log: {})",
                shade_build::build_log_path(LOG_ROOT, &plan.paths.store_name)));
        }
    }

    // Verify declared outputs under the staging tree before any store write.
    for rel in &outputs {
        let p = join(&staging, rel);
        if !fs.exists(&p) {
            return Err(format!("declared output `{rel}` was not produced by the build"));
        }
    }

    let realized =
        shade_store::realize_cdf(&mut fs, STORE_ROOT, &plan.name, &plan.version, &plan.cdf, &staging)
            .map_err(|e| format!("realize: {e}"))?;
    let _ = shade_build::write_build_log(&mut fs, LOG_ROOT, &plan.paths.store_name, log.as_bytes());
    shade_build::clean_scratch(&mut fs, BUILD_ROOT, &plan.paths.store_name);

    // Register with the store db (refs scanned from the output bytes).
    let db = StoreDb::with_backend(OrosFs, SHADE_ROOT);
    let declared: Vec<String> =
        entries.iter().filter(|(k, _)| k.starts_with("dep.")).map(|(_, v)| v.clone()).collect();
    db.register(
        &realized.paths.out_path,
        &realized.paths.digest,
        &realized.paths.store_name,
        &shade_cdf::blake3_hex(&plan.cdf),
        &declared,
    )
    .map_err(|e| format!("register: {e:?}"))?;

    Ok(BuiltOne { out_path: realized.paths.out_path.clone(), plan, built: true })
}

fn err_no(e: &BuildError) -> i64 {
    e.errno() as i64
}

/// `phase.<i>` / `output.<i>` values in index order (the executor's `indexed`).
fn indexed(entries: &alloc::collections::BTreeMap<String, String>, prefix: &str) -> Vec<String> {
    let mut keyed: Vec<(usize, String)> = entries
        .iter()
        .filter_map(|(k, v)| {
            k.strip_prefix(prefix).and_then(|i| i.parse().ok()).map(|i| (i, v.clone()))
        })
        .collect();
    keyed.sort_by_key(|(i, _)| *i);
    keyed.into_iter().map(|(_, v)| v).collect()
}

// ---- The bringup phase interpreter ------------------------------------------------

/// Run one phase command (module docs: `true`, `mkdir -p`, `printf … > …`).
fn run_phase(fs: &mut OrosFs, staging: &str, phase: &str) -> Result<(), String> {
    let cmd = phase.replace("$out", staging).replace("$OUT", staging);
    let toks = tokenize(&cmd)?;
    let toks: Vec<&str> = toks.iter().map(String::as_str).collect();
    match toks.as_slice() {
        [] | ["true"] => Ok(()),
        ["mkdir", "-p", path] => {
            backend::create_dir_all(fs, path).map_err(|e| format!("mkdir -p {path}: {e:?}"))
        }
        ["printf", text, ">", path] => {
            let bytes = printf_unescape(text);
            fs.write_file(path, bytes.as_bytes(), false)
                .map_err(|e| format!("printf > {path}: {e:?}"))
        }
        _ => Err(format!("unsupported phase for the bringup interpreter: `{cmd}`")),
    }
}

/// Whitespace tokenizer with single/double quotes (no nesting, no escapes
/// inside quotes beyond taking the span verbatim).
fn tokenize(s: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    for c in s.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => cur.push(c),
            None => match c {
                '\'' | '"' => quote = Some(c),
                c if c.is_whitespace() => {
                    if !cur.is_empty() {
                        out.push(core::mem::take(&mut cur));
                    }
                }
                c => cur.push(c),
            },
        }
    }
    if quote.is_some() {
        return Err("unterminated quote".into());
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    Ok(out)
}

/// `printf`'s backslash escapes, the subset bringup recipes use.
fn printf_unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c == '\\' {
            match it.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('\\') => out.push('\\'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

// ---- generations / rollback / gc / boot -------------------------------------------

fn cmd_generations() -> i32 {
    let line = GenLine::system_on(OrosFs, SHADE_ROOT);
    match line.list() {
        Ok(gens) if gens.is_empty() => {
            println!("shade: no generations");
            0
        }
        Ok(gens) => {
            for g in gens {
                println!(
                    "{}{}  parent={}  packages={}  reason={}",
                    g.number,
                    if g.current { "*" } else { " " },
                    g.manifest.parent,
                    g.manifest.packages.len(),
                    g.manifest.reason,
                );
            }
            0
        }
        Err(e) => {
            println!("shade generations: {}", e);
            1
        }
    }
}

fn cmd_rollback() -> i32 {
    let line = GenLine::system_on(OrosFs, SHADE_ROOT);
    match line.rollback(None) {
        Ok(n) => {
            // System-line rollback re-pins the pointer so the next boot
            // activates the rolled-back generation (10 §4).
            if let Err(e) =
                shade_gen::repin_generation_on(&mut OrosFs, CFG_ROOT, n)
            {
                println!("shade rollback: pointer re-pin failed: {}", e);
                return 1;
            }
            println!("shade: rolled back — new generation {} active", n);
            0
        }
        Err(e) => {
            println!("shade rollback: {}", e);
            1
        }
    }
}

fn cmd_gc(dry_run: bool) -> i32 {
    let db = StoreDb::with_backend(OrosFs, SHADE_ROOT);
    match db.gc(&GcOptions { dry_run, force: false }) {
        Ok(r) => {
            println!(
                "shade gc: {} kept, {} collected, {} bytes freed{}{}",
                r.kept,
                r.collected.len(),
                r.freed_bytes,
                if r.pruned_roots > 0 { " (pruned dangling roots)" } else { "" },
                if r.dry_run { " [dry run]" } else { "" },
            );
            for name in &r.collected {
                println!("  - {}", name);
            }
            0
        }
        Err(e) => {
            println!("shade gc: {:?}", e);
            1
        }
    }
}

fn cmd_boot() -> i32 {
    match boot_activate_on(OrosFs, SHADE_ROOT, CFG_ROOT, None) {
        Ok(o) => {
            println!(
                "[shade] boot: activated generation {} (pinned={:?}, fell_back={}) — no build at boot",
                o.generation, o.pinned, o.fell_back,
            );
            0
        }
        Err(shade_gen::GenError::NoGeneration) => {
            println!("[shade] boot: no generation to activate (fresh system)");
            0
        }
        Err(e) => {
            println!("[shade] boot: {}", e);
            1
        }
    }
}

// ---- e2e bringup probe -------------------------------------------------------------

/// The trivial one-file derivation — the same gate case as
/// `shade-build::tests::executor_gate_build_then_pure_lookup`, phased for the
/// bringup interpreter.
fn demo_recipe() -> RecipeRef {
    RecipeRef::Expr {
        src: r#"
derivation {
  name = "demo";
  version = "1.0";
  system = "x86_64-oros";
  phases = [ "mkdir -p $out/bin" "printf hi > $out/bin/demo" ];
  outputs = { bin = [ "demo" ]; };
}"#
        .to_string(),
        base_dir: "/".to_string(),
    }
}

/// An unrooted throwaway derivation — gc bait.
fn junk_recipe() -> RecipeRef {
    RecipeRef::Expr {
        src: r#"
derivation {
  name = "junk";
  version = "0.0";
  system = "x86_64-oros";
  phases = [ "mkdir -p $out/bin" "printf begone > $out/bin/junk" ];
  outputs = { bin = [ "junk" ]; };
}"#
        .to_string(),
        base_dir: "/".to_string(),
    }
}

macro_rules! stage {
    ($stage:expr, $cond:expr, $($why:tt)*) => {
        if $cond {
            println!("[shade-e2e] {}: ok", $stage);
        } else {
            println!("[shade-e2e] {}: FAILED — {}", $stage, format!($($why)*));
            return 1;
        }
    };
}

/// The full on-device chain, stage by stage; the first failing stage names
/// itself and stops the run.
fn cmd_e2e() -> i32 {
    let mut fs = OrosFs;

    // Stage 0: the store mount is up (lythd mounts it at init).
    stage!("store-mounted", fs.exists(STORE_ROOT), "/shade/store missing — lythd mount failed?");

    // Stage 1: first build — a real input-addressed store path.
    let b1 = match build_recipe(&demo_recipe()) {
        Ok(b) => b,
        Err(e) => {
            println!("[shade-e2e] build-1: FAILED — {}", e);
            return 1;
        }
    };
    // On the PERSISTENT store, build-1 is a fresh build only on the very first
    // boot against a blank store.img; every later boot (a full power cycle
    // against the same store.img) finds the output already realized — a HIT.
    // That hit is the cold-boot persistence proof: the store content survived
    // the power cycle. Accept either outcome; the path/readback/.drv checks
    // below validate the content is correct regardless of built-vs-hit.
    println!(
        "[shade-e2e] build-1: {} ({})",
        b1.out_path,
        if b1.built { "built — fresh store" } else { "hit — persisted across a prior boot" }
    );
    stage!(
        "build-1-realized",
        fs.exists(&b1.out_path),
        "store path missing after build-1: {}",
        b1.out_path
    );
    stage!(
        "build-1-path",
        b1.out_path.starts_with("/shade/store/") && b1.out_path.ends_with("-demo-1.0"),
        "malformed store path {}",
        b1.out_path
    );

    // Readback through the sealed store.
    let bytes = fs.read_file(&join(&b1.out_path, "bin/demo"));
    stage!(
        "build-1-readback",
        bytes.as_deref() == Ok(b"hi"),
        "bin/demo readback mismatch: {:?}",
        bytes
    );
    // The .drv sits next to the output, byte-equal to the CDF.
    let drv = fs.read_file(&b1.plan.paths.drv_path);
    stage!(
        "build-1-drv",
        drv.as_deref() == Ok(b1.plan.cdf.as_slice()),
        ".drv missing or not byte-equal to the CDF"
    );

    // Stage 2: second build of the same derivation — a pure lookup.
    let b2 = match build_recipe(&demo_recipe()) {
        Ok(b) => b,
        Err(e) => {
            println!("[shade-e2e] build-2: FAILED — {}", e);
            return 1;
        }
    };
    stage!("build-2-pure-lookup", !b2.built, "second build must be a hit, not a rebuild");
    stage!("build-2-same-path", b2.out_path == b1.out_path, "path changed across runs");

    // Stage 3: generation referencing the store path; activate; live view.
    let line = GenLine::system_on(OrosFs, SHADE_ROOT);
    let pkg = PackageEntry {
        name: "demo".to_string(),
        version: "1.0".to_string(),
        store_path: b1.out_path.clone(),
        requested: true,
    };
    let g1 = match line.create(core::slice::from_ref(&pkg), None, "e2e initial", 0) {
        Ok(n) => n,
        Err(e) => {
            println!("[shade-e2e] gen-create: FAILED — {}", e);
            return 1;
        }
    };
    stage!("gen-activate", line.activate(g1).is_ok(), "activate({g1}) failed");
    stage!(
        "gen-current",
        line.current().ok().flatten() == Some(g1),
        "current does not point at {g1}"
    );
    // The live view resolves through `current` (VFS symlink following:
    // current -> N, profile/bin/demo -> absolute store path).
    let live = join(SHADE_ROOT, "gen/system/current/profile/bin/demo");
    let bytes = fs.read_file(&live);
    stage!(
        "gen-live-view",
        bytes.as_deref() == Ok(b"hi"),
        "read through {live} mismatch: {:?}",
        bytes
    );

    // Stage 4: a second generation, then rollback — the flip both ways.
    let g2 = match line.create(core::slice::from_ref(&pkg), None, "e2e second", g1) {
        Ok(n) => n,
        Err(e) => {
            println!("[shade-e2e] gen-2-create: FAILED — {}", e);
            return 1;
        }
    };
    stage!("gen-2-activate", line.activate(g2).is_ok(), "activate({g2}) failed");
    stage!(
        "gen-2-current",
        line.current().ok().flatten() == Some(g2),
        "current does not point at {g2}"
    );
    let g3 = match line.rollback(None) {
        Ok(n) => n,
        Err(e) => {
            println!("[shade-e2e] rollback: FAILED — {}", e);
            return 1;
        }
    };
    println!("[shade-e2e] rollback: {} -> new generation {}", g2, g3);
    stage!("rollback-flip", line.current().ok().flatten() == Some(g3), "current not at {g3}");
    stage!(
        "rollback-parent",
        line.read_manifest(g3).ok().flatten().map(|m| m.parent) == Some(g1),
        "rolled-back generation does not derive from {g1}"
    );

    // Stage 4b: boot activation — the no-build path. `boot_activate` takes no
    // evaluator/builder/recipe (a boot-time build is structurally
    // unrepresentable — that IS the no-build-at-boot invariant), and with no
    // pointer it activates whatever `current` already names. Across a real
    // power cycle the RAM store's bits are gone (content-addressed store is
    // volatile by design); this proves the activation path itself builds
    // nothing, within the session.
    let boot = match boot_activate_on(OrosFs, SHADE_ROOT, CFG_ROOT, None) {
        Ok(o) => o,
        Err(e) => {
            println!("[shade-e2e] boot-activate: FAILED — {}", e);
            return 1;
        }
    };
    println!(
        "[shade-e2e] boot-activate: generation {} (pinned={:?}, fell_back={})",
        boot.generation, boot.pinned, boot.fell_back
    );
    stage!(
        "boot-activate-no-build",
        boot.generation == g3 && !boot.fell_back,
        "boot_activate did not bring up the current pre-built generation {g3}"
    );

    // Stage 5: gc — the generation's store path survives, unrooted junk dies.
    let junk = match build_recipe(&junk_recipe()) {
        Ok(b) => b,
        Err(e) => {
            println!("[shade-e2e] junk-build: FAILED — {}", e);
            return 1;
        }
    };
    let db = StoreDb::with_backend(OrosFs, SHADE_ROOT);
    let report = match db.gc(&GcOptions { dry_run: false, force: false }) {
        Ok(r) => r,
        Err(e) => {
            println!("[shade-e2e] gc: FAILED — {:?}", e);
            return 1;
        }
    };
    println!(
        "[shade-e2e] gc: {} kept, {} collected ({} bytes)",
        report.kept,
        report.collected.len(),
        report.freed_bytes
    );
    stage!(
        "gc-keeps-rooted",
        fs.read_file(&join(&b1.out_path, "bin/demo")).as_deref() == Ok(b"hi"),
        "rooted store path was collected"
    );
    stage!(
        "gc-collects-unrooted",
        report.collected.iter().any(|n| n.contains("-junk-0.0"))
            && !fs.exists(&join(&junk.out_path, "bin/junk")),
        "unrooted junk survived gc"
    );

    println!("[shade-e2e] PASS — build, idempotence, generation, activation, rollback, gc");
    0
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo<'_>) -> ! {
    // The panic message names the failing stage — worth the format cost.
    let msg = format!("[shade] panic: {}\n", info);
    lythos_rt::sys_log(&msg);
    lythos_rt::sys_task_exit()
}
