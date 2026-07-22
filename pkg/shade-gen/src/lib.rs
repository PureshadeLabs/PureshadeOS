//! shade-gen — generations, per-user profiles, and activation
//! (docs/shade-pkg/02-store.md §5–6, docs/shade-pkg/10-system-prism.md,
//! docs/shade/generations-profiles.md).
//!
//! A **generation** is one immutable snapshot of an installed set: a numbered
//! directory `N/` holding `manifest`, `prism.lock`, and a `profile/`
//! symlink forest into `/shade/store/*`. Generations live in **lines** — the
//! system line `/shade/gen/system/` and one per-user line
//! `/shade/gen/users/<user>/` — each with its own monotonic counter and its
//! own `current` activation symlink ([`GenLine`]). The lines are independent:
//! flipping one never touches another.
//!
//! ## The filesystem seam
//!
//! All generation/profile/symlink/pointer I/O goes through the injected
//! [`StoreFs`] backend (the B1 seam from [`shade_store`]) — the crate core is
//! `no_std + alloc` and touches no filesystem directly. [`HostFs`] (feature
//! `std`, default) backs the host suite and the `shade-gen` seed CLI;
//! [`OrosFs`] (feature `oros`) backs the same logic on the Lythos ABI. The
//! seam already carries `symlink`/`read_link`/`rename` — everything the
//! profile forest and the activation flip need. On OROS today
//! `symlink`/`read_link` return [`FsError::Unsupported`] (no ABI surface yet
//! — see `shade_store::oros`), so on-target activation is gated on those
//! syscalls landing; the code path is target-ready and identical to the host.
//!
//! ## The three invariants
//!
//! - **Switch and rollback are atomic.** Activation is the 02 §6.1 flip:
//!   build `N/` completely and fsync it, then `rename` a fresh symlink over
//!   `current`. Any reader sees the old generation or the new one, never
//!   neither and never a partial. Rollback is the same flip with a manifest
//!   copied from an older generation — history stays append-only.
//! - **Boot activates a pre-built generation — never builds.** [`boot_activate`]
//!   takes no evaluator, no builder, and no recipe; the types make a boot-time
//!   build unrepresentable. It flips `current` to the pointer-pinned generation
//!   (10 §2 line 3), falling back to the newest complete generation if the
//!   pinned one is missing (10 §6 last-good recovery).
//! - **Every generation is a GC root.** [`GenLine::create`] registers each
//!   package store path under `/shade/roots/` via the roots API
//!   ([`shade_store_db::StoreDb::add_root`]); the GC's byte scan over
//!   `/shade/gen/` (store-db-gc §3.3) covers the same set independently. A
//!   live generation's closure is therefore doubly rooted and never collected.
//!
//! ## Vehicle
//!
//! Host seed (shade-pkg 09 §2): the `shade-gen` binary alongside `shade-build`
//! and `shade-gc`. On OROS the same engine runs behind the unified `shade`
//! binary (`shade os rebuild`, `shade home rebuild`, `shade generations`,
//! `shade rollback`) once an OROS `EvalIo` + the symlink syscalls exist.

#![cfg_attr(not(any(test, feature = "std")), no_std)]

extern crate alloc;

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::cell::RefCell;

use shade_build::BuildError;
use shade_store::backend::{self, join, split_parent};
use shade_store::{FsError, NodeKind, StoreFs};
use shade_store_db::{DbError, StoreDb};

#[cfg(feature = "std")]
pub use shade_store::HostFs;
#[cfg(feature = "oros")]
pub use shade_store::OrosFs;

#[cfg(feature = "std")]
use std::path::Path;

#[cfg(feature = "std")]
mod prism;
#[cfg(feature = "std")]
pub use prism::{
    build_prism_packages, build_prism_packages_on, home_rebuild, home_rebuild_on, os_rebuild,
    os_rebuild_on, BuildRoots, OsRebuildOutcome,
};

/// The seam API is `&str`-pathed (`no_std` — no `std::path`); the host-facing
/// convenience API stays `Path`-based and converts at the boundary.
#[cfg(feature = "std")]
pub(crate) fn path_str(p: &Path) -> &str {
    p.to_str().expect("generation paths must be UTF-8")
}

/// The canonical prism-authoring area (10 §3): default system prism,
/// `prism.shade.bak` after retirement, and the pointer file.
pub const CANONICAL_CFG_ROOT: &str = "/cfg/shade";

/// The pointer file name under the cfg root (10 §2).
pub const POINTER_FILE: &str = "current.pointer";

/// The canonical live-view symlink: `/lth/bin ->
/// /shade/gen/system/current/profile/bin` (02 §6.1, docs/spec/fhs.md).
/// Parameterized everywhere ([`GenLine::wire_view`]) so host tests and
/// bringup tooling never touch the real path.
pub const CANONICAL_LTH_BIN: &str = "/lth/bin";

// ---- Errors -------------------------------------------------------------------

#[derive(Debug)]
pub enum GenError {
    /// A backend filesystem operation failed (seam error, tagged with the
    /// operation and path — same shape as [`shade_store_db::DbError::Fs`]).
    Fs {
        op: &'static str,
        path: String,
        err: FsError,
    },
    /// The roots API (store db) failed.
    Db(DbError),
    /// `current.pointer` exists but does not parse (10 §2: three lines).
    MalformedPointer,
    /// Two packages provide the same profile-relative file (02 §5 — collision
    /// is an error at generation-build time; no priority system in v1).
    Collision {
        rel: String,
        package: String,
        existing_target: String,
    },
    /// The numbered generation directory is missing manifest or profile —
    /// it must never be activated (02 §6.1 step 1: build completely first).
    Incomplete(u64),
    NoSuchGeneration(u64),
    /// Rollback with no generation before `current` (or no `current` at all).
    NothingToRollBack,
    /// Boot found no complete generation to activate. Boot never builds
    /// (10 §6), so this is fatal to activation, not a trigger to build.
    NoGeneration,
    /// The prism did not evaluate to a package set or a derivation, or the
    /// selector named something that is not a derivation.
    NotAPackageSet(String),
    /// No system prism source: no pointer, no `/cfg/shade/prism.shade[.bak]`.
    NoSystemPrism,
    /// The pointer names a source that cannot be resolved — fail loud, never
    /// fall back to `.bak` while a pointer exists (10 §4).
    UnresolvablePointer(String),
    /// A host-side (std) I/O failure — rebuild drivers only; the seam paths
    /// report [`GenError::Fs`].
    #[cfg(feature = "std")]
    Io(std::io::Error),
    Build(BuildError),
}

impl core::fmt::Display for GenError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            GenError::Fs { op, path, err } => write!(f, "fs: {op} {path}: {err}"),
            GenError::Db(e) => write!(f, "db: {e}"),
            GenError::MalformedPointer => write!(f, "malformed current.pointer (10 §2)"),
            GenError::Collision { rel, package, existing_target } => write!(
                f,
                "profile collision on `{rel}`: package {package} conflicts with existing entry -> {existing_target} (02 §5 — no priority system in v1)"
            ),
            GenError::Incomplete(n) => {
                write!(f, "generation {n} is incomplete (missing manifest or profile/)")
            }
            GenError::NoSuchGeneration(n) => write!(f, "no generation {n}"),
            GenError::NothingToRollBack => write!(f, "nothing to roll back to"),
            GenError::NoGeneration => {
                write!(f, "no complete generation to activate (boot never builds, 10 §6)")
            }
            GenError::NotAPackageSet(d) => write!(f, "not a package set: {d}"),
            GenError::NoSystemPrism => write!(
                f,
                "no system prism: no pointer and no /cfg/shade/prism.shade[.bak] (10 §4)"
            ),
            GenError::UnresolvablePointer(d) => write!(
                f,
                "pointer target unresolvable: {d} (failing loud — never falling back to .bak while a pointer exists, 10 §4)"
            ),
            #[cfg(feature = "std")]
            GenError::Io(e) => write!(f, "io: {e}"),
            GenError::Build(e) => write!(f, "build: {e}"),
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for GenError {}

impl From<DbError> for GenError {
    fn from(e: DbError) -> Self {
        GenError::Db(e)
    }
}
#[cfg(feature = "std")]
impl From<std::io::Error> for GenError {
    fn from(e: std::io::Error) -> Self {
        GenError::Io(e)
    }
}
impl From<BuildError> for GenError {
    fn from(e: BuildError) -> Self {
        GenError::Build(e)
    }
}
impl From<shadec::error::EvalError> for GenError {
    fn from(e: shadec::error::EvalError) -> Self {
        GenError::Build(BuildError::Eval(e))
    }
}

/// Shorthand: tag a backend failure with the operation and target path.
fn fs_op(op: &'static str, path: &str) -> impl FnOnce(FsError) -> GenError {
    let path = String::from(path);
    move |err| GenError::Fs { op, path, err }
}

// ---- Manifest -----------------------------------------------------------------

/// Record-format header line (mirrors the db's `shade-db=1` and CDF's
/// `shade-drv=1`): bumped on any change to the manifest record shape.
const MANIFEST_VERSION: &str = "shade-gen=1";

/// One `package.<i>.*` entry of the generation manifest. `store_path` is a
/// seam path (absolute, `/`-separated).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageEntry {
    pub name: String,
    pub version: String,
    pub store_path: String,
    /// Explicitly asked for vs pulled in as a dep (GC/remove semantics).
    pub requested: bool,
}

/// A parsed generation `manifest` — what is installed and why, in the one
/// canonical on-disk record format every `/shade/` subsystem uses (the CDF /
/// `db/valid` line discipline, store-db-gc §1.1): a header line first, then
/// lowercase `key=value` lines in bytewise-sorted key order, one LF per line,
/// trailing LF. No TOML anywhere under `/shade/`. List fields use indexed
/// keys (`package.<i>.*`), exactly as CDF's `dep.<i>`/`phase.<i>`.
///
/// ```text
/// shade-gen=1
/// created=1783814400                    # unix seconds (like db `registered`); informational, never hashed
/// package.0.name=alpha
/// package.0.path=/shade/store/<digest>-alpha-1.0
/// package.0.requested=1                 # 1 = explicitly asked for, 0 = dep
/// package.0.version=1.0
/// parent=1                              # generation this was derived from; 0 = none
/// reason=os rebuild /user/lyon/.prism   # human-readable, set by the CLI
/// ```
///
/// Byte-stable: [`serialize`](Manifest::serialize) is canonical (deterministic
/// key order), so parse → re-serialize reproduces the input bytes exactly —
/// the same byte-identity discipline as CDF and the db records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    /// Creation time, unix seconds; informational, never hashed anywhere.
    pub created: u64,
    /// Generation this was derived from; 0 = none.
    pub parent: u64,
    /// Human-readable, set by the CLI.
    pub reason: String,
    pub packages: Vec<PackageEntry>,
}

impl Manifest {
    /// Serialize to the canonical record: header first, then keys
    /// bytewise-sorted, one LF per line, trailing LF (the CDF line
    /// discipline). Values are forced single-line — the format is line-based.
    fn serialize(&self) -> String {
        let mut kv: Vec<(String, String)> = alloc::vec![
            ("created".to_string(), self.created.to_string()),
            ("parent".to_string(), self.parent.to_string()),
            ("reason".to_string(), one_line(&self.reason)),
        ];
        for (i, p) in self.packages.iter().enumerate() {
            kv.push((format!("package.{i}.name"), one_line(&p.name)));
            kv.push((format!("package.{i}.path"), one_line(&p.store_path)));
            kv.push((format!("package.{i}.requested"), String::from(if p.requested { "1" } else { "0" })));
            kv.push((format!("package.{i}.version"), one_line(&p.version)));
        }
        kv.sort(); // bytewise key order — the canonical order
        let mut s = String::from(MANIFEST_VERSION);
        s.push('\n');
        for (k, v) in &kv {
            s.push_str(k);
            s.push('=');
            s.push_str(v);
            s.push('\n');
        }
        s
    }

    /// Parse a manifest record. `None` if the header is missing/unsupported
    /// or a line is malformed. Unknown keys are tolerated (forward-compat,
    /// same as the db's `ValidRecord`); packages reassemble by numeric index.
    fn parse(s: &str) -> Option<Manifest> {
        let mut lines = s.lines();
        if lines.next()? != MANIFEST_VERSION {
            return None;
        }
        let mut m = Manifest { created: 0, parent: 0, reason: String::new(), packages: Vec::new() };
        let mut pkgs: BTreeMap<usize, PackageEntry> = BTreeMap::new();
        for line in lines {
            let (k, v) = line.split_once('=')?;
            match k {
                "created" => m.created = v.parse().ok()?,
                "parent" => m.parent = v.parse().ok()?,
                "reason" => m.reason = v.to_string(),
                _ => {
                    if let Some(rest) = k.strip_prefix("package.") {
                        let (idx, field) = rest.split_once('.')?;
                        let idx: usize = idx.parse().ok()?;
                        let p = pkgs.entry(idx).or_insert_with(|| PackageEntry {
                            name: String::new(),
                            version: String::new(),
                            store_path: String::new(),
                            requested: false,
                        });
                        match field {
                            "name" => p.name = v.to_string(),
                            "path" => p.store_path = v.to_string(),
                            "requested" => p.requested = v == "1",
                            "version" => p.version = v.to_string(),
                            _ => {} // unknown package field: tolerate
                        }
                    }
                    // unknown key: tolerate (forward-compat)
                }
            }
        }
        m.packages = pkgs.into_values().collect();
        Some(m)
    }
}

/// Force a value single-line: the record format is line-based, so an embedded
/// LF/CR would corrupt the record. Reasons and names never legitimately carry
/// them; replaced with a space rather than erroring.
fn one_line(s: &str) -> String {
    if s.contains(['\n', '\r']) {
        s.replace(['\n', '\r'], " ")
    } else {
        s.to_string()
    }
}

/// One listed generation: its number, manifest, and whether `current` points
/// at it.
#[derive(Debug, Clone)]
pub struct GenInfo {
    pub number: u64,
    pub manifest: Manifest,
    pub current: bool,
}

// ---- The pointer file (10 §2) ---------------------------------------------------

/// The parsed `/cfg/shade/current.pointer`: three lines — prism path, output
/// selector (may be empty), pinned system generation number.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pointer {
    pub prism: String,
    pub selector: String,
    pub generation: u64,
}

/// Read the pointer under `cfg_root` through the seam. `Ok(None)` if absent
/// (never-rebuilt or deliberately removed — the `.bak` fallback case, 10 §4).
pub fn read_pointer_on(
    fs: &mut dyn StoreFs,
    cfg_root: &str,
) -> Result<Option<Pointer>, GenError> {
    let path = join(cfg_root, POINTER_FILE);
    let bytes = match fs.read_file(&path) {
        Ok(b) => b,
        Err(FsError::NotFound) => return Ok(None),
        Err(e) => return Err(fs_op("read", &path)(e)),
    };
    let s = String::from_utf8_lossy(&bytes);
    let mut lines = s.lines();
    let prism = lines.next().ok_or(GenError::MalformedPointer)?.to_string();
    let selector = lines.next().ok_or(GenError::MalformedPointer)?.to_string();
    let generation = lines
        .next()
        .and_then(|l| l.trim().parse().ok())
        .ok_or(GenError::MalformedPointer)?;
    Ok(Some(Pointer { prism, selector, generation }))
}

/// Write the pointer atomically through the seam (temp + rename, trailing
/// newline — 10 §2: `shade os rebuild` rewrites all three lines atomically on
/// success).
pub fn write_pointer_on(fs: &mut dyn StoreFs, cfg_root: &str, p: &Pointer) -> Result<(), GenError> {
    let bytes = format!("{}\n{}\n{}\n", p.prism, p.selector, p.generation);
    write_atomic(fs, &join(cfg_root, POINTER_FILE), bytes.as_bytes())
}

/// Re-pin the pointer's generation (line 3 only; source lines untouched), if
/// a pointer exists. System-line **rollback** calls this so the next boot
/// activates the generation the user just rolled back to — without it, boot
/// would return to the pre-rollback pin. A rollback produces a built
/// generation like any rebuild, so re-pinning preserves the 10 §6 invariant
/// (boot still activates a pre-built generation, never a source prism).
pub fn repin_generation_on(
    fs: &mut dyn StoreFs,
    cfg_root: &str,
    generation: u64,
) -> Result<(), GenError> {
    if let Some(mut p) = read_pointer_on(fs, cfg_root)? {
        p.generation = generation;
        write_pointer_on(fs, cfg_root, &p)?;
    }
    Ok(())
}

/// [`read_pointer_on`] on the host backend.
#[cfg(feature = "std")]
pub fn read_pointer(cfg_root: &Path) -> Result<Option<Pointer>, GenError> {
    read_pointer_on(&mut HostFs, path_str(cfg_root))
}

/// [`write_pointer_on`] on the host backend.
#[cfg(feature = "std")]
pub fn write_pointer(cfg_root: &Path, p: &Pointer) -> Result<(), GenError> {
    write_pointer_on(&mut HostFs, path_str(cfg_root), p)
}

/// [`repin_generation_on`] on the host backend.
#[cfg(feature = "std")]
pub fn repin_generation(cfg_root: &Path, generation: u64) -> Result<(), GenError> {
    repin_generation_on(&mut HostFs, path_str(cfg_root), generation)
}

// ---- The generation line --------------------------------------------------------

/// One generation line — `/shade/gen/system/` or `/shade/gen/users/<user>/`
/// (02 §5): its own monotonic counter, its own `current` symlink, its own
/// append-only history. Cheap to construct; holds no open handles. The
/// backend lives in a `RefCell` so the API stays `&self` (production backends
/// are stateless — `HostFs`/`OrosFs` are `Copy`); the roots db carries its
/// own clone of the backend, same as the executor's registrar.
#[derive(Debug)]
pub struct GenLine<F: StoreFs> {
    fs: RefCell<F>,
    line_root: String,
    /// Root-name tag: `system` or `user-<user>` — generation roots register
    /// as `/shade/roots/gen-<tag>-<N>-<i>` (store-db-gc §3.1 `<owner>-<label>`).
    tag: String,
    db: StoreDb<F>,
}

#[cfg(feature = "std")]
impl GenLine<HostFs> {
    /// The system line `<shade_root>/gen/system` on the host backend (built
    /// by `shade os rebuild`, privileged; 07 §2.1).
    pub fn system(shade_root: impl AsRef<Path>) -> Self {
        GenLine::system_on(HostFs, path_str(shade_root.as_ref()))
    }

    /// A per-user line `<shade_root>/gen/users/<user>` on the host backend
    /// (built by `shade home rebuild`, unprivileged; 07 §2.2, 10 §5).
    pub fn user(shade_root: impl AsRef<Path>, user: &str) -> Self {
        GenLine::user_on(HostFs, path_str(shade_root.as_ref()), user)
    }
}

impl<F: StoreFs + Clone> GenLine<F> {
    /// The system line over an injected backend. `shade_root` is a seam path.
    pub fn system_on(fs: F, shade_root: &str) -> Self {
        let r = shade_root.trim_end_matches('/');
        GenLine {
            fs: RefCell::new(fs.clone()),
            line_root: format!("{r}/gen/system"),
            tag: "system".to_string(),
            db: StoreDb::with_backend(fs, r),
        }
    }

    /// A per-user line over an injected backend. `shade_root` is a seam path.
    pub fn user_on(fs: F, shade_root: &str, user: &str) -> Self {
        let r = shade_root.trim_end_matches('/');
        GenLine {
            fs: RefCell::new(fs.clone()),
            line_root: format!("{r}/gen/users/{user}"),
            tag: format!("user-{user}"),
            db: StoreDb::with_backend(fs, r),
        }
    }
}

impl<F: StoreFs> GenLine<F> {
    pub fn line_root(&self) -> &str {
        &self.line_root
    }

    fn gen_dir(&self, n: u64) -> String {
        join(&self.line_root, &n.to_string())
    }

    /// A generation is complete iff its manifest and profile exist. `create`
    /// renames a fully-built, fsynced tree into place, so an incomplete
    /// numbered directory can only be corruption — never activated.
    pub fn is_complete(&self, n: u64) -> bool {
        let fs = &mut *self.fs.borrow_mut();
        is_complete_on(fs, &self.gen_dir(n))
    }

    /// The generation `current` points at, if the symlink exists and parses.
    pub fn current(&self) -> Result<Option<u64>, GenError> {
        let fs = &mut *self.fs.borrow_mut();
        current_on(fs, &self.line_root)
    }

    /// The numbered generations present in this line, ascending.
    pub fn numbers(&self) -> Result<Vec<u64>, GenError> {
        let fs = &mut *self.fs.borrow_mut();
        numbers_on(fs, &self.line_root)
    }

    /// The newest complete generation, if any — boot's last-good fallback
    /// (10 §6, 02 §6.2).
    pub fn latest_complete(&self) -> Result<Option<u64>, GenError> {
        let fs = &mut *self.fs.borrow_mut();
        Ok(numbers_on(fs, &self.line_root)?
            .into_iter()
            .rev()
            .find(|&n| is_complete_on(fs, &join(&self.line_root, &n.to_string()))))
    }

    /// Whether generation `n` is closure-complete (structurally complete AND
    /// its full store closure exists on disk) — the boot-time bar (10 §6).
    pub fn closure_complete(&self, n: u64) -> bool {
        let fs = &mut *self.fs.borrow_mut();
        closure_complete_on(fs, &self.gen_dir(n))
    }

    /// The newest **closure-complete** generation, if any — boot's cold-start
    /// last-good fallback when the pinned generation's closure is incomplete
    /// (a referenced store path is missing). Never builds.
    pub fn latest_closure_complete(&self) -> Result<Option<u64>, GenError> {
        let fs = &mut *self.fs.borrow_mut();
        Ok(numbers_on(fs, &self.line_root)?
            .into_iter()
            .rev()
            .find(|&n| closure_complete_on(fs, &join(&self.line_root, &n.to_string()))))
    }

    /// List generations with their manifests, ascending, `current` marked —
    /// `shade generations list` (07 §2).
    pub fn list(&self) -> Result<Vec<GenInfo>, GenError> {
        let fs = &mut *self.fs.borrow_mut();
        let current = current_on(fs, &self.line_root)?;
        let mut out = Vec::new();
        for n in numbers_on(fs, &self.line_root)? {
            if let Some(manifest) = read_manifest_on(fs, &self.gen_dir(n))? {
                out.push(GenInfo { number: n, manifest, current: current == Some(n) });
            }
        }
        Ok(out)
    }

    /// The manifest of generation `n`, if present and well-formed.
    pub fn read_manifest(&self, n: u64) -> Result<Option<Manifest>, GenError> {
        let fs = &mut *self.fs.borrow_mut();
        read_manifest_on(fs, &self.gen_dir(n))
    }

    /// Create a new generation from `packages`: allocate the next number,
    /// build `N/` (manifest, lock snapshot, profile symlink forest) in a
    /// sibling temp dir, fsync it, rename it into place, and register every
    /// package store path as a GC root (`/shade/roots/gen-<tag>-<N>-<i>`).
    ///
    /// The new generation is **not activated** — that is [`activate`]'s flip,
    /// so history append and activation stay separate steps (02 §6.1 step 1
    /// vs steps 2–4). Collisions in the profile forest abort with
    /// [`GenError::Collision`] and leave the line untouched.
    ///
    /// [`activate`]: GenLine::activate
    pub fn create(
        &self,
        packages: &[PackageEntry],
        lock: Option<&[u8]>,
        reason: &str,
        parent: u64,
    ) -> Result<u64, GenError> {
        let n = {
            let fs = &mut *self.fs.borrow_mut();
            backend::create_dir_all(fs, &self.line_root)
                .map_err(fs_op("create_dir_all", &self.line_root))?;
            // Allocate-and-rename loop: two concurrent creators may pick the
            // same number; rename onto an existing directory fails, and the
            // loser retries with the next one. Numbers stay monotonic, never
            // reused.
            loop {
                let n = numbers_on(fs, &self.line_root)?.last().copied().unwrap_or(0) + 1;
                let tmp = backend::temp_sibling(fs, &self.line_root, &n.to_string(), "gen");

                if let Err(e) = build_gen_tree(fs, &tmp, packages, lock, reason, parent) {
                    backend::remove_tree(fs, &tmp);
                    return Err(e);
                }

                let dst = self.gen_dir(n);
                match fs.rename(&tmp, &dst) {
                    Ok(()) => {
                        let _ = fs.sync_dir(&self.line_root);
                        break n;
                    }
                    Err(_) if fs.exists(&dst) => {
                        // Lost the allocation race; retry with a fresh number.
                        backend::remove_tree(fs, &tmp);
                        continue;
                    }
                    Err(e) => {
                        backend::remove_tree(fs, &tmp);
                        return Err(fs_op("rename", &dst)(e));
                    }
                }
            }
        };
        // Root registration (roots API seam): one direct root per package
        // store path. The GC additionally byte-scans /shade/gen/
        // (store-db-gc §3.3) — belt and braces; the over-approximation
        // direction is the safe one. Outside the fs borrow: the db holds its
        // own backend handle.
        for (i, p) in packages.iter().enumerate() {
            self.db
                .add_root(&format!("gen-{}-{n}-{i}", self.tag), &p.store_path)?;
        }
        Ok(n)
    }

    /// Activate generation `n` — the 02 §6.1 flip, and the **only** thing
    /// activation touches in the line:
    ///
    /// 1. verify `N/` is complete (built + renamed by [`create`]);
    /// 2. symlink `.current.new -> N`;
    /// 3. `rename(".current.new", "current")` — atomic at the VFS level: any
    ///    reader sees the old target or the new one, never neither;
    /// 4. fsync the line directory (forces the RFS commit, 02 §6.3).
    ///
    /// Idempotent: re-activating the current generation re-runs the same flip
    /// to the same target. The live system view (`/lth/bin`) is a separate,
    /// also-idempotent step — [`wire_view`](GenLine::wire_view) — because it
    /// lives outside the line and only the system line has one.
    ///
    /// [`create`]: GenLine::create
    pub fn activate(&self, n: u64) -> Result<(), GenError> {
        let fs = &mut *self.fs.borrow_mut();
        let d = self.gen_dir(n);
        if !fs.exists(&d) {
            return Err(GenError::NoSuchGeneration(n));
        }
        if !is_complete_on(fs, &d) {
            return Err(GenError::Incomplete(n));
        }
        let tmp = join(&self.line_root, ".current.new");
        let _ = fs.unlink(&tmp);
        fs.symlink(&n.to_string(), &tmp).map_err(fs_op("symlink", &tmp))?;
        rename_replace(fs, &tmp, &join(&self.line_root, "current"))?;
        let _ = fs.sync_dir(&self.line_root);
        Ok(())
    }

    /// Wire the live view: `link -> <line>/current/profile/bin` — the single
    /// symlink everything else dereferences through (`/lth/bin` for the
    /// system line, 02 §6.1). Idempotent and atomic (temp + rename); the
    /// target string goes through `current`, so subsequent [`activate`] flips
    /// retarget the view with no further writes here.
    ///
    /// [`activate`]: GenLine::activate
    pub fn wire_view(&self, link: &str) -> Result<(), GenError> {
        let fs = &mut *self.fs.borrow_mut();
        let (parent, _) = split_parent(link);
        backend::create_dir_all(fs, parent).map_err(fs_op("create_dir_all", parent))?;
        let target = join(&join(&self.line_root, "current"), "profile/bin");
        let tmp = join(parent, ".view.new");
        let _ = fs.unlink(&tmp);
        fs.symlink(&target, &tmp).map_err(fs_op("symlink", &tmp))?;
        rename_replace(fs, &tmp, link)?;
        Ok(())
    }

    /// Roll back: create a **new** generation whose manifest (and lock) copy
    /// generation `target`'s (default: the generation before `current`), then
    /// activate it. History stays linear and append-only — rollback twice
    /// returns to where you started (02 §5, 07 §`shade rollback`).
    pub fn rollback(&self, target: Option<u64>) -> Result<u64, GenError> {
        let (target, manifest, lock) = {
            let fs = &mut *self.fs.borrow_mut();
            let current = current_on(fs, &self.line_root)?.ok_or(GenError::NothingToRollBack)?;
            let target = match target {
                Some(t) => t,
                None => numbers_on(fs, &self.line_root)?
                    .into_iter()
                    .rev()
                    .find(|&n| n < current)
                    .ok_or(GenError::NothingToRollBack)?,
            };
            let manifest = read_manifest_on(fs, &self.gen_dir(target))?
                .ok_or(GenError::NoSuchGeneration(target))?;
            let lock = fs.read_file(&join(&self.gen_dir(target), "prism.lock")).ok();
            (target, manifest, lock)
        };
        let n = self.create(
            &manifest.packages,
            lock.as_deref(),
            &format!("rollback to {target}"),
            target,
        )?;
        self.activate(n)?;
        Ok(n)
    }
}

// ---- Line internals over an explicit backend --------------------------------------
//
// Free functions so the `GenLine` methods can compose them under a single
// `RefCell` borrow (`create`/`rollback` would double-borrow through `&self`
// methods otherwise).

fn is_complete_on(fs: &mut dyn StoreFs, gen_dir: &str) -> bool {
    fs.exists(&join(gen_dir, "manifest")) && fs.exists(&join(gen_dir, "profile"))
}

/// A generation is **closure-complete** iff it is structurally complete
/// (manifest + profile) AND every store path in its closure still exists on
/// disk. This is the boot-time bar (10 §6): a cold boot against a persistent
/// store can find a generation whose tree is intact but whose referenced store
/// outputs were, e.g., lost with a volatile store or never persisted — its
/// profile forest then points at `/shade/store/<hash>` paths that do not
/// exist, and no-build-at-boot means it cannot be repaired. Such a generation
/// must never be activated; boot falls back to the newest closure-complete one.
///
/// The closure is the manifest's package store paths — exactly what the profile
/// forest symlinks resolve into and what the live system runs.
fn closure_complete_on(fs: &mut dyn StoreFs, gen_dir: &str) -> bool {
    if !is_complete_on(fs, gen_dir) {
        return false;
    }
    match read_manifest_on(fs, gen_dir) {
        Ok(Some(m)) => m.packages.iter().all(|p| fs.exists(&p.store_path)),
        // Missing/garbled manifest ⇒ not bootable (already excluded by
        // is_complete_on for the missing case; belt-and-braces for a parse
        // failure).
        _ => false,
    }
}

fn current_on(fs: &mut dyn StoreFs, line_root: &str) -> Result<Option<u64>, GenError> {
    let link = join(line_root, "current");
    match fs.read_link(&link) {
        Ok(t) => Ok(t.parse().ok()),
        Err(FsError::NotFound) => Ok(None),
        Err(e) => Err(fs_op("read_link", &link)(e)),
    }
}

fn numbers_on(fs: &mut dyn StoreFs, line_root: &str) -> Result<Vec<u64>, GenError> {
    let mut out = Vec::new();
    let entries = match fs.read_dir(line_root) {
        Ok(entries) => entries,
        Err(FsError::NotFound) => return Ok(out),
        Err(e) => return Err(fs_op("read_dir", line_root)(e)),
    };
    for (name, _) in entries {
        if let Ok(n) = name.parse() {
            out.push(n);
        }
    }
    out.sort_unstable();
    Ok(out)
}

fn read_manifest_on(fs: &mut dyn StoreFs, gen_dir: &str) -> Result<Option<Manifest>, GenError> {
    let path = join(gen_dir, "manifest");
    match fs.read_file(&path) {
        Ok(bytes) => Ok(Manifest::parse(&String::from_utf8_lossy(&bytes))),
        Err(FsError::NotFound) => Ok(None),
        Err(e) => Err(fs_op("read", &path)(e)),
    }
}

/// Build a complete generation tree at `tmp`: manifest, lock snapshot, and
/// the profile symlink forest, fsynced — ready for the rename into place.
fn build_gen_tree(
    fs: &mut dyn StoreFs,
    tmp: &str,
    packages: &[PackageEntry],
    lock: Option<&[u8]>,
    reason: &str,
    parent: u64,
) -> Result<(), GenError> {
    fs.mkdir(tmp).map_err(fs_op("mkdir", tmp))?;
    let manifest = Manifest {
        created: now_unix(),
        parent,
        reason: reason.to_string(),
        packages: packages.to_vec(),
    };
    let mpath = join(tmp, "manifest");
    fs.write_file(&mpath, manifest.serialize().as_bytes(), false)
        .map_err(fs_op("write", &mpath))?;
    // The lockfile snapshot that produced this generation; the unified schema
    // is deferred (shade 08 §5), so absent is legal.
    let lpath = join(tmp, "prism.lock");
    fs.write_file(
        &lpath,
        lock.unwrap_or(b"# no prism.lock (unified lock schema deferred, shade 08 \xc2\xa75)\n"),
        false,
    )
    .map_err(fs_op("write", &lpath))?;
    let profile = join(tmp, "profile");
    fs.mkdir(&profile).map_err(fs_op("mkdir", &profile))?;
    for p in packages {
        merge_into_profile(fs, &profile, &p.store_path, &p.store_path, &p.name)?;
    }
    let _ = fs.sync_dir(tmp);
    Ok(())
}

/// Rename with replace semantics through the seam. Where the backend's rename
/// is no-replace (OROS RFS, MemFs), the destination is unlinked first — the
/// same fallback the store db uses for its records. On such a backend the
/// flip degrades to unlink+rename (a reader can race into `NotFound`); the
/// host backend renames over the destination atomically.
fn rename_replace(fs: &mut dyn StoreFs, tmp: &str, dst: &str) -> Result<(), GenError> {
    match fs.rename(tmp, dst) {
        Ok(()) => Ok(()),
        Err(FsError::Exists) => {
            fs.unlink(dst).map_err(fs_op("unlink", dst))?;
            fs.rename(tmp, dst).map_err(fs_op("rename", dst))
        }
        Err(e) => {
            let _ = fs.unlink(tmp);
            Err(fs_op("rename", dst)(e))
        }
    }
}

/// Atomic write through the seam: temp sibling + rename (+ best-effort dir
/// fsync) — the pointer's 10 §2 atomic rewrite.
fn write_atomic(fs: &mut dyn StoreFs, path: &str, bytes: &[u8]) -> Result<(), GenError> {
    let (parent, name) = split_parent(path);
    backend::create_dir_all(fs, parent).map_err(fs_op("create_dir_all", parent))?;
    let tmp = backend::temp_sibling(fs, parent, name, "ptr");
    fs.write_file(&tmp, bytes, false).map_err(fs_op("write", &tmp))?;
    rename_replace(fs, &tmp, path)?;
    let _ = fs.sync_dir(parent);
    Ok(())
}

// ---- Boot activation (10 §6) ----------------------------------------------------

/// What [`boot_activate`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootOutcome {
    /// The generation activated.
    pub generation: u64,
    /// The pointer's pinned generation, if a pointer was present.
    pub pinned: Option<u64>,
    /// True if the pinned (or `current`) generation was unusable and the
    /// newest complete generation was activated instead — the last-good
    /// recovery of 02 §6.2 / 10 §6.
    pub fell_back: bool,
}

/// Boot-time activation of the **pre-built** system generation, over an
/// injected backend. Boot consumes built generations, never source prisms
/// (10 §6): this function takes no evaluator, no builder, and no recipe — a
/// boot-time build is unrepresentable, which is how the no-build-at-boot
/// invariant is enforced.
///
/// Order: the pointer's pinned generation (line 3, 10 §2) if complete; else
/// — pointer absent — whatever `current` already points at; else the newest
/// complete generation (last-good recovery); else [`GenError::NoGeneration`].
/// The activation itself is the same idempotent flip as any switch, plus
/// wiring the live view at `lth_bin` when given.
pub fn boot_activate_on<F: StoreFs + Clone>(
    fs: F,
    shade_root: &str,
    cfg_root: &str,
    lth_bin: Option<&str>,
) -> Result<BootOutcome, GenError> {
    let line = GenLine::system_on(fs, shade_root);
    let pinned =
        read_pointer_on(&mut *line.fs.borrow_mut(), cfg_root)?.map(|p| p.generation);

    // Boot bar is CLOSURE-completeness, not just structural: a persistent store
    // may hold a generation tree whose referenced store paths no longer exist
    // (e.g. after a store loss, or a partial persist). no-build-at-boot means
    // such a generation cannot be repaired, so it must never be activated —
    // fall back to the newest generation with a complete closure (10 §6). If
    // none has a complete closure, fail loud (NoGeneration): boot never builds.
    let (generation, fell_back) = match pinned {
        Some(n) if line.closure_complete(n) => (n, false),
        Some(_) => (
            // Pinned generation's closure missing/corrupt: last-good with a
            // complete closure, never a rebuild and never `.bak` (10 §4, 10 §6).
            line.latest_closure_complete()?.ok_or(GenError::NoGeneration)?,
            true,
        ),
        None => match line.current()? {
            Some(c) if line.closure_complete(c) => (c, false),
            _ => (line.latest_closure_complete()?.ok_or(GenError::NoGeneration)?, true),
        },
    };

    line.activate(generation)?;
    if let Some(link) = lth_bin {
        line.wire_view(link)?;
    }
    Ok(BootOutcome { generation, pinned, fell_back })
}

/// [`boot_activate_on`] on the host backend.
#[cfg(feature = "std")]
pub fn boot_activate(
    shade_root: &Path,
    cfg_root: &Path,
    lth_bin: Option<&Path>,
) -> Result<BootOutcome, GenError> {
    boot_activate_on(
        HostFs,
        path_str(shade_root),
        path_str(cfg_root),
        lth_bin.map(path_str),
    )
}

// ---- Profile symlink forest -------------------------------------------------------

/// Merge one package's output tree into the profile: directories merge,
/// every file/symlink becomes a profile symlink to its absolute store path.
/// A leaf that already exists is a collision — an error at generation-build
/// time (02 §5), naming the file and the losing package.
fn merge_into_profile(
    fs: &mut dyn StoreFs,
    dst: &str,
    pkg_root: &str,
    src: &str,
    pkg_name: &str,
) -> Result<(), GenError> {
    let entries = fs.read_dir(src).map_err(fs_op("read_dir", src))?;
    for (name, kind) in entries {
        let src_path = join(src, &name);
        let dst_path = join(dst, &name);
        if kind == NodeKind::Dir {
            match fs.mkdir(&dst_path) {
                Ok(()) | Err(FsError::Exists) => {}
                Err(e) => return Err(fs_op("mkdir", &dst_path)(e)),
            }
            merge_into_profile(fs, &dst_path, pkg_root, &src_path, pkg_name)?;
        } else {
            if fs.exists(&dst_path) {
                let rel = src_path
                    .strip_prefix(&format!("{pkg_root}/"))
                    .unwrap_or(&src_path)
                    .to_string();
                let existing_target = fs
                    .read_link(&dst_path)
                    .unwrap_or_else(|_| "<non-symlink>".to_string());
                return Err(GenError::Collision { rel, package: pkg_name.to_string(), existing_target });
            }
            fs.symlink(&src_path, &dst_path).map_err(fs_op("symlink", &dst_path))?;
        }
    }
    Ok(())
}

// ---- Small helpers ----------------------------------------------------------------
//
// Wall-clock time (the manifest `created=` stamp) is the one ambient fact the
// fs seam does not carry — cfg'd per platform, same as the store db's stamp.

#[cfg(feature = "std")]
fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(all(feature = "oros", not(feature = "std")))]
fn now_unix() -> u64 {
    // SYS_TIME_EPOCH: unix epoch milliseconds (CMOS-anchored).
    (unsafe { lythos_syscall::syscall0(lythos_abi::syscall::SYS_TIME_EPOCH) }) / 1000
}

// Featureless no_std fallback (keeps `--no-default-features` checkable).
#[cfg(not(any(feature = "std", feature = "oros")))]
fn now_unix() -> u64 {
    0
}

/// RFC 3339 UTC from unix seconds (civil-from-days, Hinnant's algorithm).
/// Display-only: the manifest stores `created` as raw unix seconds (like the
/// db's `registered`); this formats it for `list` output and diagnostics.
pub fn rfc3339_utc(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3_600,
        (rem % 3_600) / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests;
