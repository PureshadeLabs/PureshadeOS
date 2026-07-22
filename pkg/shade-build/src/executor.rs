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
//! ## The three seams
//!
//! - [`BuildSandbox`] owns *how* builder commands run. [`PermissiveSandbox`]
//!   is the bringup impl: full host environment, `sh -c`, no isolation
//!   (06 §3's host-assisted approximation, documented as weaker in 08 §5).
//!   Real isolation lands behind this trait without touching the executor.
//! - [`StoreRegistrar`] owns *what happens after* a realization.
//!   [`NoopRegistrar`] is the default; the db-locked registration procedure
//!   (06 §5: reference scan, `/shade/db/refs`, `/shade/db/valid`) replaces it.
//! - [`StoreFs`] (the B1 seam) owns every filesystem operation the executor
//!   does *itself*: scratch setup/teardown (`/shade/build/<drv>`), the build
//!   log (`/shade/log/<drv>.log`), the dep-existence check, and realization.
//!   [`HostFs`](shade_store::HostFs) is the default; on-target wiring injects
//!   [`OrosFs`](shade_store::OrosFs). The scaffolding functions
//!   ([`prepare_scratch`], [`write_build_log`], …) are `no_std` and compile
//!   for the OROS target; the run loop stays `std` until the native
//!   `BuildSandbox` (audit step 3(b)) exists.
//!
//! ## Failure semantics
//!
//! A nonzero phase exit, a missing declared output, or a store-layer
//! mismatch aborts the derivation: the build scratch dir is removed (unless
//! `keep_failed`), the store is left untouched (realization is the only
//! store write and it is atomic), and the error carries the ABI errno
//! ([`BuildError::errno`]). Nothing after the failed derivation builds —
//! its dependents could only rebuild the same missing input.

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use shade_store::backend::{self, join};
use shade_store::{FsError, FsResult, StoreFs};
use shadec::io::EvalIo;

use crate::{plan_graph, BuildError, BuildPlan, Built, PlanGraph, RecipeRef, Resolver};

/// Production roots (docs/shade-pkg/02-store.md §1). [`Executor`] takes them
/// as fields so host tests and bringup tooling can point elsewhere.
pub const CANONICAL_BUILD_ROOT: &str = "/shade/build";
pub const CANONICAL_LOG_ROOT: &str = "/shade/log";

// ---- Seam-routed scaffolding (portable; shared by host and target) ----------

/// The build scratch dir for one derivation: `<build_root>/<store_name>`.
pub fn scratch_dir(build_root: &str, store_name: &str) -> String {
    join(build_root, store_name)
}

/// The build log path for one derivation: `<log_root>/<store_name>.log`.
pub fn build_log_path(log_root: &str, store_name: &str) -> String {
    join(log_root, &format!("{store_name}.log"))
}

/// Create a clean build scratch for `store_name` under `build_root`
/// (06 §2 phase 2): remove any leftover from a previous failed/kept build
/// (it would contaminate this one), then create `<scratch>/tmp` and the
/// `$out` staging dir `<scratch>/out`. Returns `(scratch, staging)`.
pub fn prepare_scratch(
    fs: &mut dyn StoreFs,
    build_root: &str,
    store_name: &str,
) -> FsResult<(String, String)> {
    let scratch = scratch_dir(build_root, store_name);
    backend::remove_tree(fs, &scratch);
    backend::create_dir_all(fs, &join(&scratch, "tmp"))?;
    let staging = join(&scratch, "out");
    backend::create_dir_all(fs, &staging)?;
    Ok((scratch, staging))
}

/// Remove the build scratch (success or failure path, unless `keep_failed`).
/// Best-effort like [`backend::remove_tree`]: on a backend without rmdir
/// (OROS today) the files are unlinked and the empty dir skeleton is left
/// for the store GC.
pub fn clean_scratch(fs: &mut dyn StoreFs, build_root: &str, store_name: &str) {
    backend::remove_tree(fs, &scratch_dir(build_root, store_name));
}

/// Write the build log for `store_name` through the seam (06 §1). Creates
/// `log_root` on demand; replaces an existing log (the seam's `write_file`
/// is exclusive-create on OROS, so a stale log is unlinked first — the
/// executor is the only writer for a given derivation). Returns the log path.
pub fn write_build_log(
    fs: &mut dyn StoreFs,
    log_root: &str,
    store_name: &str,
    bytes: &[u8],
) -> FsResult<String> {
    backend::create_dir_all(fs, log_root)?;
    let path = build_log_path(log_root, store_name);
    match fs.unlink(&path) {
        Ok(()) | Err(FsError::NotFound) => {}
        Err(e) => return Err(e),
    }
    fs.write_file(&path, bytes, false)?;
    Ok(path)
}

// ---- Seam A: the sandbox (portable — nameable without `std`) -----------------
//
// `BuildSandbox` is the how-builds-run seam. It lives here, outside the `std`
// gate, so a native OROS impl can both see it and satisfy it: its signature
// names only portable types — `&str`/`String` seam paths (no `std::path`), the
// [`BuildLogSink`] byte sink, and [`SandboxError`]. The host vehicle
// (`PermissiveSandbox`, the Seatbelt impl, `ChildStream`) stays `std`-gated in
// `mod host` and adapts to this signature at the host boundary.

/// Everything the sandbox needs to prepare one build: identity, directories,
/// and the derivation-declared environment. Paths are seam strings (absolute,
/// `/`-separated) — the same shape as the [`StoreFs`] seam, no `std::path`.
pub struct SandboxSpec<'a> {
    /// `<digest>-<name>-<version>` — the store name being built.
    pub store_name: &'a str,
    /// The build scratch dir `<build_root>/<store_name>` (already created,
    /// with a `tmp/` subdirectory).
    pub scratch: &'a str,
    /// The `$out` staging dir inside the scratch (already created). Phases
    /// write here; the executor realizes this tree into the store on success.
    pub staging: &'a str,
    /// The CDF `system` value (exported as `TARGET`, 06 §4).
    pub system: &'a str,
    /// Recipe env vars, uppercase restored from the CDF's lowercase fold.
    pub env: &'a [(String, String)],
    /// Resolved input store paths (the derivation's `dep.*` set) under the
    /// executor's store root — dep `bin/` dirs join `PATH` in this order.
    pub inputs: &'a [String],
    /// Supervisor-chosen parallelism (`JOBS`, never hashed — 06 §4).
    pub jobs: u32,
}

/// A prepared build environment: where phases run and with what variables.
/// Seam-string paths (no `std::path`) so the type is nameable without `std`.
pub struct BuildEnv {
    /// Working directory for every phase (the scratch dir, 06 §2 phase 3).
    pub cwd: String,
    /// The `$out` staging dir (declared outputs are verified under it).
    pub staging: String,
    /// Variables set for every phase, on top of whatever base environment
    /// the sandbox chooses (permissive: the full host env).
    pub vars: Vec<(String, String)>,
}

/// Where a build phase's child output is written. The executor passes its
/// in-memory build-log buffer; a host sandbox captures the child's
/// stdout+stderr (a shared host fd, so interleaving is preserved) and folds the
/// bytes in here. The native OROS sandbox writes the supervisor log endpoint
/// through the same call. A **byte** sink on purpose — build output is not
/// guaranteed UTF-8, so this is `&[u8]`, not a `core::fmt::Write`.
pub trait BuildLogSink {
    /// Append `bytes` to the log (all of them, or return an error).
    fn write_all(&mut self, bytes: &[u8]) -> Result<(), SandboxError>;
}

/// The executor's default sink: the accumulating build-log buffer.
impl BuildLogSink for Vec<u8> {
    fn write_all(&mut self, bytes: &[u8]) -> Result<(), SandboxError> {
        self.extend_from_slice(bytes);
        Ok(())
    }
}

/// Portable sandbox-seam error: a human-readable message. Deliberately minimal
/// — the executor folds it back into the existing [`BuildError`] variants
/// (`Io` for prepare/spawn, `MissingOutput` for output verification) at the
/// host call site, so no parallel error taxonomy is introduced.
#[derive(Debug, Clone)]
pub struct SandboxError(String);

impl SandboxError {
    /// A sandbox error from any message.
    pub fn new(msg: impl Into<String>) -> Self {
        SandboxError(msg.into())
    }
}

impl core::fmt::Display for SandboxError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Host convenience: any `std::io::Error` folds into a [`SandboxError`] (its
/// `Display`), so host impls keep using `?`. Host-only — the trait itself
/// never names `std::io`.
#[cfg(feature = "std")]
impl From<std::io::Error> for SandboxError {
    fn from(e: std::io::Error) -> Self {
        SandboxError(e.to_string())
    }
}

/// Seam A — how builder commands actually run (06 §3). The executor is
/// mechanism-blind: isolation (capability-restricted builder tasks on OROS,
/// or a host sandbox facility) is a new impl of this trait, not an executor
/// change. Defined outside the `std` gate so the native OROS impl can name and
/// satisfy it.
pub trait BuildSandbox {
    /// Turn a [`SandboxSpec`] into a runnable [`BuildEnv`] (create whatever
    /// the mechanism needs beyond the executor-made scratch/staging dirs).
    fn prepare(&self, spec: &SandboxSpec) -> Result<BuildEnv, SandboxError>;

    /// Run one phase command; stdout and stderr go to `log`. Returns the
    /// exit code (nonzero fails the build at that phase, 06 §2).
    fn spawn(
        &self,
        env: &BuildEnv,
        command: &str,
        log: &mut dyn BuildLogSink,
    ) -> Result<i32, SandboxError>;

    /// Verify the declared outputs (`output.<i>` rel paths like `bin/rkilo`)
    /// exist under the staging tree; return their absolute staged paths.
    /// A missing declaration is an error naming it.
    fn collect_outputs(
        &self,
        env: &BuildEnv,
        declared: &[String],
    ) -> Result<Vec<String>, SandboxError>;
}

// ---- Seam B: the registrar (portable) ----------------------------------------

/// Tag a seam failure with the operation and target path — same shape the
/// store layer uses, so [`BuildError::errno`] folds it for free.
fn seam_err(op: &'static str, path: &str) -> impl FnOnce(FsError) -> BuildError {
    let path = String::from(path);
    move |err| BuildError::Store(shade_store::StoreError::Fs { op, path, err })
}

/// One realization, as reported to the registrar after the store write.
/// Paths are seam strings (no `std::path`).
pub struct Registration<'a> {
    /// The realized output path (under the executor's store root).
    pub out_path: &'a str,
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
/// executor's call site does not change. Errors fold into [`BuildError`]
/// directly (a host impl may use the `std`-only `Register` variant internally —
/// the trait return stays portable).
pub trait StoreRegistrar {
    fn register(&self, reg: &Registration) -> Result<(), BuildError>;
}

/// Default registrar: records nothing. "Exists ⇒ complete" already holds
/// from the store's atomic realize, so bringup runs correctly without db
/// records — GC and reference queries are what the real registrar adds.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopRegistrar;

impl StoreRegistrar for NoopRegistrar {
    fn register(&self, _reg: &Registration) -> Result<(), BuildError> {
        Ok(())
    }
}

// ---- The executor (portable) -------------------------------------------------

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

/// The `shade build` executor — **one** executor, host and target, over
/// injected seams: the [`StoreFs`] backend (`fs`), the [`BuildSandbox`] vehicle
/// (`sandbox`), the [`StoreRegistrar`] (`registrar`), and [`Resolver`]s. It
/// names no host types; the host default backend is supplied by the
/// `#[cfg(feature = "std")]` [`new`](Executor::new) convenience.
pub struct Executor<'a> {
    /// Store root — a seam path (absolute, `/`-separated).
    pub store_root: String,
    /// Build scratch parent: each derivation builds in
    /// `<build_root>/<store_name>/` (06 §2 phase 2).
    pub build_root: String,
    /// Log parent: each build writes `<log_root>/<store_name>.log` (06 §1).
    pub log_root: String,
    /// Keep the scratch dir after the build (success or failure) — the CLI
    /// `--keep-failed` (07 §`shade build`).
    pub keep_failed: bool,
    /// `JOBS` for the sandbox env (not hashed).
    pub jobs: u32,
    /// LOOKUP-THEN-BUILD sources, consulted in order per derivation.
    pub resolvers: &'a [&'a dyn Resolver],
    pub sandbox: &'a dyn BuildSandbox,
    pub registrar: &'a dyn StoreRegistrar,
    /// The B1 filesystem seam every executor-owned fs operation goes through:
    /// scratch, log, dep-existence, realization. Injected explicitly; the host
    /// default (`HostFs`) is set by [`new`](Executor::new).
    ///
    /// Single owner, no interior mutability: the executor *owns* its one
    /// backend and every consumer (scratch/log setup, the dep-existence check,
    /// realization) takes `&mut` from it per operation via the `&mut self` run
    /// methods — the same single-owner + per-op-`&mut` discipline the kernel
    /// VFS adopts for backends (docs/plans/per-task-mount-namespace.md §5.1).
    /// The former `RefCell<Box<…>>` was option (A) at this layer and carried a
    /// runtime-borrow-panic risk; owning the box outright removes it
    /// structurally.
    fs: Box<dyn StoreFs + 'a>,
}

impl<'a> Executor<'a> {
    /// Construct over explicit seam backends — the portable entry, no host
    /// default. `store_root`/`build_root`/`log_root` are seam strings; `fs` is
    /// the injected [`StoreFs`] backend.
    pub fn with_backends(
        store_root: impl Into<String>,
        build_root: impl Into<String>,
        log_root: impl Into<String>,
        resolvers: &'a [&'a dyn Resolver],
        sandbox: &'a dyn BuildSandbox,
        registrar: &'a dyn StoreRegistrar,
        fs: Box<dyn StoreFs + 'a>,
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
            fs,
        }
    }

    /// Host convenience: default [`HostFs`](shade_store::HostFs), preserving the
    /// existing `impl Into<PathBuf>` call shape (same convention as
    /// `read_pointer`/`StoreDb::new`).
    #[cfg(feature = "std")]
    pub fn new(
        store_root: impl Into<std::path::PathBuf>,
        build_root: impl Into<std::path::PathBuf>,
        log_root: impl Into<std::path::PathBuf>,
        resolvers: &'a [&'a dyn Resolver],
        sandbox: &'a dyn BuildSandbox,
        registrar: &'a dyn StoreRegistrar,
    ) -> Self {
        fn s(p: std::path::PathBuf) -> String {
            p.to_str().expect("store paths must be UTF-8").to_string()
        }
        Self::with_backends(
            s(store_root.into()),
            s(build_root.into()),
            s(log_root.into()),
            resolvers,
            sandbox,
            registrar,
            Box::new(shade_store::HostFs),
        )
    }

    /// Swap the filesystem backend. Tests inject the in-memory seam backend
    /// here; on-target wiring injects [`OrosFs`](shade_store::OrosFs).
    pub fn set_fs(&mut self, fs: impl StoreFs + 'a) {
        self.fs = Box::new(fs);
    }

    /// Evaluate `recipe`, order its derivation closure, and satisfy every
    /// derivation (lookup or build). See the module docs for the pipeline.
    pub fn run(
        &mut self,
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
    pub fn run_graph(&mut self, graph: &PlanGraph) -> Result<ExecOutcome, BuildError> {
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
    fn order(&mut self, graph: &PlanGraph) -> Result<Vec<Node>, BuildError> {
        let mut order: Vec<Node> = Vec::new();
        let mut done: BTreeMap<String, ()> = BTreeMap::new();
        // DFS stack: (drv_key, expanded?)
        let mut stack: Vec<(String, bool)> = Vec::new();
        stack.push((graph.root_drv.clone(), false));
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
                    let realized = join(&self.store_root, &store_name);
                    if !self.fs.exists(&realized) {
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
    fn satisfy(&mut self, node: &Node) -> Result<Built, BuildError> {
        for r in self.resolvers {
            if let Some(out_path) = r.resolve(&node.plan)? {
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
    fn build_node(&mut self, node: &Node) -> Result<Built, BuildError> {
        let store_name = &node.plan.paths.store_name;
        // Owned copy: `build_in` below takes `&mut self`, so no borrow of a
        // `self` field (`self.build_root`) may span it.
        let build_root = self.build_root.clone();
        let (scratch, staging) = prepare_scratch(&mut *self.fs, &build_root, store_name)
            .map_err(seam_err("prepare_scratch", &build_root))?;

        let result = self.build_in(node, &scratch, &staging);
        // Success or failure, the scratch (including any partial staged
        // outputs) is removed unless the caller asked to keep it.
        if !self.keep_failed {
            clean_scratch(&mut *self.fs, &build_root, store_name);
        }
        result
    }

    /// Write the accumulated build log through the seam; returns its path.
    fn flush_log(&mut self, store_name: &str, buf: &[u8]) -> Result<String, BuildError> {
        // Owned copy so the `&mut *self.fs` op below borrows only the `fs`
        // field, not `self.log_root` too.
        let log_root = self.log_root.clone();
        write_build_log(&mut *self.fs, &log_root, store_name, buf)
            .map_err(seam_err("write_build_log", &log_root))
    }

    fn build_in(&mut self, node: &Node, scratch: &str, staging: &str) -> Result<Built, BuildError> {
        let store_name = &node.plan.paths.store_name;

        // Inputs: dep store paths under *this* store root. Postorder
        // guarantees they are realized by now.
        let inputs: Vec<String> = node
            .deps
            .iter()
            .map(|d| join(&self.store_root, d.rsplit('/').next().unwrap_or(d)))
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
        let env = self.sandbox.prepare(&spec).map_err(sandbox_build_err)?;

        // The log accumulates here and is written through the seam; the
        // sandbox's `spawn` folds each phase's child output into it via the
        // portable `BuildLogSink` (a host vehicle captures a real fd; see
        // `spawn_capturing`).
        let mut logbuf: Vec<u8> = Vec::new();
        for (i, phase) in node.phases.iter().enumerate() {
            logbuf.extend_from_slice(
                format!("[shade-build] {store_name} phase {i}: {phase}\n").as_bytes(),
            );
            let code = self.sandbox.spawn(&env, phase, &mut logbuf).map_err(sandbox_build_err)?;
            if code != 0 {
                logbuf.extend_from_slice(
                    format!("[shade-build] phase {i} failed with exit code {code}\n").as_bytes(),
                );
                let log = self.flush_log(store_name, &logbuf)?;
                return Err(BuildError::PhaseFailed {
                    drv: node.drv_key.clone(),
                    phase: i,
                    code,
                    log,
                });
            }
        }
        // The log exists for every attempted build, phases or not — and
        // before output verification, so a MissingOutput leaves it behind.
        self.flush_log(store_name, &logbuf)?;

        // Verify every declared output exists before any store write.
        self.sandbox.collect_outputs(&env, &node.outputs).map_err(|e| {
            BuildError::MissingOutput { drv: node.drv_key.clone(), detail: e.to_string() }
        })?;

        let realized = shade_store::realize_cdf(
            &mut *self.fs,
            &self.store_root,
            &node.plan.name,
            &node.plan.version,
            &node.plan.cdf,
            staging,
        )?;

        self.registrar.register(&Registration {
            out_path: &realized.paths.out_path,
            store_name,
            digest: &realized.paths.digest,
            cdf_hash: &shade_cdf::blake3_hex(&node.plan.cdf),
            refs: &node.deps,
        })?;

        Ok(Built::Realized { out_path: realized.paths.out_path })
    }
}

/// Fold a sandbox-seam error (`prepare`/`spawn`) into the portable
/// [`BuildError::Sandbox`] variant — the de-gated executor cannot construct the
/// `std`-only `BuildError::Io` the old fold used.
fn sandbox_build_err(e: SandboxError) -> BuildError {
    BuildError::Sandbox(e.to_string())
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

// ---- The host executor vehicles (std) ----------------------------------------

#[cfg(feature = "std")]
pub use host::*;

#[cfg(feature = "std")]
mod host {
    use std::fs;
    use std::io;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};

    use super::{
        BuildEnv, BuildLogSink, BuildSandbox, Registration, SandboxError, SandboxSpec,
        StoreRegistrar,
    };
    use crate::BuildError;

    // ---- The host sandbox vehicles --------------------------------------------
    //
    // The `BuildSandbox` trait, its portable types, the `Executor`, and the
    // registrar seam all live in the parent module, outside the `std` gate.
    // What stays here is the host machinery that satisfies the seams:
    // `PermissiveSandbox`, `DbRegistrar`, and the child-output capture
    // (`ChildStream` + `spawn_capturing`).

    /// The bringup sandbox: phases run as `sh -c <command>` with the **full host
    /// environment** plus the fixed build vars — no isolation of any kind. This
    /// is host-assisted mode (06 intro, 01 §6.1): the derivation contract
    /// (cwd, `$out`, env vars, exit semantics) is exact; the *enforcement* rows
    /// of 06 §3.1 are simply not enforced. `sandbox=1` in the CDF names the
    /// contract, and 08 §5 records that this impl overstates it.
    #[derive(Debug, Default, Clone, Copy)]
    pub struct PermissiveSandbox;

    impl BuildSandbox for PermissiveSandbox {
        fn prepare(&self, spec: &SandboxSpec) -> Result<BuildEnv, SandboxError> {
            let staging = spec.staging.to_string();
            let mut vars: Vec<(String, String)> = vec![
                // Both spellings: `$OUT` is the 06 §4 variable; recipes' phase
                // strings carry the literal `$out` token (03 §5.2), which the
                // shell resolves through the lowercase variable — the
                // substitution seam without a textual rewrite pass.
                ("OUT".into(), staging.clone()),
                ("out".into(), staging),
                ("TARGET".into(), spec.system.into()),
                ("TMPDIR".into(), Path::new(spec.scratch).join("tmp").to_string_lossy().into_owned()),
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
                .map(|d| Path::new(d).join("bin").to_string_lossy().into_owned())
                .collect();
            if let Ok(host_path) = std::env::var("PATH") {
                path_entries.push(host_path);
            }
            vars.push(("PATH".into(), path_entries.join(":")));
            vars.extend(spec.env.iter().cloned());
            Ok(BuildEnv {
                cwd: spec.scratch.to_string(),
                staging: spec.staging.to_string(),
                vars,
            })
        }

        fn spawn(
            &self,
            env: &BuildEnv,
            command: &str,
            log: &mut dyn BuildLogSink,
        ) -> Result<i32, SandboxError> {
            let mut cmd = Command::new("sh");
            cmd.arg("-c")
                .arg(command)
                .current_dir(&env.cwd)
                .envs(env.vars.iter().map(|(k, v)| (k.as_str(), v.as_str())));
            spawn_capturing(cmd, log)
        }

        fn collect_outputs(
            &self,
            env: &BuildEnv,
            declared: &[String],
        ) -> Result<Vec<String>, SandboxError> {
            let mut out = Vec::with_capacity(declared.len());
            for rel in declared {
                let p = Path::new(&env.staging).join(rel);
                if !p.exists() {
                    return Err(SandboxError::new(format!(
                        "declared output `{rel}` was not produced by the build"
                    )));
                }
                out.push(p.to_string_lossy().into_owned());
            }
            Ok(out)
        }
    }

    /// The real registrar (06 §5): records each realization in the store database
    /// under `/shade/db/` — reference-scans the output, unions the declared
    /// `dep.*`, and writes `db/refs/<digest>` + `db/valid/<digest>`. This is what
    /// makes `shade gc` possible; the executor call site is unchanged.
    pub struct DbRegistrar {
        db: shade_store_db::StoreDb<shade_store::HostFs>,
    }

    impl DbRegistrar {
        /// A registrar whose db/roots/log roots are the siblings of `store_root`
        /// (canonical `/shade` layout, 02 §1).
        pub fn for_store_root(store_root: &Path) -> Self {
            DbRegistrar { db: shade_store_db::StoreDb::for_store_root(store_root) }
        }

        /// A registrar over an explicit [`StoreDb`](shade_store_db::StoreDb).
        pub fn new(db: shade_store_db::StoreDb<shade_store::HostFs>) -> Self {
            DbRegistrar { db }
        }
    }

    impl StoreRegistrar for DbRegistrar {
        fn register(&self, reg: &Registration) -> Result<(), BuildError> {
            // `out_path` is already a seam string; the db layer takes `&str`.
            self.db
                .register(reg.out_path, reg.digest, reg.store_name, reg.cdf_hash, reg.refs)
                .map(|_| ())
                .map_err(|e| BuildError::Register(io::Error::from(e)))
        }
    }

    /// Host vehicle for streaming one phase's child output: the sandbox seam
    /// hands the builder a real fd (`spawn` takes `&fs::File` — child stdio
    /// cannot point at an in-memory sink). The stream lands in the host temp
    /// dir and is folded into the seam-written build log after the phase; the
    /// log's durable home is always behind the [`StoreFs`] seam. The native
    /// OROS sandbox replaces this with the supervisor log endpoint
    /// (`SandboxPlan`'s Ipc grant), not with a host file.
    struct ChildStream {
        path: PathBuf,
        file: fs::File,
    }

    impl ChildStream {
        fn create() -> io::Result<ChildStream> {
            use core::sync::atomic::{AtomicU64, Ordering};
            static SEQ: AtomicU64 = AtomicU64::new(0);
            // pid + monotonic seq is the uniqueness; the file is transient and
            // removed as soon as its bytes are folded into the log sink.
            let path = std::env::temp_dir().join(format!(
                ".shade-build-phase-{}-{}",
                std::process::id(),
                SEQ.fetch_add(1, Ordering::Relaxed),
            ));
            let file = fs::File::create(&path)?;
            Ok(ChildStream { path, file })
        }

        fn file(&self) -> &fs::File {
            &self.file
        }

        /// The bytes the phase wrote; the transient host file is removed.
        fn into_bytes(self) -> io::Result<Vec<u8>> {
            let ChildStream { path, file } = self;
            drop(file);
            let bytes = fs::read(&path)?;
            let _ = fs::remove_file(&path);
            Ok(bytes)
        }
    }

    /// The host vehicle behind [`BuildLogSink`]: run `cmd` with stdin closed and
    /// stdout+stderr wired to one shared temp-file fd (so their interleaving is
    /// preserved exactly), then fold the captured bytes into `log` and return
    /// the exit code. A child process needs a real fd, so the bytes detour
    /// through a host temp file before reaching the (possibly in-memory) sink.
    /// Both host sandboxes (`PermissiveSandbox`, the Seatbelt impl) build their
    /// `Command` and hand it here.
    pub(crate) fn spawn_capturing(
        mut cmd: Command,
        log: &mut dyn BuildLogSink,
    ) -> Result<i32, SandboxError> {
        let stream = ChildStream::create()?;
        let status = cmd
            .stdin(Stdio::null())
            .stdout(Stdio::from(stream.file().try_clone()?))
            .stderr(Stdio::from(stream.file().try_clone()?))
            .status()?;
        log.write_all(&stream.into_bytes()?)?;
        // A signal-killed builder has no exit code; report it as failed.
        Ok(status.code().unwrap_or(-1))
    }

}
