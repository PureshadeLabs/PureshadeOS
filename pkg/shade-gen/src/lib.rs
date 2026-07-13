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
//! `shade rollback`) once argv + a VFS `EvalIo` exist.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use shade_build::BuildError;
use shade_store_db::StoreDb;

mod prism;
pub use prism::{build_prism_packages, home_rebuild, os_rebuild, BuildRoots, OsRebuildOutcome};

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
    Io(io::Error),
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
    Build(BuildError),
}

impl std::fmt::Display for GenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GenError::Io(e) => write!(f, "io: {e}"),
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
            GenError::Build(e) => write!(f, "build: {e}"),
        }
    }
}

impl std::error::Error for GenError {}

impl From<io::Error> for GenError {
    fn from(e: io::Error) -> Self {
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

// ---- Manifest -----------------------------------------------------------------

/// Record-format header line (mirrors the db's `shade-db=1` and CDF's
/// `shade-drv=1`): bumped on any change to the manifest record shape.
const MANIFEST_VERSION: &str = "shade-gen=1";

/// One `package.<i>.*` entry of the generation manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageEntry {
    pub name: String,
    pub version: String,
    pub store_path: PathBuf,
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
        let mut kv: Vec<(String, String)> = vec![
            ("created".to_string(), self.created.to_string()),
            ("parent".to_string(), self.parent.to_string()),
            ("reason".to_string(), one_line(&self.reason)),
        ];
        for (i, p) in self.packages.iter().enumerate() {
            kv.push((format!("package.{i}.name"), one_line(&p.name)));
            kv.push((format!("package.{i}.path"), one_line(&p.store_path.to_string_lossy())));
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
        let mut pkgs: std::collections::BTreeMap<usize, PackageEntry> =
            std::collections::BTreeMap::new();
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
                            store_path: PathBuf::new(),
                            requested: false,
                        });
                        match field {
                            "name" => p.name = v.to_string(),
                            "path" => p.store_path = PathBuf::from(v),
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

/// Read the pointer under `cfg_root`. `Ok(None)` if absent (never-rebuilt or
/// deliberately removed — the `.bak` fallback case, 10 §4).
pub fn read_pointer(cfg_root: &Path) -> io::Result<Option<Pointer>> {
    let s = match fs::read_to_string(cfg_root.join(POINTER_FILE)) {
        Ok(s) => s,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let mut lines = s.lines();
    let bad = || io::Error::new(io::ErrorKind::InvalidData, "malformed current.pointer (10 §2)");
    let prism = lines.next().ok_or_else(bad)?.to_string();
    let selector = lines.next().ok_or_else(bad)?.to_string();
    let generation = lines
        .next()
        .and_then(|l| l.trim().parse().ok())
        .ok_or_else(bad)?;
    Ok(Some(Pointer { prism, selector, generation }))
}

/// Write the pointer atomically (temp + rename, trailing newline — 10 §2:
/// `shade os rebuild` rewrites all three lines atomically on success).
pub fn write_pointer(cfg_root: &Path, p: &Pointer) -> io::Result<()> {
    let bytes = format!("{}\n{}\n{}\n", p.prism, p.selector, p.generation);
    write_atomic(&cfg_root.join(POINTER_FILE), bytes.as_bytes())
}

/// Re-pin the pointer's generation (line 3 only; source lines untouched), if
/// a pointer exists. System-line **rollback** calls this so the next boot
/// activates the generation the user just rolled back to — without it, boot
/// would return to the pre-rollback pin. A rollback produces a built
/// generation like any rebuild, so re-pinning preserves the 10 §6 invariant
/// (boot still activates a pre-built generation, never a source prism).
pub fn repin_generation(cfg_root: &Path, generation: u64) -> io::Result<()> {
    if let Some(mut p) = read_pointer(cfg_root)? {
        p.generation = generation;
        write_pointer(cfg_root, &p)?;
    }
    Ok(())
}

// ---- The generation line --------------------------------------------------------

/// One generation line — `/shade/gen/system/` or `/shade/gen/users/<user>/`
/// (02 §5): its own monotonic counter, its own `current` symlink, its own
/// append-only history. Cheap to construct; holds no open handles.
#[derive(Debug, Clone)]
pub struct GenLine {
    line_root: PathBuf,
    /// Root-name tag: `system` or `user-<user>` — generation roots register
    /// as `/shade/roots/gen-<tag>-<N>-<i>` (store-db-gc §3.1 `<owner>-<label>`).
    tag: String,
    db: StoreDb,
}

impl GenLine {
    /// The system line `<shade_root>/gen/system` (built by `shade os rebuild`,
    /// privileged; 07 §2.1).
    pub fn system(shade_root: impl AsRef<Path>) -> Self {
        let r = shade_root.as_ref();
        GenLine {
            line_root: r.join("gen").join("system"),
            tag: "system".to_string(),
            db: StoreDb::new(r),
        }
    }

    /// A per-user line `<shade_root>/gen/users/<user>` (built by
    /// `shade home rebuild`, unprivileged; 07 §2.2, 10 §5).
    pub fn user(shade_root: impl AsRef<Path>, user: &str) -> Self {
        let r = shade_root.as_ref();
        GenLine {
            line_root: r.join("gen").join("users").join(user),
            tag: format!("user-{user}"),
            db: StoreDb::new(r),
        }
    }

    pub fn line_root(&self) -> &Path {
        &self.line_root
    }

    fn gen_dir(&self, n: u64) -> PathBuf {
        self.line_root.join(n.to_string())
    }

    /// A generation is complete iff its manifest and profile exist. `create`
    /// renames a fully-built, fsynced tree into place, so an incomplete
    /// numbered directory can only be corruption — never activated.
    pub fn is_complete(&self, n: u64) -> bool {
        let d = self.gen_dir(n);
        d.join("manifest").exists() && d.join("profile").exists()
    }

    /// The generation `current` points at, if the symlink exists and parses.
    pub fn current(&self) -> io::Result<Option<u64>> {
        match fs::read_link(self.line_root.join("current")) {
            Ok(t) => Ok(t.to_string_lossy().parse().ok()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// The numbered generations present in this line, ascending.
    pub fn numbers(&self) -> io::Result<Vec<u64>> {
        let mut out = Vec::new();
        let rd = match fs::read_dir(&self.line_root) {
            Ok(rd) => rd,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(out),
            Err(e) => return Err(e),
        };
        for e in rd {
            if let Ok(n) = e?.file_name().to_string_lossy().parse() {
                out.push(n);
            }
        }
        out.sort_unstable();
        Ok(out)
    }

    /// The newest complete generation, if any — boot's last-good fallback
    /// (10 §6, 02 §6.2).
    pub fn latest_complete(&self) -> io::Result<Option<u64>> {
        Ok(self
            .numbers()?
            .into_iter()
            .rev()
            .find(|&n| self.is_complete(n)))
    }

    /// List generations with their manifests, ascending, `current` marked —
    /// `shade generations list` (07 §2).
    pub fn list(&self) -> io::Result<Vec<GenInfo>> {
        let current = self.current()?;
        let mut out = Vec::new();
        for n in self.numbers()? {
            if let Some(manifest) = self.read_manifest(n)? {
                out.push(GenInfo { number: n, manifest, current: current == Some(n) });
            }
        }
        Ok(out)
    }

    /// The manifest of generation `n`, if present and well-formed.
    pub fn read_manifest(&self, n: u64) -> io::Result<Option<Manifest>> {
        match fs::read_to_string(self.gen_dir(n).join("manifest")) {
            Ok(s) => Ok(Manifest::parse(&s)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
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
    pub fn create(
        &self,
        packages: &[PackageEntry],
        lock: Option<&[u8]>,
        reason: &str,
        parent: u64,
    ) -> Result<u64, GenError> {
        fs::create_dir_all(&self.line_root)?;
        // Allocate-and-rename loop: two concurrent creators may pick the same
        // number; rename onto an existing directory fails, and the loser
        // retries with the next one. Numbers stay monotonic, never reused.
        loop {
            let n = self.numbers()?.last().copied().unwrap_or(0) + 1;
            let tmp = self
                .line_root
                .join(format!(".tmp-gen-{n}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&tmp);

            let built = (|| -> Result<(), GenError> {
                fs::create_dir_all(&tmp)?;
                let manifest = Manifest {
                    created: now_unix(),
                    parent,
                    reason: reason.to_string(),
                    packages: packages.to_vec(),
                };
                write_file_synced(&tmp.join("manifest"), manifest.serialize().as_bytes())?;
                // The lockfile snapshot that produced this generation; the
                // unified schema is deferred (shade 08 §5), so absent is legal.
                write_file_synced(
                    &tmp.join("prism.lock"),
                    lock.unwrap_or(b"# no prism.lock (unified lock schema deferred, shade 08 \xc2\xa75)\n"),
                )?;
                let profile = tmp.join("profile");
                fs::create_dir_all(&profile)?;
                for p in packages {
                    merge_into_profile(&profile, &p.store_path, &p.store_path, &p.name)?;
                }
                fsync_dir(&tmp)?;
                Ok(())
            })();
            if let Err(e) = built {
                let _ = fs::remove_dir_all(&tmp);
                return Err(e);
            }

            match fs::rename(&tmp, self.gen_dir(n)) {
                Ok(()) => {
                    fsync_dir(&self.line_root)?;
                    // Root registration (roots API seam): one direct root per
                    // package store path. The GC additionally byte-scans
                    // /shade/gen/ (store-db-gc §3.3) — belt and braces; the
                    // over-approximation direction is the safe one.
                    for (i, p) in packages.iter().enumerate() {
                        self.db
                            .add_root(&format!("gen-{}-{n}-{i}", self.tag), &p.store_path)?;
                    }
                    return Ok(n);
                }
                Err(_) if self.gen_dir(n).exists() => {
                    // Lost the allocation race; retry with a fresh number.
                    let _ = fs::remove_dir_all(&tmp);
                    continue;
                }
                Err(e) => {
                    let _ = fs::remove_dir_all(&tmp);
                    return Err(GenError::Io(e));
                }
            }
        }
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
    pub fn activate(&self, n: u64) -> Result<(), GenError> {
        if !self.gen_dir(n).exists() {
            return Err(GenError::NoSuchGeneration(n));
        }
        if !self.is_complete(n) {
            return Err(GenError::Incomplete(n));
        }
        let tmp = self.line_root.join(".current.new");
        let _ = fs::remove_file(&tmp);
        symlink(Path::new(&n.to_string()), &tmp)?;
        fs::rename(&tmp, self.line_root.join("current"))?;
        fsync_dir(&self.line_root)?;
        Ok(())
    }

    /// Wire the live view: `link -> <line>/current/profile/bin` — the single
    /// symlink everything else dereferences through (`/lth/bin` for the
    /// system line, 02 §6.1). Idempotent and atomic (temp + rename); the
    /// target string goes through `current`, so subsequent [`activate`] flips
    /// retarget the view with no further writes here.
    pub fn wire_view(&self, link: &Path) -> io::Result<()> {
        if let Some(parent) = link.parent() {
            fs::create_dir_all(parent)?;
        }
        let target = self.line_root.join("current").join("profile").join("bin");
        let tmp = link.with_file_name(".view.new");
        let _ = fs::remove_file(&tmp);
        symlink(&target, &tmp)?;
        fs::rename(&tmp, link)?;
        Ok(())
    }

    /// Roll back: create a **new** generation whose manifest (and lock) copy
    /// generation `target`'s (default: the generation before `current`), then
    /// activate it. History stays linear and append-only — rollback twice
    /// returns to where you started (02 §5, 07 §`shade rollback`).
    pub fn rollback(&self, target: Option<u64>) -> Result<u64, GenError> {
        let current = self.current()?.ok_or(GenError::NothingToRollBack)?;
        let target = match target {
            Some(t) => t,
            None => self
                .numbers()?
                .into_iter()
                .rev()
                .find(|&n| n < current)
                .ok_or(GenError::NothingToRollBack)?,
        };
        let manifest = self
            .read_manifest(target)?
            .ok_or(GenError::NoSuchGeneration(target))?;
        let lock = fs::read(self.gen_dir(target).join("prism.lock")).ok();
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

/// Boot-time activation of the **pre-built** system generation. Boot consumes
/// built generations, never source prisms (10 §6): this function takes no
/// evaluator, no builder, and no recipe — a boot-time build is
/// unrepresentable, which is how the no-build-at-boot invariant is enforced.
///
/// Order: the pointer's pinned generation (line 3, 10 §2) if complete; else
/// — pointer absent — whatever `current` already points at; else the newest
/// complete generation (last-good recovery); else [`GenError::NoGeneration`].
/// The activation itself is the same idempotent flip as any switch, plus
/// wiring the live view at `lth_bin` when given.
pub fn boot_activate(
    shade_root: &Path,
    cfg_root: &Path,
    lth_bin: Option<&Path>,
) -> Result<BootOutcome, GenError> {
    let line = GenLine::system(shade_root);
    let pinned = read_pointer(cfg_root)?.map(|p| p.generation);

    let (generation, fell_back) = match pinned {
        Some(n) if line.is_complete(n) => (n, false),
        Some(_) => (
            // Pinned generation missing/corrupt: last-good, never a rebuild
            // and never `.bak` (10 §4, 10 §6).
            line.latest_complete()?.ok_or(GenError::NoGeneration)?,
            true,
        ),
        None => match line.current()? {
            Some(c) if line.is_complete(c) => (c, false),
            _ => (line.latest_complete()?.ok_or(GenError::NoGeneration)?, true),
        },
    };

    line.activate(generation)?;
    if let Some(link) = lth_bin {
        line.wire_view(link)?;
    }
    Ok(BootOutcome { generation, pinned, fell_back })
}

// ---- Profile symlink forest -------------------------------------------------------

/// Merge one package's output tree into the profile: directories merge,
/// every file/symlink becomes a profile symlink to its absolute store path.
/// A leaf that already exists is a collision — an error at generation-build
/// time (02 §5), naming the file and the losing package.
fn merge_into_profile(
    dst: &Path,
    pkg_root: &Path,
    src: &Path,
    pkg_name: &str,
) -> Result<(), GenError> {
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        let ft = fs::symlink_metadata(&src_path)?.file_type();
        if ft.is_dir() {
            fs::create_dir_all(&dst_path)?;
            merge_into_profile(&dst_path, pkg_root, &src_path, pkg_name)?;
        } else {
            if fs::symlink_metadata(&dst_path).is_ok() {
                let rel = src_path
                    .strip_prefix(pkg_root)
                    .unwrap_or(&src_path)
                    .to_string_lossy()
                    .into_owned();
                let existing_target = fs::read_link(&dst_path)
                    .map(|t| t.to_string_lossy().into_owned())
                    .unwrap_or_else(|_| "<non-symlink>".to_string());
                return Err(GenError::Collision { rel, package: pkg_name.to_string(), existing_target });
            }
            symlink(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

// ---- Small helpers ----------------------------------------------------------------

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
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

/// Write + fsync a file (no rename — callers build inside a temp tree that is
/// renamed as a whole).
fn write_file_synced(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut f = fs::File::create(path)?;
    f.write_all(bytes)?;
    f.sync_all()
}

/// Atomic write: temp sibling + rename + best-effort dir fsync.
fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let tmp = parent.join(format!(".tmp-ptr-{}", std::process::id()));
    write_file_synced(&tmp, bytes)?;
    fs::rename(&tmp, path)?;
    fsync_dir(parent)
}

/// Best-effort directory fsync (forces the rename/commit durable, 02 §6.3).
fn fsync_dir(dir: &Path) -> io::Result<()> {
    if let Ok(f) = fs::File::open(dir) {
        let _ = f.sync_all();
    }
    Ok(())
}

#[cfg(unix)]
fn symlink(target: &Path, link: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}
#[cfg(not(unix))]
fn symlink(_target: &Path, _link: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "generation symlinks require a unix host",
    ))
}

#[cfg(test)]
mod tests;
