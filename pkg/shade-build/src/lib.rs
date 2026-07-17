//! shade-build — the `shade build` core (docs/shade-pkg/07-cli.md §`shade
//! build`, 09-bootstrap.md §2 pipeline `recipe → shadec eval → CDF → shade
//! build → store path`).
//!
//! One function, [`build`], drives the pipeline:
//!
//! 1. **eval** the recipe with the shade evaluator ([`shadec`]) → a derivation
//!    value carrying canonical CDF bytes;
//! 2. **digest** those bytes and construct the target store paths under a
//!    `store_root` ([`shade_store::store_paths_at`]) — input-addressed, so the
//!    path is a pure function of the resolved inputs;
//! 3. **resolve**: consult [`Resolver`]s in order (LOOKUP-THEN-BUILD, never
//!    build-first). The local store is the only resolver source today; a
//!    remote substituter (shade-pkg 08 §6, signed) is a *new* [`Resolver`]
//!    impl, not a refactor;
//! 4. **realize**: on a resolver miss, run the [`Builder`] to stage the output
//!    tree, then [`shade_store::realize_cdf`] installs it atomically and
//!    idempotently. On a hit, nothing builds.
//!
//! ## Feature split / what is deferred
//!
//! The plan/address half (eval → CDF → store paths) and the executor's
//! filesystem scaffolding are `no_std + alloc` over the B1 [`StoreFs`] seam
//! and compile for the OROS target (feature `oros`). The executor **run
//! loop** and both sandboxes stay behind `std`: [`BuildSandbox`]'s host
//! vehicles spawn real processes, and the native OROS builder task (lowering
//! `SandboxPlan` to `SYS_MOUNT` + capability grants — audit step 3(b)) is
//! deferred. Likewise the OROS `shade` binary (`pkg/shade`) stays a stub
//! until an OROS `EvalIo` exists.

#![cfg_attr(not(any(test, feature = "std")), no_std)]

extern crate alloc;

use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;

#[cfg(feature = "std")]
use std::io;
#[cfg(feature = "std")]
use std::path::{Path, PathBuf};

use shadec::error::{EvalError, Pos};
use shadec::eval::Evaluator;
use shadec::io::EvalIo;
use shadec::value::Value;

pub use shade_store::{FsError, StoreFs, StorePaths};

/// shade-store's seam API is `&str`-pathed (`no_std` — no `std::path`); this
/// crate's host-facing API stays `Path`-based and converts at the boundary.
#[cfg(feature = "std")]
pub(crate) fn path_str(p: &Path) -> &str {
    p.to_str().expect("store paths must be UTF-8")
}

mod executor;
pub use executor::{
    build_log_path, clean_scratch, prepare_scratch, scratch_dir, write_build_log,
    CANONICAL_BUILD_ROOT, CANONICAL_LOG_ROOT,
};
#[cfg(feature = "std")]
pub use executor::{
    BuildEnv, BuildSandbox, DbRegistrar, ExecOutcome, Executor, NoopRegistrar, PermissiveSandbox,
    Registration, SandboxSpec, StoreRegistrar,
};

#[cfg(feature = "std")]
mod sandbox;
#[cfg(feature = "std")]
pub use sandbox::{
    CapGrant, LythosSandbox, MountPlan, SandboxPlan, BUILD_GID, BUILD_UID, BUILD_UMASK,
    SANDBOX_HOME,
};

/// What to build: a recipe file or an inline expression. Argv parsing (the
/// CLI) turns its positional argument into one of these; it is stubbed for
/// OROS (no OROS `EvalIo` yet) and thin on the host.
#[derive(Debug, Clone)]
pub enum RecipeRef {
    /// A path to a `.shade` file (or a directory containing `default.shade`).
    File(String),
    /// An inline Shade expression, evaluated with `base_dir` as its base
    /// directory (for relative path literals).
    Expr { src: String, base_dir: String },
}

/// A resolved, addressed build plan: identity, canonical CDF, target paths.
/// Same recipe + same resolved inputs ⇒ same `cdf` ⇒ same `paths` (the
/// input-addressing invariant the whole pipeline rests on).
#[derive(Debug, Clone)]
pub struct BuildPlan {
    pub name: String,
    pub version: String,
    /// The canonical CDF bytes — exactly what becomes the `.drv`.
    pub cdf: Vec<u8>,
    /// Digest + output/`.drv` paths under the chosen `store_root`.
    pub paths: StorePaths,
}

/// A source that can satisfy a store path **without a local build**
/// (LOOKUP-THEN-BUILD). The local store is the only implementation today
/// ([`LocalStore`]); a remote substituter is a new impl of this trait, not a
/// change to [`build`].
///
/// [`resolve`](Resolver::resolve) returns `Ok(Some(out_path))` if the source
/// made the output available in the local store (for the local store, it was
/// already there; a substituter would fetch-and-install here), or `Ok(None)`
/// if this source cannot satisfy the plan.
#[cfg(feature = "std")]
pub trait Resolver {
    /// A short source name for diagnostics / the build outcome.
    fn source(&self) -> &str;
    fn resolve(&self, plan: &BuildPlan) -> io::Result<Option<PathBuf>>;
}

/// The local store resolver: a hit iff `<out_path>` already exists. Stateless
/// — the store root is encoded in `plan.paths.out_path`. This is the only
/// resolver source wired today (the base of the LOOKUP-THEN-BUILD stack).
#[cfg(feature = "std")]
#[derive(Debug, Default, Clone, Copy)]
pub struct LocalStore;

#[cfg(feature = "std")]
impl Resolver for LocalStore {
    fn source(&self) -> &str {
        "local"
    }
    fn resolve(&self, plan: &BuildPlan) -> io::Result<Option<PathBuf>> {
        // Immutable store: "exists ⇒ complete" (shade-store realize contract),
        // so existence alone is a hit.
        Ok(Path::new(&plan.paths.out_path)
            .exists()
            .then(|| PathBuf::from(&plan.paths.out_path)))
    }
}

/// A staged output tree ready to be realized into the store, plus an optional
/// scratch directory removed on drop. The [`Builder`] owns temp-dir policy and
/// hands one of these back; [`build`] realizes `root`, then drops the guard.
#[cfg(feature = "std")]
pub struct StagedOutput {
    /// The staged tree to install as `<out_path>`.
    pub root: PathBuf,
    /// A scratch dir to remove on drop (e.g. the build working area). `None`
    /// leaves nothing to clean up.
    pub cleanup: Option<PathBuf>,
}

#[cfg(feature = "std")]
impl StagedOutput {
    /// A staged tree at `root` with no separate scratch to clean up.
    pub fn at(root: impl Into<PathBuf>) -> Self {
        StagedOutput { root: root.into(), cleanup: None }
    }
    /// A staged tree at `root` whose enclosing scratch dir `cleanup` is removed
    /// on drop.
    pub fn with_cleanup(root: impl Into<PathBuf>, cleanup: impl Into<PathBuf>) -> Self {
        StagedOutput { root: root.into(), cleanup: Some(cleanup.into()) }
    }
}

#[cfg(feature = "std")]
impl Drop for StagedOutput {
    fn drop(&mut self) {
        if let Some(dir) = &self.cleanup {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

/// Runs a derivation's build (shade-pkg 06 phase 3) to produce a staged output
/// tree. **Track 2** — the real sandboxed builder is deferred (it runs on
/// OROS). [`build`] only invokes this on a resolver miss; the returned tree is
/// handed to [`shade_store::realize_cdf`]. Host builders and test doubles
/// implement the trait.
#[cfg(feature = "std")]
pub trait Builder {
    fn build(&self, plan: &BuildPlan) -> io::Result<StagedOutput>;
}

/// How the plan was satisfied.
#[cfg(feature = "std")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Built {
    /// Satisfied by a [`Resolver`]; no build ran. `source` is the resolver
    /// that provided it (`"local"` for a store hit).
    Resolved { source: String, out_path: PathBuf },
    /// Built locally and realized into the store.
    Realized { out_path: PathBuf },
}

#[cfg(feature = "std")]
impl Built {
    /// The output store path either way.
    pub fn out_path(&self) -> &Path {
        match self {
            Built::Resolved { out_path, .. } | Built::Realized { out_path } => out_path,
        }
    }
}

/// The result of [`build`]: the resolved plan and how it was satisfied.
#[cfg(feature = "std")]
#[derive(Debug, Clone)]
pub struct Outcome {
    pub plan: BuildPlan,
    pub result: Built,
}

#[derive(Debug)]
pub enum BuildError {
    /// Evaluating the recipe failed.
    Eval(EvalError),
    /// The expression did not evaluate to a derivation.
    NotADerivation,
    /// Addressing/realization in the store layer failed.
    Store(shade_store::StoreError),
    /// The builder or a resolver failed with an IO error.
    #[cfg(feature = "std")]
    Io(io::Error),
    /// A `.drv` in the closure is not readable canonical CDF.
    CdfParse { drv: String, error: shade_cdf::CdfParseError },
    /// A closure CDF parsed but is not a usable derivation (missing keys).
    BadDrv { drv: String, detail: String },
    /// A `dep.*` ref names a store path with no producing derivation in
    /// this evaluation and no existing store entry.
    UnknownDep { drv: String, dep: String },
    /// `dep.*` edges formed a cycle — unconstructible under
    /// input-addressing, so the input is corrupt.
    Cycle { drv: String },
    /// A source derivation (`builder=fetch`) missed every resolver; the
    /// fetcher (shade-pkg 06 §2 phase 1) is not implemented yet.
    FetchUnrealized { drv: String },
    /// A builder phase exited nonzero. `log` (a seam path under the
    /// executor's log root) has the captured output.
    PhaseFailed { drv: String, phase: usize, code: i32, log: String },
    /// The build succeeded but a declared output was not produced.
    MissingOutput { drv: String, detail: String },
    /// The store registrar rejected a realization.
    #[cfg(feature = "std")]
    Register(io::Error),
}

impl core::fmt::Display for BuildError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            BuildError::Eval(e) => write!(f, "eval: {e}"),
            BuildError::NotADerivation => write!(
                f,
                "recipe did not evaluate to a derivation (package-set selection is the driver's job)"
            ),
            BuildError::Store(e) => write!(f, "store: {e}"),
            #[cfg(feature = "std")]
            BuildError::Io(e) => write!(f, "io: {e}"),
            BuildError::CdfParse { drv, error } => write!(f, "{drv}: unreadable CDF: {error}"),
            BuildError::BadDrv { drv, detail } => write!(f, "{drv}: {detail}"),
            BuildError::UnknownDep { drv, dep } => write!(
                f,
                "{drv}: input {dep} has no producing derivation in this evaluation and is not in the store"
            ),
            BuildError::Cycle { drv } => write!(
                f,
                "{drv}: dependency cycle (impossible for well-formed input-addressed derivations — corrupt input)"
            ),
            BuildError::FetchUnrealized { drv } => write!(
                f,
                "{drv}: source derivation is not in the store and fetching is not implemented (shade-pkg 06 §2 phase 1)"
            ),
            BuildError::PhaseFailed { drv, phase, code, log } => write!(
                f,
                "{drv}: phase {phase} failed with exit code {code} (log: {log})"
            ),
            BuildError::MissingOutput { drv, detail } => write!(f, "{drv}: {detail}"),
            #[cfg(feature = "std")]
            BuildError::Register(e) => write!(f, "register: {e}"),
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for BuildError {}

impl BuildError {
    /// The Lythos ABI errno this failure surfaces as at the syscall-shaped
    /// boundary (`abi/lythos-abi/src/errno.rs`; the OROS `shade` binary
    /// reports these once it can call the executor). Host CLIs map any
    /// error to exit code 1 (shade-pkg 07 §1) and print the message.
    pub fn errno(&self) -> u64 {
        use lythos_abi::errno as e;
        #[cfg(feature = "std")]
        fn io_errno(err: &io::Error) -> u64 {
            use lythos_abi::errno as e;
            match err.kind() {
                io::ErrorKind::NotFound => e::ENOENT,
                io::ErrorKind::AlreadyExists => e::EEXIST,
                io::ErrorKind::PermissionDenied => e::ENOPERM,
                io::ErrorKind::StorageFull => e::ENOSPC,
                io::ErrorKind::ReadOnlyFilesystem => e::EROFS,
                _ => e::EINVAL,
            }
        }
        match self {
            // Bad recipe / bad derivation input.
            BuildError::Eval(_)
            | BuildError::NotADerivation
            | BuildError::CdfParse { .. }
            | BuildError::BadDrv { .. }
            | BuildError::Cycle { .. } => e::EINVAL,
            // A named input does not exist; a declared output was not made.
            BuildError::UnknownDep { .. } | BuildError::MissingOutput { .. } => e::ENOENT,
            // Unimplemented realization path.
            BuildError::FetchUnrealized { .. } => e::ENOSYS,
            // The derivation's own build failed — invalid derivation as far
            // as the caller is concerned (there is no dedicated child-exit
            // sentinel in the 14-code ABI table).
            BuildError::PhaseFailed { .. } => e::EINVAL,
            // Store: a conflicting .drv already occupies the path.
            BuildError::Store(shade_store::StoreError::DrvMismatch(_)) => e::EEXIST,
            BuildError::Store(shade_store::StoreError::Cdf(_))
            | BuildError::Store(shade_store::StoreError::OutputPathNotElidable(_)) => e::EINVAL,
            // Seam errors already speak the errno vocabulary (shade-store's
            // FsError mirrors the vfs-core/errno fold).
            BuildError::Store(shade_store::StoreError::Fs { err, .. }) => match err {
                shade_store::FsError::NotFound => e::ENOENT,
                shade_store::FsError::Exists => e::EEXIST,
                shade_store::FsError::NotDir => e::ENOTDIR,
                shade_store::FsError::IsDir => e::EISDIR,
                shade_store::FsError::NotEmpty => e::ENOTEMPTY,
                shade_store::FsError::NoSpace => e::ENOSPC,
                shade_store::FsError::Invalid => e::EINVAL,
                shade_store::FsError::ReadOnly => e::EROFS,
                shade_store::FsError::Device => e::EIO,
                shade_store::FsError::Unsupported => e::ENOSYS,
            },
            #[cfg(feature = "std")]
            BuildError::Io(err) | BuildError::Register(err) => io_errno(err),
        }
    }
}

impl From<EvalError> for BuildError {
    fn from(e: EvalError) -> Self {
        BuildError::Eval(e)
    }
}
impl From<shade_store::StoreError> for BuildError {
    fn from(e: shade_store::StoreError) -> Self {
        BuildError::Store(e)
    }
}

/// A [`plan`] plus the full derivation closure the evaluation emitted —
/// what the executor topologically orders and builds. Closure keys are
/// **canonical** drvPaths (`/shade/store/…​.drv`, independent of the chosen
/// `store_root`) because that is how `dep.*` refs inside CDF bytes name
/// their producers.
#[derive(Debug, Clone)]
pub struct PlanGraph {
    pub root: BuildPlan,
    /// Canonical drvPath of the root (the closure key to start from).
    pub root_drv: String,
    /// Canonical drvPath → CDF bytes, every derivation emitted during eval
    /// (a superset of the root's reachable closure).
    pub closure: alloc::collections::BTreeMap<String, Vec<u8>>,
}

/// Evaluate `recipe` and address it into `store_root`, without building.
/// Separated from [`build`] so callers can inspect the plan (path, digest)
/// — e.g. `shade build --dry-run`-style lookups — and so the eval/address
/// half is unit-testable on its own. `store_root` is a seam path (absolute,
/// `/`-separated — `no_std` has no `std::path`).
pub fn plan(
    recipe: &RecipeRef,
    store_root: &str,
    toolchain: Option<&str>,
    io: &dyn EvalIo,
) -> Result<BuildPlan, BuildError> {
    plan_graph(recipe, store_root, toolchain, io).map(|g| g.root)
}

/// [`plan`], keeping the emitted derivation closure for the executor.
pub fn plan_graph(
    recipe: &RecipeRef,
    store_root: &str,
    toolchain: Option<&str>,
    io: &dyn EvalIo,
) -> Result<PlanGraph, BuildError> {
    let mut ev = Evaluator::new(io);
    ev.toolchain = toolchain.map(str::to_string);
    let pos = Pos { file: Arc::from("<shade-build>"), line: 0, col: 0 };

    let value = match recipe {
        RecipeRef::File(p) => {
            let abs = shadec::parser::normalize_path(p);
            ev.import(&abs, &pos)?
        }
        RecipeRef::Expr { src, base_dir } => {
            let expr = shadec::parser::parse_str(src, Arc::from("<expr>"), base_dir)?;
            let env = ev.initial_env();
            ev.eval(&expr, &env)?
        }
    };

    plan_value(&mut ev, &value, store_root)
}

/// [`plan_graph`] for an already-evaluated derivation value on a live
/// evaluator — the seam a package-set driver (generations, shade-gen) uses to
/// address many derivations out of **one** prism evaluation instead of
/// re-evaluating the prism per package. The returned closure is every
/// derivation `ev` has emitted so far, a superset of the root's reachable
/// closure — which is all [`PlanGraph`] promises.
pub fn plan_value(
    ev: &mut Evaluator,
    value: &Value,
    store_root: &str,
) -> Result<PlanGraph, BuildError> {
    let pos = Pos { file: Arc::from("<shade-build>"), line: 0, col: 0 };

    let Value::Attrs(m) = value else {
        return Err(BuildError::NotADerivation);
    };
    if !ev.attrs_is_derivation(m, &pos)? {
        return Err(BuildError::NotADerivation);
    }

    // Forcing drvPath triggers emission; the CDF is then recorded in ev.drvs.
    let drv_path = ev.force_attr_string(m, "drvPath", &pos)?;
    let cdf = ev
        .drvs
        .get(&*drv_path.s)
        .expect("emission records the CDF keyed by drvPath")
        .as_ref()
        .clone();
    // name is already normalized on the derivation value (drv.rs); version was
    // validated at emission. store_paths_at re-checks both — a bad identity can
    // never reach a path.
    let name = ev.force_attr_string(m, "name", &pos)?.s.to_string();
    let version = ev.force_attr_string(m, "version", &pos)?.s.to_string();

    let paths = shade_store::store_paths_at(store_root, &name, &version, &cdf)?;
    let root = BuildPlan { name, version, cdf, paths };
    let closure = ev
        .drvs
        .iter()
        .map(|(k, v)| (k.clone(), v.as_ref().clone()))
        .collect();
    Ok(PlanGraph { root, root_drv: drv_path.s.to_string(), closure })
}

/// Drive the full pipeline for `recipe` into `store_root`.
///
/// LOOKUP-THEN-BUILD: after addressing, `resolvers` are consulted in order and
/// the first that satisfies the plan wins (no build). On a miss, `builder`
/// stages the output and it is realized into the store. Realization is atomic
/// and idempotent (shade-store), so a concurrent or repeated build converges
/// on the same immutable path.
///
/// The evaluator is `Rc`-based and recurses on deep recipes; run this on a
/// generously sized stack (the host seed CLI uses a large worker thread).
#[cfg(feature = "std")]
pub fn build(
    recipe: &RecipeRef,
    store_root: &Path,
    resolvers: &[&dyn Resolver],
    builder: &dyn Builder,
    toolchain: Option<&str>,
    io: &dyn EvalIo,
) -> Result<Outcome, BuildError> {
    let plan = plan(recipe, path_str(store_root), toolchain, io)?;

    // Lookup first, across every resolver source in order (local store, then
    // any future substituters). First hit wins; nothing is built.
    for r in resolvers {
        if let Some(out_path) = r.resolve(&plan).map_err(BuildError::Io)? {
            let result = Built::Resolved { source: r.source().to_string(), out_path };
            return Ok(Outcome { plan, result });
        }
    }

    // Miss: build, then realize into the store (atomic / idempotent),
    // through the host filesystem backend.
    let staged = builder.build(&plan).map_err(BuildError::Io)?;
    let realized = shade_store::realize_cdf(
        &mut shade_store::HostFs,
        path_str(store_root),
        &plan.name,
        &plan.version,
        &plan.cdf,
        path_str(&staged.root),
    )?;
    let result = Built::Realized { out_path: PathBuf::from(&realized.paths.out_path) };
    Ok(Outcome { plan, result })
}

#[cfg(test)]
mod tests;
