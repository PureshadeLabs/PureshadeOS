//! shade-store-db — the store metadata database, GC roots, and mark-and-sweep
//! garbage collector (docs/shade-pkg/02-store.md §7, docs/shade/store-db-gc.md).
//!
//! [`shade_store`] (track 1) realizes immutable, input-addressed store paths.
//! This crate records **what was realized and what it references**
//! (`/shade/db/`), tracks the **live set** (`/shade/roots/` + in-flight build
//! locks), and reclaims everything unreachable (`shade gc`).
//!
//! ## The filesystem seam
//!
//! All I/O goes through the injected [`StoreFs`] backend (the B1 seam from
//! [`shade_store`]) — the crate core is `no_std + alloc` and touches no
//! filesystem directly. [`HostFs`](shade_store::HostFs) (feature `std`,
//! default) backs the host suite and the `shade-gc` seed CLI;
//! [`OrosFs`](shade_store::OrosFs) (feature `oros`) backs the same logic on
//! the Lythos ABI. Paths are plain `/`-separated strings on both sides.
//!
//! ## The database (02 §7.2)
//!
//! A plain directory-of-files DB — no binary format, no TOML. Per store
//! digest:
//! - `db/valid/<digest>` — a `key=value` registration record (line format,
//!   same shape as CDF): full BLAKE3 of the `.drv`, registration time, output
//!   store-name, deriver. Its existence *is* the "valid" marker (02 §2 —
//!   immutable once registered valid).
//! - `db/refs/<digest>` — the referenced store digests, one per line
//!   (LF-separated). Union of the derivation's declared `dep.*` and the
//!   digests found by [reference scanning](StoreDb::register) the output.
//!
//! Mutations serialize on `db/lock`, taken by the seam's
//! [`create_exclusive`](StoreFs::create_exclusive) — atomic create-if-absent,
//! exactly one winner, losers get `Exists` (the flock-equivalent 02 §7.2
//! calls for). On target the backing primitive is `SYS_CREATE`'s
//! exclusive-create guarantee (docs/spec/syscalls.md; verified by the `make
//! kernel-tests` exclusive-create boot probe); on the host it is
//! `OpenOptions::create_new` — same semantics.
//!
//! ## The live set (02 §7.1)
//!
//! GC keeps the transitive closure (over `db/refs`) of:
//! 1. **Direct roots** — symlinks under `/shade/roots/` into the store. Anyone
//!    may root a path; dangling symlinks are pruned.
//! 2. **Indirect roots** — build locks under `/shade/db/locks/`: an in-flight
//!    build lists the digests (its inputs + its in-progress output) it needs
//!    kept alive. Held for the build's duration, removed on completion.
//! 3. **Generations** — every store digest embedded anywhere under
//!    `/shade/gen/` (manifests + profile symlink forests), byte-scanned so no
//!    installed generation is collected.
//!
//! ## GC (02 §7.3)
//!
//! `shade gc`: take the db lock, refuse if builds are in flight (unless
//! forced), **mark** the closure of the roots, **sweep** every store entry not
//! marked (plus every entry violating the 02 §2 grammar) and its db records.
//! Deletion frees host blocks directly; on the OROS RFS the unlink reclaims
//! blocks at the next mount-time mark-and-sweep (RFS SPACE-1: a block is free
//! iff reachable from no valid superblock) — GC never touches a free structure
//! because RFS has none. The OROS backend has no rmdir syscall yet, so a
//! swept directory's empty skeleton survives until the ABI grows one; its
//! files (the bytes) are reclaimed.

#![cfg_attr(not(any(test, feature = "std")), no_std)]

extern crate alloc;

use alloc::collections::{BTreeSet, VecDeque};
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use shade_cdf::BASE32_ALPHABET;
use shade_store::backend::{self, join, split_parent};
use shade_store::{FsError, NodeKind, StoreFs};

#[cfg(feature = "std")]
pub use shade_store::HostFs;
#[cfg(feature = "oros")]
pub use shade_store::OrosFs;

/// The canonical `/shade` prefix (02 §1). [`StoreDb`] takes its roots as
/// arguments so host tests and tooling can target elsewhere; this is the
/// production value.
pub const CANONICAL_SHADE_ROOT: &str = "/shade";

/// Record-format header line (mirrors CDF's `shade-drv=1`): bumped on any
/// change to the `db/valid` record shape.
const DB_RECORD_VERSION: &str = "shade-db=1";

/// How long [`acquire_lock`] spins on a held `db/lock` before giving up
/// (the lock is held only for short mutations).
const LOCK_DEADLINE_MS: u64 = 10_000;

/// Database error: a seam failure tagged with the operation and path, or a
/// busy condition (lock held / builds in flight).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DbError {
    /// A backend filesystem operation failed.
    Fs {
        op: &'static str,
        path: String,
        err: FsError,
    },
    /// The db is busy: the mutation lock stayed held past the deadline, or
    /// GC was refused because builds are in flight (re-run with force).
    Busy(String),
}

impl core::fmt::Display for DbError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            DbError::Fs { op, path, err } => write!(f, "fs: {op} {path}: {err}"),
            DbError::Busy(msg) => write!(f, "busy: {msg}"),
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for DbError {}

/// Fold to `io::Error` so std consumers (`DbRegistrar`, shade-gen) keep their
/// `io::Result` plumbing: `Busy` → `WouldBlock` (the pre-seam kind).
#[cfg(feature = "std")]
impl From<DbError> for std::io::Error {
    fn from(e: DbError) -> Self {
        let kind = match &e {
            DbError::Busy(_) => std::io::ErrorKind::WouldBlock,
            DbError::Fs { err: FsError::NotFound, .. } => std::io::ErrorKind::NotFound,
            DbError::Fs { err: FsError::Exists, .. } => std::io::ErrorKind::AlreadyExists,
            DbError::Fs { .. } => std::io::ErrorKind::Other,
        };
        std::io::Error::new(kind, e.to_string())
    }
}

pub type DbResult<T> = core::result::Result<T, DbError>;

/// Shorthand: tag a backend failure with the operation and target path.
fn fs_op(op: &'static str, path: &str) -> impl FnOnce(FsError) -> DbError {
    let path = String::from(path);
    move |err| DbError::Fs { op, path, err }
}

/// The store metadata database rooted at a `/shade` prefix, over an injected
/// [`StoreFs`] backend. Cheap to construct; holds no open handles. The
/// backend lives in a `RefCell` so the query/mutation API stays `&self`
/// (backends are stateless — `HostFs`/`OrosFs` are `Copy`).
#[derive(Debug, Clone)]
pub struct StoreDb<F: StoreFs> {
    fs: core::cell::RefCell<F>,
    store_root: String,
    db_root: String,
    roots_root: String,
    gen_root: String,
    log_root: String,
}

#[cfg(feature = "std")]
impl StoreDb<HostFs> {
    /// A host-backed `StoreDb` over a `/shade` prefix: `store/`, `db/`,
    /// `roots/`, `gen/`, `log/` are the canonical 02 §1 subdirectories under
    /// `shade_root`.
    pub fn new(shade_root: impl AsRef<std::path::Path>) -> Self {
        StoreDb::with_backend(HostFs, &shade_root.as_ref().to_string_lossy())
    }

    /// Derive the sibling `db/`, `roots/`, `gen/`, `log/` roots from an
    /// explicit store root (its parent is the `/shade` prefix). The build
    /// executor's registrar uses this — it already threads `store_root`.
    pub fn for_store_root(store_root: impl AsRef<std::path::Path>) -> Self {
        StoreDb::for_store_root_on(HostFs, &store_root.as_ref().to_string_lossy())
    }
}

impl<F: StoreFs> StoreDb<F> {
    /// A `StoreDb` over a `/shade` prefix on the injected backend `fs`.
    pub fn with_backend(fs: F, shade_root: &str) -> Self {
        let r = shade_root.trim_end_matches('/');
        StoreDb {
            fs: core::cell::RefCell::new(fs),
            store_root: format!("{r}/store"),
            db_root: format!("{r}/db"),
            roots_root: format!("{r}/roots"),
            gen_root: format!("{r}/gen"),
            log_root: format!("{r}/log"),
        }
    }

    /// [`with_backend`](StoreDb::with_backend), but keyed on an explicit
    /// store root whose parent is the `/shade` prefix.
    pub fn for_store_root_on(fs: F, store_root: &str) -> Self {
        let store_root = store_root.trim_end_matches('/');
        let (shade, _) = split_parent(store_root);
        let mut db = StoreDb::with_backend(fs, shade);
        db.store_root = String::from(store_root);
        db
    }

    pub fn store_root(&self) -> &str {
        &self.store_root
    }
    pub fn roots_dir(&self) -> &str {
        &self.roots_root
    }

    fn refs_dir(&self) -> String {
        join(&self.db_root, "refs")
    }
    fn valid_dir(&self) -> String {
        join(&self.db_root, "valid")
    }
    fn locks_dir(&self) -> String {
        join(&self.db_root, "locks")
    }
    fn lock_file(&self) -> String {
        join(&self.db_root, "lock")
    }

    // ---- Registration (02 §7.2, 06 §5) ------------------------------------

    /// Register a realized output: reference-scan its tree, union with the
    /// derivation's declared `dep.*`, and write `db/refs/<digest>` +
    /// `db/valid/<digest>` under the db lock. Idempotent (immutable store ⇒
    /// re-registering yields the same records).
    ///
    /// - `out_path` — the realized output directory (under `store_root`).
    /// - `digest` — its 32-char store digest.
    /// - `store_name` — `<digest>-<name>-<version>`.
    /// - `cdf_hash` — full BLAKE3-256 of the `.drv`/CDF bytes, lowercase hex.
    /// - `declared_refs` — the derivation's `dep.*` store paths (canonical
    ///   `/shade/store/...`), the seed of the reference record.
    pub fn register(
        &self,
        out_path: &str,
        digest: &str,
        store_name: &str,
        cdf_hash: &str,
        declared_refs: &[String],
    ) -> DbResult<ValidRecord>
    where
        F: Clone,
    {
        let fs = &mut *self.fs.borrow_mut();
        let _lock = acquire_lock(fs, &self.db_root, &self.lock_file(), LOCK_DEADLINE_MS)?;

        // Reference scan (Nix-style): find every store-path digest the output
        // bytes embed — catches paths the compiler baked into binaries,
        // panic strings, or `env!`-captured values that no declaration names.
        let mut refs = BTreeSet::new();
        scan_tree(fs, out_path, &self.scan_prefix(), &mut refs)?;
        // Union the declared deps: the `.drv` references all its `dep.*`.
        for d in declared_refs {
            if let Some(dg) = digest_from_store_path(d) {
                refs.insert(dg.to_string());
            }
        }
        // A path never references itself.
        refs.remove(digest);
        let refs: Vec<String> = refs.into_iter().collect();

        let record = ValidRecord {
            digest: digest.to_string(),
            cdf_hash: cdf_hash.to_string(),
            registered: now_unix(),
            deriver: store_name.to_string(),
            name: store_name.to_string(),
            refs: refs.clone(),
        };

        // refs/<digest>: one referenced digest per line, trailing LF.
        let mut refs_buf = String::new();
        for r in &refs {
            refs_buf.push_str(r);
            refs_buf.push('\n');
        }
        write_atomic(fs, &join(&self.refs_dir(), digest), refs_buf.as_bytes())?;
        // valid/<digest>: the registration record (existence = valid).
        write_atomic(fs, &join(&self.valid_dir(), digest), record.serialize().as_bytes())?;

        Ok(record)
    }

    /// The `db/refs/<digest>` set (empty if unregistered or record-less).
    pub fn read_refs(&self, digest: &str) -> DbResult<Vec<String>> {
        let fs = &mut *self.fs.borrow_mut();
        read_refs_on(fs, &self.refs_dir(), digest)
    }

    /// The `db/valid/<digest>` record, if the path is registered valid.
    pub fn read_valid(&self, digest: &str) -> DbResult<Option<ValidRecord>> {
        let fs = &mut *self.fs.borrow_mut();
        let path = join(&self.valid_dir(), digest);
        match fs.read_file(&path) {
            Ok(bytes) => {
                let s = String::from_utf8_lossy(&bytes);
                Ok(ValidRecord::parse(digest, &s))
            }
            Err(FsError::NotFound) => Ok(None),
            Err(e) => Err(fs_op("read", &path)(e)),
        }
    }

    pub fn is_valid(&self, digest: &str) -> bool {
        let fs = &mut *self.fs.borrow_mut();
        fs.exists(&join(&self.valid_dir(), digest))
    }

    // ---- Roots (02 §7.1) --------------------------------------------------

    /// Add a **direct** GC root: a symlink `roots/<name> -> store_path`.
    /// Anyone may root a path (02 §7.1 rule 2); the name convention is
    /// `<owner>-<label>`. Replaces an existing root of the same name.
    pub fn add_root(&self, name: &str, store_path: &str) -> DbResult<()> {
        let fs = &mut *self.fs.borrow_mut();
        backend::create_dir_all(fs, &self.roots_root)
            .map_err(fs_op("create_dir_all", &self.roots_root))?;
        let link = join(&self.roots_root, name);
        let _ = fs.unlink(&link);
        fs.symlink(store_path, &link).map_err(fs_op("symlink", &link))
    }

    /// Remove a direct root. Absent is not an error.
    pub fn remove_root(&self, name: &str) -> DbResult<()> {
        let fs = &mut *self.fs.borrow_mut();
        let link = join(&self.roots_root, name);
        match fs.unlink(&link) {
            Ok(()) | Err(FsError::NotFound) => Ok(()),
            Err(e) => Err(fs_op("unlink", &link)(e)),
        }
    }

    /// Direct roots as `(name, target)` — dangling symlinks excluded (they are
    /// pruned by [`gc`](StoreDb::gc), not here).
    pub fn list_roots(&self) -> DbResult<Vec<(String, String)>> {
        let fs = &mut *self.fs.borrow_mut();
        let mut out = Vec::new();
        let entries = match fs.read_dir(&self.roots_root) {
            Ok(entries) => entries,
            Err(FsError::NotFound) => return Ok(out),
            Err(e) => return Err(fs_op("read_dir", &self.roots_root)(e)),
        };
        for (name, _) in entries {
            if let Ok(target) = fs.read_link(&join(&self.roots_root, &name)) {
                out.push((name, target));
            }
        }
        out.sort();
        Ok(out)
    }

    /// Acquire an **indirect** root: a build lock at `db/locks/<id>` naming
    /// every digest the in-flight build needs kept alive (its input closure +
    /// its in-progress output). The returned guard removes the lock on drop —
    /// so a live build's inputs are never collected (02 §7.1 rule 3), and a
    /// crash leaves a stale lock that a later run overwrites or GC ignores
    /// once the id is reused.
    pub fn lock_build(&self, id: &str, keep: &[impl AsRef<str>]) -> DbResult<BuildLock<F>>
    where
        F: Clone,
    {
        let fs = &mut *self.fs.borrow_mut();
        let locks = self.locks_dir();
        backend::create_dir_all(fs, &locks).map_err(fs_op("create_dir_all", &locks))?;
        let mut buf = String::new();
        for k in keep {
            if let Some(d) = digest_from_store_path(k.as_ref()) {
                buf.push_str(d);
                buf.push('\n');
            }
        }
        let path = join(&locks, id);
        write_atomic(fs, &path, buf.as_bytes())?;
        Ok(BuildLock { fs: fs.clone(), path })
    }

    /// Number of build locks currently held (builds in flight).
    pub fn builds_in_flight(&self) -> usize {
        let fs = &mut *self.fs.borrow_mut();
        fs.read_dir(&self.locks_dir()).map(|v| v.len()).unwrap_or(0)
    }

    // ---- GC (02 §7.3) -----------------------------------------------------

    /// Mark-and-sweep the store. Under the db lock: refuse if builds are in
    /// flight unless `force`; mark the closure of every root; sweep every
    /// store entry not in the mark set — plus every entry violating the 02 §2
    /// grammar — together with its `db/refs`, `db/valid`, and build log. With
    /// `dry_run`, computes the report without deleting anything.
    ///
    /// **Safety** (02 §7.3): the mark set is computed under the lock over an
    /// immutable store; in-flight builds hold locks whose digests are roots,
    /// so a live build's inputs are marked and its unregistered output lives
    /// under `build/`, not `store/`. References are a *superset* (declared ∪
    /// scanned), so reachability is never under-approximated — GC cannot
    /// collect a rooted or reference-reachable path.
    pub fn gc(&self, opts: &GcOptions) -> DbResult<GcReport>
    where
        F: Clone,
    {
        let fs = &mut *self.fs.borrow_mut();
        let _lock = acquire_lock(fs, &self.db_root, &self.lock_file(), LOCK_DEADLINE_MS)?;

        let inflight = fs.read_dir(&self.locks_dir()).map(|v| v.len()).unwrap_or(0);
        if inflight > 0 && !opts.force {
            return Err(DbError::Busy(format!(
                "{inflight} build(s) in flight (/shade/db/locks non-empty); \
                 re-run with force to override"
            )));
        }

        // MARK: closure of the roots over db/refs.
        let (roots, pruned_roots) = self.collect_roots(fs)?;
        let marked = self.mark_closure(fs, roots)?;

        // SWEEP: every store entry whose digest is not marked, or whose name
        // is not a valid store name, is dead.
        let mut report = GcReport {
            collected: Vec::new(),
            kept: 0,
            freed_bytes: 0,
            pruned_roots,
            dry_run: opts.dry_run,
        };
        let entries = match fs.read_dir(&self.store_root) {
            Ok(entries) => entries,
            Err(FsError::NotFound) => return Ok(report),
            Err(e) => return Err(fs_op("read_dir", &self.store_root)(e)),
        };
        for (name, _) in entries {
            let path = join(&self.store_root, &name);
            let live = match store_entry_digest(&name) {
                Some(d) => marked.contains(d),
                None => false, // grammar violation ⇒ dead (02 §7.3 step 3)
            };
            if live {
                report.kept += 1;
                continue;
            }
            report.freed_bytes += entry_size(fs, &path);
            report.collected.push(name.clone());
            if !opts.dry_run {
                // Remove the whole dead store path via the sole reclamation
                // primitive. On a realize-guarded store mount (OROS) this is
                // SYS_STORE_REMOVE — the kernel deletes the sealed tree BELOW
                // the seal (no in-place unseal exists); on host/mem it is a
                // plain recursive delete. Idempotent and correct on the `.drv`
                // (never sealed) too.
                let _ = fs.remove_store_path(&path);
                // Drop the db records + log of the reclaimed digest. Both the
                // output dir and its `.drv` map to one digest; either entry
                // reaching here removes the (idempotent) shared records.
                if let Some(d) = store_entry_digest(&name) {
                    let _ = fs.unlink(&join(&self.refs_dir(), d));
                    let _ = fs.unlink(&join(&self.valid_dir(), d));
                }
                let _ = fs.unlink(&join(&self.log_root, &format!("{name}.log")));
            }
        }
        report.collected.sort();
        Ok(report)
    }

    /// The digests of every root (02 §7.1), plus the count of dangling direct
    /// roots pruned as a side effect (rule 2).
    fn collect_roots(&self, fs: &mut F) -> DbResult<(BTreeSet<String>, usize)> {
        let mut roots = BTreeSet::new();
        let mut pruned = 0usize;

        // 1. Direct roots: roots/* symlinks. A target that no longer exists is
        //    a dangling root — pruned.
        if let Ok(entries) = fs.read_dir(&self.roots_root) {
            for (name, _) in entries {
                let link = join(&self.roots_root, &name);
                let target = match fs.read_link(&link) {
                    Ok(t) => t,
                    Err(_) => continue, // not a symlink; leave it be
                };
                let abs = if target.starts_with('/') {
                    target.clone()
                } else {
                    join(&self.roots_root, &target)
                };
                if fs.exists(&abs) {
                    if let Some(d) = digest_from_store_path(&target) {
                        roots.insert(d.to_string());
                    }
                } else {
                    let _ = fs.unlink(&link);
                    pruned += 1;
                }
            }
        }

        // 2. Indirect roots: build locks. Each lock file lists digests.
        if let Ok(entries) = fs.read_dir(&self.locks_dir()) {
            for (name, _) in entries {
                if let Ok(bytes) = fs.read_file(&join(&self.locks_dir(), &name)) {
                    for line in String::from_utf8_lossy(&bytes).lines() {
                        let l = line.trim();
                        if !l.is_empty() {
                            roots.insert(l.to_string());
                        }
                    }
                }
            }
        }

        // 3. Generations: every store digest embedded under /shade/gen —
        //    manifests' store-path lines and profile symlink forests. Keeps
        //    all installed generations live (02 §7.1 rule 1).
        scan_tree(fs, &self.gen_root, &self.scan_prefix(), &mut roots)?;

        Ok((roots, pruned))
    }

    /// BFS the reference closure of `roots` over `db/refs`.
    fn mark_closure(&self, fs: &mut F, roots: BTreeSet<String>) -> DbResult<BTreeSet<String>> {
        let refs_dir = self.refs_dir();
        let mut marked = BTreeSet::new();
        let mut queue: VecDeque<String> = roots.into_iter().collect();
        while let Some(d) = queue.pop_front() {
            if !marked.insert(d.clone()) {
                continue;
            }
            for r in read_refs_on(fs, &refs_dir, &d)? {
                if !marked.contains(&r) {
                    queue.push_back(r);
                }
            }
        }
        Ok(marked)
    }

    /// The reference-scan byte pattern: `<store_root>/` (followed by a
    /// 32-char base32 digest).
    fn scan_prefix(&self) -> Vec<u8> {
        format!("{}/", self.store_root).into_bytes()
    }
}

/// The `db/refs/<digest>` set on an explicit backend (empty if unregistered).
fn read_refs_on(fs: &mut dyn StoreFs, refs_dir: &str, digest: &str) -> DbResult<Vec<String>> {
    let path = join(refs_dir, digest);
    match fs.read_file(&path) {
        Ok(bytes) => Ok(String::from_utf8_lossy(&bytes)
            .lines()
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect()),
        Err(FsError::NotFound) => Ok(Vec::new()),
        Err(e) => Err(fs_op("read", &path)(e)),
    }
}

/// The `db/valid/<digest>` registration record (02 §7.2). References live in
/// the sibling `db/refs/<digest>`; [`refs`](ValidRecord::refs) mirrors them for
/// convenience after a [`register`](StoreDb::register).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidRecord {
    pub digest: String,
    /// Full BLAKE3-256 of the `.drv`/CDF bytes, lowercase hex (untruncated).
    pub cdf_hash: String,
    /// Registration time, unix seconds.
    pub registered: u64,
    /// The producing derivation's store-name (deriver link, 02 §7.2).
    pub deriver: String,
    /// The output store-name `<digest>-<name>-<version>`.
    pub name: String,
    /// The referenced digests recorded alongside in `db/refs/<digest>`.
    pub refs: Vec<String>,
}

impl ValidRecord {
    /// Serialize to the canonical `key=value` record (header first, then keys
    /// bytewise-sorted, one LF per line — the CDF line discipline).
    fn serialize(&self) -> String {
        format!(
            "{DB_RECORD_VERSION}\ncdf-hash={}\nderiver={}\nname={}\nregistered={}\n",
            self.cdf_hash, self.deriver, self.name, self.registered
        )
    }

    /// Parse a record for `digest` (refs left empty — read them from
    /// `db/refs`). `None` if the header is missing/unsupported.
    fn parse(digest: &str, s: &str) -> Option<ValidRecord> {
        let mut lines = s.lines();
        if lines.next()? != DB_RECORD_VERSION {
            return None;
        }
        let mut cdf_hash = String::new();
        let mut deriver = String::new();
        let mut name = String::new();
        let mut registered = 0u64;
        for line in lines {
            let (k, v) = line.split_once('=')?;
            match k {
                "cdf-hash" => cdf_hash = v.to_string(),
                "deriver" => deriver = v.to_string(),
                "name" => name = v.to_string(),
                "registered" => registered = v.parse().ok()?,
                _ => {} // unknown key: tolerate (forward-compat)
            }
        }
        Some(ValidRecord {
            digest: digest.to_string(),
            cdf_hash,
            registered,
            deriver,
            name,
            refs: Vec::new(),
        })
    }
}

/// Options for [`StoreDb::gc`].
#[derive(Debug, Clone, Copy, Default)]
pub struct GcOptions {
    /// Compute the report without deleting anything.
    pub dry_run: bool,
    /// Proceed even with builds in flight (02 §7.3 step 1 `--force`).
    pub force: bool,
}

/// The outcome of a [`gc`](StoreDb::gc) run.
#[derive(Debug, Clone, Default)]
pub struct GcReport {
    /// Store-names deleted (or that *would* be, under `dry_run`).
    pub collected: Vec<String>,
    /// Live store entries kept.
    pub kept: usize,
    /// Bytes reclaimed (best-effort tree size).
    pub freed_bytes: u64,
    /// Dangling direct roots pruned.
    pub pruned_roots: usize,
    /// Whether this was a dry run (nothing deleted).
    pub dry_run: bool,
}

/// A held build lock (indirect GC root). Dropping it removes the lock file, so
/// the build's kept-alive set is released when the build finishes. Carries its
/// own backend copy so the drop needs no `StoreDb` borrow.
#[derive(Debug)]
pub struct BuildLock<F: StoreFs> {
    fs: F,
    path: String,
}

impl<F: StoreFs> BuildLock<F> {
    /// Release the lock now (equivalent to dropping it).
    pub fn release(self) {}
}

impl<F: StoreFs> Drop for BuildLock<F> {
    fn drop(&mut self) {
        let _ = self.fs.unlink(&self.path);
    }
}

// ---- Locking ------------------------------------------------------------------

/// The db mutation lock guard; removes `db/lock` on drop. Owns a backend copy
/// so releasing needs no outer borrow.
#[derive(Debug)]
struct DbLock<F: StoreFs> {
    fs: F,
    path: String,
}
impl<F: StoreFs> Drop for DbLock<F> {
    fn drop(&mut self) {
        let _ = self.fs.unlink(&self.path);
    }
}

/// Acquire the db mutation lock (`db/lock`) by the seam's atomic
/// [`create_exclusive`](StoreFs::create_exclusive) — exactly one winner,
/// losers see `Exists` and spin briefly (the lock is held only for short
/// mutations), then fail [`DbError::Busy`] rather than blocking forever on a
/// stale lock.
fn acquire_lock<F: StoreFs + Clone>(
    fs: &mut F,
    db_root: &str,
    lock_path: &str,
    deadline_ms: u64,
) -> DbResult<DbLock<F>> {
    backend::create_dir_all(fs, db_root).map_err(fs_op("create_dir_all", db_root))?;
    let deadline = monotonic_ms().saturating_add(deadline_ms);
    loop {
        let body = format!("{}\n", fs.unique_token());
        match fs.create_exclusive(lock_path, body.as_bytes()) {
            Ok(()) => return Ok(DbLock { fs: fs.clone(), path: String::from(lock_path) }),
            Err(FsError::Exists) => {
                if monotonic_ms() >= deadline {
                    return Err(DbError::Busy(String::from(
                        "db lock held (another store mutation in progress)",
                    )));
                }
                backoff();
            }
            Err(e) => return Err(fs_op("create_exclusive", lock_path)(e)),
        }
    }
}

// ---- Free functions ---------------------------------------------------------

/// Extract the 32-char digest from a store path or store-name. Accepts a full
/// path (`/…/<digest>-<name>-<version>[.drv]`) or the bare final component;
/// returns `None` if the component is not a valid store name.
fn digest_from_store_path(s: &str) -> Option<&str> {
    let base = s.rsplit('/').next().unwrap_or(s);
    let base = base.strip_suffix(".drv").unwrap_or(base);
    store_entry_digest(base)
}

/// The digest of a store *entry name* (`<digest>-<name>-<version>` or its
/// `.drv`): the first 32 chars, iff they are all base32 and followed by `-`.
fn store_entry_digest(name: &str) -> Option<&str> {
    let bare = name.strip_suffix(".drv").unwrap_or(name);
    let b = bare.as_bytes();
    if b.len() < 33 || b[32] != b'-' {
        return None;
    }
    if b[..32].iter().all(|c| BASE32_ALPHABET.contains(c)) {
        Some(&bare[..32])
    } else {
        None
    }
}

/// Scan `buf` for `prefix` followed by 32 base32 chars; insert each digest.
fn scan_bytes(prefix: &[u8], buf: &[u8], set: &mut BTreeSet<String>) {
    let plen = prefix.len();
    if plen == 0 || buf.len() < plen + 32 {
        return;
    }
    let mut i = 0;
    let last = buf.len() - (plen + 32);
    while i <= last {
        if &buf[i..i + plen] == prefix {
            let cand = &buf[i + plen..i + plen + 32];
            if cand.iter().all(|c| BASE32_ALPHABET.contains(c)) {
                set.insert(String::from_utf8_lossy(cand).into_owned());
                i += plen + 32;
                continue;
            }
        }
        i += 1;
    }
}

/// Recursively scan a tree through the seam: byte-scan regular files, scan
/// symlink targets. Missing root ⇒ empty. A backend without readlink (OROS
/// today) contributes no symlink targets — and can hold none either.
fn scan_tree(
    fs: &mut dyn StoreFs,
    path: &str,
    prefix: &[u8],
    set: &mut BTreeSet<String>,
) -> DbResult<()> {
    let meta = match fs.metadata(path) {
        Ok(m) => m,
        Err(FsError::NotFound) => return Ok(()),
        Err(e) => return Err(fs_op("stat", path)(e)),
    };
    match meta.kind {
        NodeKind::Symlink => {
            if let Ok(target) = fs.read_link(path) {
                scan_bytes(prefix, target.as_bytes(), set);
            }
        }
        NodeKind::Dir => {
            let entries = fs.read_dir(path).map_err(fs_op("read_dir", path))?;
            for (name, _) in entries {
                scan_tree(fs, &join(path, &name), prefix, set)?;
            }
        }
        NodeKind::File | NodeKind::Other => {
            let buf = fs.read_file(path).map_err(fs_op("read", path))?;
            scan_bytes(prefix, &buf, set);
        }
    }
    Ok(())
}

/// Best-effort on-media size of a store entry (file or tree); symlinks and
/// unreadable entries count as 0.
fn entry_size(fs: &mut dyn StoreFs, path: &str) -> u64 {
    let meta = match fs.metadata(path) {
        Ok(m) => m,
        Err(_) => return 0,
    };
    match meta.kind {
        NodeKind::Symlink => 0,
        NodeKind::Dir => {
            let mut total = 0;
            if let Ok(entries) = fs.read_dir(path) {
                for (name, _) in entries {
                    total += entry_size(fs, &join(path, &name));
                }
            }
            total
        }
        NodeKind::File | NodeKind::Other => meta.len,
    }
}

/// Atomic write through the seam: temp file in the same directory, then
/// rename (durability is the backend's `write_file` contract + best-effort
/// `sync_dir` — crash never leaves a partial). A pre-existing destination is
/// replaced: where the backend's rename is no-replace (OROS RFS), the stale
/// record is unlinked first — safe under the db lock, and records are
/// idempotent re-registrations anyway.
fn write_atomic(fs: &mut dyn StoreFs, path: &str, bytes: &[u8]) -> DbResult<()> {
    let (parent, name) = split_parent(path);
    backend::create_dir_all(fs, parent).map_err(fs_op("create_dir_all", parent))?;
    let tmp = backend::temp_sibling(fs, parent, name, "db");
    fs.write_file(&tmp, bytes, false).map_err(fs_op("write", &tmp))?;
    match fs.rename(&tmp, path) {
        Ok(()) => {}
        Err(FsError::Exists) => {
            fs.unlink(path).map_err(fs_op("unlink", path))?;
            if let Err(e) = fs.rename(&tmp, path) {
                let _ = fs.unlink(&tmp);
                return Err(fs_op("rename", path)(e));
            }
        }
        Err(e) => {
            let _ = fs.unlink(&tmp);
            return Err(fs_op("rename", path)(e));
        }
    }
    let _ = fs.sync_dir(parent);
    Ok(())
}

// ---- Environment (clock + backoff) ------------------------------------------
//
// The only two ambient facts the db needs that the fs seam does not carry:
// wall-clock time (the `registered=` stamp) and a brief wait between lock
// retries. cfg'd per platform: std host, raw Lythos syscalls on target.

#[cfg(feature = "std")]
fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(feature = "std")]
fn monotonic_ms() -> u64 {
    use std::sync::OnceLock;
    use std::time::Instant;
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_millis() as u64
}

#[cfg(feature = "std")]
fn backoff() {
    std::thread::sleep(std::time::Duration::from_millis(5));
}

#[cfg(all(feature = "oros", not(feature = "std")))]
fn now_unix() -> u64 {
    // SYS_TIME_EPOCH: unix epoch milliseconds (CMOS-anchored).
    (unsafe { lythos_syscall::syscall0(lythos_abi::syscall::SYS_TIME_EPOCH) }) / 1000
}

#[cfg(all(feature = "oros", not(feature = "std")))]
fn monotonic_ms() -> u64 {
    // SYS_TIME: milliseconds since boot (APIC tick counter).
    unsafe { lythos_syscall::syscall0(lythos_abi::syscall::SYS_TIME) }
}

#[cfg(all(feature = "oros", not(feature = "std")))]
fn backoff() {
    unsafe {
        lythos_syscall::syscall0(lythos_abi::syscall::SYS_YIELD);
    }
}

// Featureless no_std fallback (keeps `--no-default-features` checkable):
// epoch 0, a counting pseudo-clock so lock deadlines still expire, no wait.
#[cfg(not(any(feature = "std", feature = "oros")))]
fn now_unix() -> u64 {
    0
}

#[cfg(not(any(feature = "std", feature = "oros")))]
fn monotonic_ms() -> u64 {
    use core::sync::atomic::{AtomicU64, Ordering};
    static TICKS: AtomicU64 = AtomicU64::new(0);
    TICKS.fetch_add(1, Ordering::Relaxed)
}

#[cfg(not(any(feature = "std", feature = "oros")))]
fn backoff() {}

#[cfg(test)]
mod tests;
