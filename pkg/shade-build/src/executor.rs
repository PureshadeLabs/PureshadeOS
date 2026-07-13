//! The build executor: CDF → run → realize (docs/shade/build-executor.md,
//! docs/shade-pkg/06-build.md).
//!
//! [`Executor::run`] drives a whole derivation closure: evaluate the recipe,
//! topologically order the emitted derivations over their `dep.*` refs, and
//! for each one LOOKUP-THEN-BUILD — a resolver hit skips the build entirely;
//! a miss runs the derivation's phases through the [`BuildSandbox`] seam,
//! verifies the declared outputs, realizes them input-addressed into the
//! store ([`shade_store::realize_cdf`]), and reports the path to the
//! [`StoreRegistrar`] seam.
//!
//! ## The two seams
//!
//! - [`BuildSandbox`] owns *how* builder commands run. [`PermissiveSandbox`]
//!   is the bringup impl: full host environment, `sh -c`, no isolation
//!   (06 §3's host-assisted approximation, documented as weaker in 08 §5).
//!   Real isolation lands behind this trait without touching the executor.
//! - [`StoreRegistrar`] owns *what happens after* a realization.
//!   [`NoopRegistrar`] is the default; the db-locked registration procedure
//!   (06 §5: reference scan, `/shade/db/refs`, `/shade/db/valid`) replaces it.
//!
//! ## Failure semantics
//!
//! A nonzero phase exit, a missing declared output, or a store-layer
//! mismatch aborts the derivation: the build scratch dir is removed (unless
//! `keep_failed`), the store is left untouched (realization is the only
//! store write and it is atomic), and the error carries the ABI errno
//! ([`BuildError::errno`]). Nothing after the failed derivation builds —
//! its dependents could only rebuild the same missing input.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use shadec::io::EvalIo;

use crate::{plan_graph, BuildError, BuildPlan, Built, RecipeRef, Resolver};

/// Production roots (docs/shade-pkg/02-store.md §1). [`Executor`] takes them
/// as fields so host tests and bringup tooling can point elsewhere.
pub const CANONICAL_BUILD_ROOT: &str = "/shade/build";
pub const CANONICAL_LOG_ROOT: &str = "/shade/log";

// ---- Seam A: the sandbox ----------------------------------------------------

/// Everything the sandbox needs to prepare one build: identity, directories,
/// and the derivation-declared environment. The executor computes this from
/// the parsed CDF; the sandbox turns it into a [`BuildEnv`].
pub struct SandboxSpec<'a> {
    /// `<digest>-<name>-<version>` — the store name being built.
    pub store_name: &'a str,
    /// The build scratch dir `<build_root>/<store_name>` (already created,
    /// with a `tmp/` subdirectory).
    pub scratch: &'a Path,
    /// The `$out` staging dir inside the scratch (already created). Phases
    /// write here; the executor realizes this tree into the store on success.
    pub staging: &'a Path,
    /// The CDF `system` value (exported as `TARGET`, 06 §4).
    pub system: &'a str,
    /// Recipe env vars, uppercase restored from the CDF's lowercase fold.
    pub env: &'a [(String, String)],
    /// Resolved input store paths (the derivation's `dep.*` set) under the
    /// executor's store root — dep `bin/` dirs join `PATH` in this order.
    pub inputs: &'a [PathBuf],
    /// Supervisor-chosen parallelism (`JOBS`, never hashed — 06 §4).
    pub jobs: u32,
}

/// A prepared build environment: where phases run and with what variables.
pub struct BuildEnv {
    /// Working directory for every phase (the scratch dir, 06 §2 phase 3).
    pub cwd: PathBuf,
    /// The `$out` staging dir (declared outputs are verified under it).
    pub staging: PathBuf,
    /// Variables set for every phase, on top of whatever base environment
    /// the sandbox chooses (permissive: the full host env).
    pub vars: Vec<(String, String)>,
}

/// Seam A — how builder commands actually run (06 §3). The executor is
/// mechanism-blind: isolation (capability-restricted builder tasks on OROS,
/// or a host sandbox facility) is a new impl of this trait, not an executor
/// change.
pub trait BuildSandbox {
    /// Turn a [`SandboxSpec`] into a runnable [`BuildEnv`] (create whatever
    /// the mechanism needs beyond the executor-made scratch/staging dirs).
    fn prepare(&self, spec: &SandboxSpec) -> io::Result<BuildEnv>;

    /// Run one phase command; stdout and stderr go to `log`. Returns the
    /// exit code (nonzero fails the build at that phase, 06 §2).
    fn spawn(&self, env: &BuildEnv, command: &str, log: &fs::File) -> io::Result<i32>;

    /// Verify the declared outputs (`output.<i>` rel paths like `bin/rkilo`)
    /// exist under the staging tree; return their absolute staged paths.
    /// A missing declaration is an error naming it.
    fn collect_outputs(&self, env: &BuildEnv, declared: &[String]) -> io::Result<Vec<PathBuf>>;
}

/// The bringup sandbox: phases run as `sh -c <command>` with the **full host
/// environment** plus the fixed build vars — no isolation of any kind. This
/// is host-assisted mode (06 intro, 01 §6.1): the derivation contract
/// (cwd, `$out`, env vars, exit semantics) is exact; the *enforcement* rows
/// of 06 §3.1 are simply not enforced. `sandbox=1` in the CDF names the
/// contract, and 08 §5 records that this impl overstates it.
#[derive(Debug, Default, Clone, Copy)]
pub struct PermissiveSandbox;

impl BuildSandbox for PermissiveSandbox {
    fn prepare(&self, spec: &SandboxSpec) -> io::Result<BuildEnv> {
        let staging = spec.staging.to_string_lossy().into_owned();
        let mut vars: Vec<(String, String)> = vec![
            // Both spellings: `$OUT` is the 06 §4 variable; recipes' phase
            // strings carry the literal `$out` token (03 §5.2), which the
            // shell resolves through the lowercase variable — the
            // substitution seam without a textual rewrite pass.
            ("OUT".into(), staging.clone()),
            ("out".into(), staging),
            ("TARGET".into(), spec.system.into()),
            ("TMPDIR".into(), spec.scratch.join("tmp").to_string_lossy().into_owned()),
            ("SOURCE_DATE_EPOCH".into(), "0".into()),
            ("TZ".into(), "UTC".into()),
            ("LANG".into(), "C.UTF-8".into()),
            ("LC_ALL".into(), "C.UTF-8".into()),
            ("JOBS".into(), spec.jobs.to_string()),
        ];
        // Input bin/ dirs head PATH in dep order (06 §4); permissive mode
        // keeps the host PATH behind them so `sh`/coreutils keep working.
        let mut path_entries: Vec<String> = spec
            .inputs
            .iter()
            .map(|d| d.join("bin").to_string_lossy().into_owned())
            .collect();
        if let Ok(host_path) = std::env::var("PATH") {
            path_entries.push(host_path);
        }
        vars.push(("PATH".into(), path_entries.join(":")));
        vars.extend(spec.env.iter().cloned());
        Ok(BuildEnv { cwd: spec.scratch.to_path_buf(), staging: spec.staging.to_path_buf(), vars })
    }

    fn spawn(&self, env: &BuildEnv, command: &str, log: &fs::File) -> io::Result<i32> {
        let status = Command::new("sh")
            .arg("-c")
            .arg(command)
            .current_dir(&env.cwd)
            .envs(env.vars.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .stdin(Stdio::null())
            .stdout(Stdio::from(log.try_clone()?))
            .stderr(Stdio::from(log.try_clone()?))
            .status()?;
        // A signal-killed builder has no exit code; report it as failed.
        Ok(status.code().unwrap_or(-1))
    }

    fn collect_outputs(&self, env: &BuildEnv, declared: &[String]) -> io::Result<Vec<PathBuf>> {
        let mut out = Vec::with_capacity(declared.len());
        for rel in declared {
            let p = env.staging.join(rel);
            if !p.exists() {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("declared output `{rel}` was not produced by the build"),
                ));
            }
            out.push(p);
        }
        Ok(out)
    }
}

// ---- Seam B: the registrar --------------------------------------------------

/// One realization, as reported to the registrar after the store write.
pub struct Registration<'a> {
    /// The realized output path (under the executor's store root).
    pub out_path: &'a Path,
    /// `<digest>-<name>-<version>`.
    pub store_name: &'a str,
    /// The 32-char store digest.
    pub digest: &'a str,
    /// Full BLAKE3-256 of the CDF bytes, lowercase hex.
    pub cdf_hash: &'a str,
    /// The derivation's input refs: its `dep.*` store paths (canonical form,
    /// exactly as hashed). Seed of the 06 §5 reference-scan record.
    pub refs: &'a [String],
}

/// Seam B — what happens after each realization. The real registration
/// procedure (06 §5: verify, reference-scan, `/shade/db/refs` +
/// `/shade/db/valid` under the db lock) replaces the default impl; the
/// executor's call site does not change.
pub trait StoreRegistrar {
    fn register(&self, reg: &Registration) -> io::Result<()>;
}

/// Default registrar: records nothing. "Exists ⇒ complete" already holds
/// from the store's atomic realize, so bringup runs correctly without db
/// records — GC and reference queries are what the real registrar adds.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopRegistrar;

impl StoreRegistrar for NoopRegistrar {
    fn register(&self, _reg: &Registration) -> io::Result<()> {
        Ok(())
    }
}

/// The real registrar (06 §5): records each realization in the store database
/// under `/shade/db/` — reference-scans the output, unions the declared
/// `dep.*`, and writes `db/refs/<digest>` + `db/valid/<digest>`. This is what
/// makes `shade gc` possible; the executor call site is unchanged.
pub struct DbRegistrar {
    db: shade_store_db::StoreDb,
}

impl DbRegistrar {
    /// A registrar whose db/roots/log roots are the siblings of `store_root`
    /// (canonical `/shade` layout, 02 §1).
    pub fn for_store_root(store_root: &Path) -> Self {
        DbRegistrar { db: shade_store_db::StoreDb::for_store_root(store_root) }
    }

    /// A registrar over an explicit [`StoreDb`](shade_store_db::StoreDb).
    pub fn new(db: shade_store_db::StoreDb) -> Self {
        DbRegistrar { db }
    }
}

impl StoreRegistrar for DbRegistrar {
    fn register(&self, reg: &Registration) -> io::Result<()> {
        self.db
            .register(reg.out_path, reg.digest, reg.store_name, reg.cdf_hash, reg.refs)
            .map(|_| ())
    }
}

// ---- The executor -----------------------------------------------------------

/// One parsed derivation in the build graph.
struct Node {
    /// Canonical drvPath — the closure map key.
    drv_key: String,
    plan: BuildPlan,
    /// Canonical `dep.*` out paths (refs), in CDF order.
    deps: Vec<String>,
    phases: Vec<String>,
    /// Declared outputs (`output.<i>` values, e.g. `bin/rkilo`).
    outputs: Vec<String>,
    /// Recipe env, uppercase restored.
    env: Vec<(String, String)>,
    system: String,
    /// A source derivation (`builder=fetch`): satisfiable by lookup only —
    /// the fetcher (06 §2 phase 1) is not implemented yet.
    is_fetch: bool,
}

/// The result of one [`Executor::run`]: per-derivation results in build
/// order (dependencies first, the root last).
#[derive(Debug, Clone)]
pub struct ExecOutcome {
    /// The root derivation's plan (identity, CDF, paths).
    pub root: BuildPlan,
    /// `(store_name, result)` for every derivation in the closure walk.
    pub results: Vec<(String, Built)>,
}

impl ExecOutcome {
    /// The root derivation's result (always the last entry).
    pub fn root_result(&self) -> &Built {
        &self.results.last().expect("closure walk always includes the root").1
    }
}

/// The `shade build` executor. Construct one per store/scratch/log root
/// configuration and [`run`](Executor::run) recipes through it.
pub struct Executor<'a> {
    pub store_root: PathBuf,
    /// Build scratch parent: each derivation builds in
    /// `<build_root>/<store_name>/` (06 §2 phase 2).
    pub build_root: PathBuf,
    /// Log parent: each build writes `<log_root>/<store_name>.log` (06 §1).
    pub log_root: PathBuf,
    /// Keep the scratch dir after the build (success or failure) — the CLI
    /// `--keep-failed` (07 §`shade build`).
    pub keep_failed: bool,
    /// `JOBS` for the sandbox env (not hashed).
    pub jobs: u32,
    /// LOOKUP-THEN-BUILD sources, consulted in order per derivation.
    pub resolvers: &'a [&'a dyn Resolver],
    pub sandbox: &'a dyn BuildSandbox,
    pub registrar: &'a dyn StoreRegistrar,
}

impl<'a> Executor<'a> {
    /// A permissive-sandbox, no-op-registrar executor over `resolvers` with
    /// the production scratch/log roots relative to nothing — callers set
    /// the roots explicitly; this just bundles the defaults for the seams.
    pub fn new(
        store_root: impl Into<PathBuf>,
        build_root: impl Into<PathBuf>,
        log_root: impl Into<PathBuf>,
        resolvers: &'a [&'a dyn Resolver],
        sandbox: &'a dyn BuildSandbox,
        registrar: &'a dyn StoreRegistrar,
    ) -> Self {
        Executor {
            store_root: store_root.into(),
            build_root: build_root.into(),
            log_root: log_root.into(),
            keep_failed: false,
            jobs: 1,
            resolvers,
            sandbox,
            registrar,
        }
    }

    /// Evaluate `recipe`, order its derivation closure, and satisfy every
    /// derivation (lookup or build). See the module docs for the pipeline.
    pub fn run(
        &self,
        recipe: &RecipeRef,
        toolchain: Option<&str>,
        io: &dyn EvalIo,
    ) -> Result<ExecOutcome, BuildError> {
        let graph = plan_graph(recipe, &self.store_root, toolchain, io)?;
        self.run_graph(&graph)
    }

    /// [`run`](Executor::run) from an already-planned graph — the entry a
    /// package-set driver uses after [`crate::plan_value`], one call per
    /// selected package over a single shared evaluation.
    pub fn run_graph(&self, graph: &crate::PlanGraph) -> Result<ExecOutcome, BuildError> {
        let order = self.order(graph)?;

        let mut results = Vec::with_capacity(order.len());
        for node in &order {
            let built = self.satisfy(node)?;
            results.push((node.plan.paths.store_name.clone(), built));
        }
        Ok(ExecOutcome { root: graph.root.clone(), results })
    }

    /// Topologically order the closure reachable from the root: postorder
    /// over `dep.*` edges, dependencies before dependents. Input-addressing
    /// makes true cycles unconstructible (a dep's path is a function of its
    /// own hash), so a cycle here means corrupt input and is an error.
    fn order(&self, graph: &crate::PlanGraph) -> Result<Vec<Node>, BuildError> {
        let mut order: Vec<Node> = Vec::new();
        let mut done: BTreeMap<String, ()> = BTreeMap::new();
        // DFS stack: (drv_key, expanded?)
        let mut stack: Vec<(String, bool)> = vec![(graph.root_drv.clone(), false)];
        let mut in_progress: BTreeMap<String, ()> = BTreeMap::new();

        while let Some((key, expanded)) = stack.pop() {
            if done.contains_key(&key) {
                continue;
            }
            if expanded {
                in_progress.remove(&key);
                done.insert(key.clone(), ());
                order.push(self.parse_node(&key, &graph.closure[&key])?);
                continue;
            }
            if in_progress.insert(key.clone(), ()).is_some() {
                return Err(BuildError::Cycle { drv: key });
            }
            stack.push((key.clone(), true));
            let deps = dep_refs(&shade_cdf::parse(&graph.closure[&key]).map_err(|e| {
                BuildError::CdfParse { drv: key.clone(), error: e }
            })?);
            for dep_out in deps {
                let dep_key = format!("{dep_out}.drv");
                if graph.closure.contains_key(&dep_key) {
                    stack.push((dep_key, false));
                } else {
                    // The evaluator emits every derivation whose outPath it
                    // hands out, so an unknown ref means the CDF names a
                    // store path with no producing derivation in this eval.
                    // Tolerable only if it is already realized.
                    let store_name = dep_out
                        .rsplit('/')
                        .next()
                        .unwrap_or(&dep_out)
                        .to_string();
                    if !self.store_root.join(&store_name).exists() {
                        return Err(BuildError::UnknownDep { drv: key.clone(), dep: dep_out });
                    }
                }
            }
        }
        Ok(order)
    }

    /// Parse one closure entry into a build [`Node`].
    fn parse_node(&self, drv_key: &str, cdf: &[u8]) -> Result<Node, BuildError> {
        let entries = shade_cdf::parse(cdf)
            .map_err(|e| BuildError::CdfParse { drv: drv_key.to_string(), error: e })?;
        let required = |k: &str| {
            entries.get(k).cloned().ok_or_else(|| BuildError::BadDrv {
                drv: drv_key.to_string(),
                detail: format!("missing required CDF key `{k}`"),
            })
        };
        let name = required("name")?;
        let version = required("version")?;
        let paths = shade_store::store_paths_at(&self.store_root, &name, &version, cdf)?;
        let plan = BuildPlan { name, version, cdf: cdf.to_vec(), paths };
        Ok(Node {
            drv_key: drv_key.to_string(),
            deps: dep_refs(&entries),
            phases: indexed(&entries, "phase."),
            outputs: indexed(&entries, "output."),
            env: entries
                .iter()
                .filter_map(|(k, v)| {
                    k.strip_prefix("env.")
                        // uppercase restore: env keys are validated
                        // [A-Z_][A-Z0-9_]* at emission and stored as their
                        // lowercase fold (02 §3.3) — the fold is invertible.
                        .map(|name| (name.to_ascii_uppercase(), v.clone()))
                })
                .collect(),
            system: entries.get("system").cloned().unwrap_or_default(),
            is_fetch: entries.get("builder").map(String::as_str) == Some("fetch"),
            plan,
        })
    }

    /// LOOKUP-THEN-BUILD for one derivation.
    fn satisfy(&self, node: &Node) -> Result<Built, BuildError> {
        for r in self.resolvers {
            if let Some(out_path) = r.resolve(&node.plan).map_err(BuildError::Io)? {
                return Ok(Built::Resolved { source: r.source().to_string(), out_path });
            }
        }
        if node.is_fetch {
            // Source derivations realize by fetching (06 §2 phase 1), which
            // is not implemented — a miss cannot be built.
            return Err(BuildError::FetchUnrealized { drv: node.drv_key.clone() });
        }
        self.build_node(node)
    }

    /// Run one derivation's phases in the sandbox and realize the staged
    /// output. Any failure cleans the scratch (unless `keep_failed`) and
    /// leaves the store untouched — realization is the only store write.
    fn build_node(&self, node: &Node) -> Result<Built, BuildError> {
        let store_name = &node.plan.paths.store_name;
        let scratch = self.build_root.join(store_name);
        // A leftover from a previous failed/kept build would contaminate
        // this one; start clean.
        let _ = fs::remove_dir_all(&scratch);
        fs::create_dir_all(scratch.join("tmp")).map_err(BuildError::Io)?;
        let staging = scratch.join("out");
        fs::create_dir_all(&staging).map_err(BuildError::Io)?;

        let result = self.build_in(node, &scratch, &staging);
        // Success or failure, the scratch (including any partial staged
        // outputs) is removed unless the caller asked to keep it.
        if !self.keep_failed {
            let _ = fs::remove_dir_all(&scratch);
        }
        result
    }

    fn build_in(&self, node: &Node, scratch: &Path, staging: &Path) -> Result<Built, BuildError> {
        let store_name = &node.plan.paths.store_name;

        // Inputs: dep store paths under *this* store root. Postorder
        // guarantees they are realized by now.
        let inputs: Vec<PathBuf> = node
            .deps
            .iter()
            .map(|d| self.store_root.join(d.rsplit('/').next().unwrap_or(d)))
            .collect();

        let spec = SandboxSpec {
            store_name,
            scratch,
            staging,
            system: &node.system,
            env: &node.env,
            inputs: &inputs,
            jobs: self.jobs,
        };
        let env = self.sandbox.prepare(&spec).map_err(BuildError::Io)?;

        fs::create_dir_all(&self.log_root).map_err(BuildError::Io)?;
        let log_path = self.log_root.join(format!("{store_name}.log"));
        let mut log = fs::File::create(&log_path).map_err(BuildError::Io)?;

        for (i, phase) in node.phases.iter().enumerate() {
            writeln!(log, "[shade-build] {store_name} phase {i}: {phase}")
                .map_err(BuildError::Io)?;
            let code = self.sandbox.spawn(&env, phase, &log).map_err(BuildError::Io)?;
            if code != 0 {
                writeln!(log, "[shade-build] phase {i} failed with exit code {code}")
                    .map_err(BuildError::Io)?;
                return Err(BuildError::PhaseFailed {
                    drv: node.drv_key.clone(),
                    phase: i,
                    code,
                    log: log_path,
                });
            }
        }

        // Verify every declared output exists before any store write.
        self.sandbox.collect_outputs(&env, &node.outputs).map_err(|e| {
            BuildError::MissingOutput { drv: node.drv_key.clone(), detail: e.to_string() }
        })?;

        let realized = shade_store::realize_cdf(
            &self.store_root,
            &node.plan.name,
            &node.plan.version,
            &node.plan.cdf,
            staging,
        )?;

        self.registrar
            .register(&Registration {
                out_path: &realized.paths.out_path,
                store_name,
                digest: &realized.paths.digest,
                cdf_hash: &shade_cdf::blake3_hex(&node.plan.cdf),
                refs: &node.deps,
            })
            .map_err(BuildError::Register)?;

        Ok(Built::Realized { out_path: realized.paths.out_path })
    }
}

/// `dep.<i>` values in numeric index order (refs).
fn dep_refs(entries: &BTreeMap<String, String>) -> Vec<String> {
    indexed(entries, "dep.")
}

/// Collect `<prefix><i>` values sorted by the numeric index — bytewise key
/// order would put `phase.10` before `phase.2`.
fn indexed(entries: &BTreeMap<String, String>, prefix: &str) -> Vec<String> {
    let mut v: Vec<(usize, &String)> = entries
        .iter()
        .filter_map(|(k, val)| {
            k.strip_prefix(prefix)
                .and_then(|i| i.parse::<usize>().ok())
                .map(|i| (i, val))
        })
        .collect();
    v.sort_unstable_by_key(|(i, _)| *i);
    v.into_iter().map(|(_, s)| s.clone()).collect()
}
