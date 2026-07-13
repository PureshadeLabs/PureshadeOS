//! shade-store-db — the store metadata database, GC roots, and mark-and-sweep
//! garbage collector (docs/shade-pkg/02-store.md §7, docs/shade/store-db-gc.md).
//!
//! [`shade_store`] (track 1) realizes immutable, input-addressed store paths.
//! This crate records **what was realized and what it references**
//! (`/shade/db/`), tracks the **live set** (`/shade/roots/` + in-flight build
//! locks), and reclaims everything unreachable (`shade gc`).
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
//! Mutations serialize on `db/lock` (exclusive-create — the flock-equivalent
//! 02 §7.2 calls for). On target the backing primitive is `SYS_CREATE`:
//! atomic create-if-absent, exactly one winner, losers get `EEXIST`
//! (docs/spec/syscalls.md, SYS_CREATE exclusive-create guarantee; verified
//! by the `make kernel-tests` exclusive-create boot probe). The host seed
//! uses `OpenOptions::create_new` — same semantics.
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
//! because RFS has none.

use std::collections::{BTreeSet, VecDeque};
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use shade_cdf::BASE32_ALPHABET;

/// The canonical `/shade` prefix (02 §1). [`StoreDb`] takes its roots as
/// arguments so host tests and tooling can target elsewhere; this is the
/// production value.
pub const CANONICAL_SHADE_ROOT: &str = "/shade";

/// Record-format header line (mirrors CDF's `shade-drv=1`): bumped on any
/// change to the `db/valid` record shape.
const DB_RECORD_VERSION: &str = "shade-db=1";

/// The store metadata database rooted at a `/shade` prefix. Cheap to
/// construct; holds no open handles.
#[derive(Debug, Clone)]
pub struct StoreDb {
    store_root: PathBuf,
    db_root: PathBuf,
    roots_root: PathBuf,
    gen_root: PathBuf,
    log_root: PathBuf,
}

impl StoreDb {
    /// A `StoreDb` over a `/shade` prefix: `store/`, `db/`, `roots/`, `gen/`,
    /// `log/` are the canonical 02 §1 subdirectories under `shade_root`.
    pub fn new(shade_root: impl AsRef<Path>) -> Self {
        let r = shade_root.as_ref();
        StoreDb {
            store_root: r.join("store"),
            db_root: r.join("db"),
            roots_root: r.join("roots"),
            gen_root: r.join("gen"),
            log_root: r.join("log"),
        }
    }

    /// Derive the sibling `db/`, `roots/`, `gen/`, `log/` roots from an
    /// explicit store root (its parent is the `/shade` prefix). The build
    /// executor's registrar uses this — it already threads `store_root`.
    pub fn for_store_root(store_root: impl AsRef<Path>) -> Self {
        let store_root = store_root.as_ref().to_path_buf();
        let shade = store_root
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from(CANONICAL_SHADE_ROOT));
        StoreDb {
            store_root,
            db_root: shade.join("db"),
            roots_root: shade.join("roots"),
            gen_root: shade.join("gen"),
            log_root: shade.join("log"),
        }
    }

    pub fn store_root(&self) -> &Path {
        &self.store_root
    }
    pub fn roots_dir(&self) -> &Path {
        &self.roots_root
    }

    fn refs_dir(&self) -> PathBuf {
        self.db_root.join("refs")
    }
    fn valid_dir(&self) -> PathBuf {
        self.db_root.join("valid")
    }
    fn locks_dir(&self) -> PathBuf {
        self.db_root.join("locks")
    }
    fn lock_file(&self) -> PathBuf {
        self.db_root.join("lock")
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
        out_path: &Path,
        digest: &str,
        store_name: &str,
        cdf_hash: &str,
        declared_refs: &[String],
    ) -> io::Result<ValidRecord> {
        let _lock = self.acquire_lock()?;

        // Reference scan (Nix-style): find every store-path digest the output
        // bytes embed — catches paths the compiler baked into binaries,
        // panic strings, or `env!`-captured values that no declaration names.
        let mut refs = self.scan_tree_for_digests(out_path)?;
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
        write_atomic(&self.refs_dir().join(digest), refs_buf.as_bytes())?;
        // valid/<digest>: the registration record (existence = valid).
        write_atomic(&self.valid_dir().join(digest), record.serialize().as_bytes())?;

        Ok(record)
    }

    /// The `db/refs/<digest>` set (empty if unregistered or record-less).
    pub fn read_refs(&self, digest: &str) -> io::Result<Vec<String>> {
        match fs::read_to_string(self.refs_dir().join(digest)) {
            Ok(s) => Ok(s.lines().map(str::to_string).filter(|l| !l.is_empty()).collect()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(e),
        }
    }

    /// The `db/valid/<digest>` record, if the path is registered valid.
    pub fn read_valid(&self, digest: &str) -> io::Result<Option<ValidRecord>> {
        match fs::read_to_string(self.valid_dir().join(digest)) {
            Ok(s) => Ok(ValidRecord::parse(digest, &s)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    pub fn is_valid(&self, digest: &str) -> bool {
        self.valid_dir().join(digest).exists()
    }

    // ---- Roots (02 §7.1) --------------------------------------------------

    /// Add a **direct** GC root: a symlink `roots/<name> -> store_path`.
    /// Anyone may root a path (02 §7.1 rule 2); the name convention is
    /// `<owner>-<label>`. Replaces an existing root of the same name.
    pub fn add_root(&self, name: &str, store_path: &Path) -> io::Result<()> {
        fs::create_dir_all(&self.roots_root)?;
        let link = self.roots_root.join(name);
        let _ = fs::remove_file(&link);
        symlink(store_path, &link)
    }

    /// Remove a direct root. Absent is not an error.
    pub fn remove_root(&self, name: &str) -> io::Result<()> {
        match fs::remove_file(self.roots_root.join(name)) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// Direct roots as `(name, target)` — dangling symlinks excluded (they are
    /// pruned by [`gc`](StoreDb::gc), not here).
    pub fn list_roots(&self) -> io::Result<Vec<(String, PathBuf)>> {
        let mut out = Vec::new();
        let rd = match fs::read_dir(&self.roots_root) {
            Ok(rd) => rd,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(out),
            Err(e) => return Err(e),
        };
        for e in rd {
            let e = e?;
            if let Ok(target) = fs::read_link(e.path()) {
                let name = e.file_name().to_string_lossy().into_owned();
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
    pub fn lock_build(&self, id: &str, keep: &[impl AsRef<str>]) -> io::Result<BuildLock> {
        fs::create_dir_all(self.locks_dir())?;
        let mut buf = String::new();
        for k in keep {
            if let Some(d) = digest_from_store_path(k.as_ref()) {
                buf.push_str(d);
                buf.push('\n');
            }
        }
        let path = self.locks_dir().join(id);
        write_atomic(&path, buf.as_bytes())?;
        Ok(BuildLock { path })
    }

    /// Number of build locks currently held (builds in flight).
    pub fn builds_in_flight(&self) -> usize {
        count_entries(&self.locks_dir())
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
    pub fn gc(&self, opts: &GcOptions) -> io::Result<GcReport> {
        let _lock = self.acquire_lock()?;

        let inflight = self.builds_in_flight();
        if inflight > 0 && !opts.force {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                format!(
                    "{inflight} build(s) in flight (/shade/db/locks non-empty); \
                     re-run with force to override"
                ),
            ));
        }

        // MARK: closure of the roots over db/refs.
        let (roots, pruned_roots) = self.collect_roots()?;
        let marked = self.mark_closure(roots)?;

        // SWEEP: every store entry whose digest is not marked, or whose name
        // is not a valid store name, is dead.
        let mut report = GcReport {
            collected: Vec::new(),
            kept: 0,
            freed_bytes: 0,
            pruned_roots,
            dry_run: opts.dry_run,
        };
        let rd = match fs::read_dir(&self.store_root) {
            Ok(rd) => rd,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(report),
            Err(e) => return Err(e),
        };
        for e in rd {
            let e = e?;
            let name = e.file_name().to_string_lossy().into_owned();
            let path = e.path();
            let live = match store_entry_digest(&name) {
                Some(d) => marked.contains(d),
                None => false, // grammar violation ⇒ dead (02 §7.3 step 3)
            };
            if live {
                report.kept += 1;
                continue;
            }
            report.freed_bytes += entry_size(&path);
            report.collected.push(name.clone());
            if !opts.dry_run {
                remove_path(&path)?;
                // Drop the db records + log of the reclaimed digest. Both the
                // output dir and its `.drv` map to one digest; either entry
                // reaching here removes the (idempotent) shared records.
                if let Some(d) = store_entry_digest(&name) {
                    let _ = fs::remove_file(self.refs_dir().join(d));
                    let _ = fs::remove_file(self.valid_dir().join(d));
                }
                let _ = fs::remove_file(self.log_root.join(format!("{name}.log")));
            }
        }
        report.collected.sort();
        Ok(report)
    }

    /// The digests of every root (02 §7.1), plus the count of dangling direct
    /// roots pruned as a side effect (rule 2).
    fn collect_roots(&self) -> io::Result<(BTreeSet<String>, usize)> {
        let mut roots = BTreeSet::new();
        let mut pruned = 0usize;

        // 1. Direct roots: roots/* symlinks. A target that no longer exists is
        //    a dangling root — pruned.
        if let Ok(rd) = fs::read_dir(&self.roots_root) {
            for e in rd {
                let e = e?;
                let link = e.path();
                let target = match fs::read_link(&link) {
                    Ok(t) => t,
                    Err(_) => continue, // not a symlink; leave it be
                };
                let abs = if target.is_absolute() {
                    target.clone()
                } else {
                    self.roots_root.join(&target)
                };
                if abs.exists() {
                    if let Some(d) = digest_from_store_path(&target.to_string_lossy()) {
                        roots.insert(d.to_string());
                    }
                } else {
                    let _ = fs::remove_file(&link);
                    pruned += 1;
                }
            }
        }

        // 2. Indirect roots: build locks. Each lock file lists digests.
        if let Ok(rd) = fs::read_dir(self.locks_dir()) {
            for e in rd {
                let e = e?;
                if let Ok(content) = fs::read_to_string(e.path()) {
                    for line in content.lines() {
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
        let gen_digests = self.scan_tree_for_digests(&self.gen_root)?;
        roots.extend(gen_digests);

        Ok((roots, pruned))
    }

    /// BFS the reference closure of `roots` over `db/refs`.
    fn mark_closure(&self, roots: BTreeSet<String>) -> io::Result<BTreeSet<String>> {
        let mut marked = BTreeSet::new();
        let mut queue: VecDeque<String> = roots.into_iter().collect();
        while let Some(d) = queue.pop_front() {
            if !marked.insert(d.clone()) {
                continue;
            }
            for r in self.read_refs(&d)? {
                if !marked.contains(&r) {
                    queue.push_back(r);
                }
            }
        }
        Ok(marked)
    }

    /// Recursively scan a tree for embedded store-path digests: read regular
    /// files and scan their bytes, and scan symlink targets, for the byte
    /// pattern `<store_root>/` followed by a 32-char base32 digest. Missing
    /// root ⇒ empty set.
    fn scan_tree_for_digests(&self, root: &Path) -> io::Result<BTreeSet<String>> {
        let mut set = BTreeSet::new();
        let prefix = {
            let mut s = self.store_root.as_os_str().to_os_string();
            s.push("/");
            s.into_string()
                .unwrap_or_else(|_| format!("{}/", self.store_root.display()))
                .into_bytes()
        };
        scan_tree(root, &prefix, &mut set)?;
        Ok(set)
    }

    // ---- Locking ----------------------------------------------------------

    /// Acquire the db mutation lock (`db/lock`) by exclusive-create. Spins
    /// briefly on contention (the lock is held only for short mutations),
    /// then fails rather than blocking forever on a stale lock.
    fn acquire_lock(&self) -> io::Result<DbLock> {
        fs::create_dir_all(&self.db_root)?;
        let path = self.lock_file();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match fs::OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(mut f) => {
                    let _ = writeln!(f, "{}", std::process::id());
                    return Ok(DbLock { path });
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                    if Instant::now() >= deadline {
                        return Err(io::Error::new(
                            io::ErrorKind::WouldBlock,
                            "db lock held (another store mutation in progress)",
                        ));
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(e) => return Err(e),
            }
        }
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
/// the build's kept-alive set is released when the build finishes.
#[derive(Debug)]
pub struct BuildLock {
    path: PathBuf,
}

impl BuildLock {
    /// Release the lock now (equivalent to dropping it).
    pub fn release(self) {}
}

impl Drop for BuildLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// The db mutation lock guard; removes `db/lock` on drop.
struct DbLock {
    path: PathBuf,
}
impl Drop for DbLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
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

/// Recursively scan a tree: byte-scan regular files, scan symlink targets.
fn scan_tree(path: &Path, prefix: &[u8], set: &mut BTreeSet<String>) -> io::Result<()> {
    let meta = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    let ft = meta.file_type();
    if ft.is_symlink() {
        if let Ok(target) = fs::read_link(path) {
            scan_bytes(prefix, target.to_string_lossy().as_bytes(), set);
        }
    } else if ft.is_dir() {
        for entry in fs::read_dir(path)? {
            scan_tree(&entry?.path(), prefix, set)?;
        }
    } else {
        // Regular file: stream it so a large binary does not balloon memory.
        let mut f = fs::File::open(path)?;
        let mut buf = Vec::new();
        f.read_to_end(&mut buf)?;
        scan_bytes(prefix, &buf, set);
    }
    Ok(())
}

/// Best-effort on-media size of a store entry (file or tree); symlinks and
/// unreadable entries count as 0.
fn entry_size(path: &Path) -> u64 {
    let meta = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(_) => return 0,
    };
    let ft = meta.file_type();
    if ft.is_symlink() {
        0
    } else if ft.is_dir() {
        let mut total = 0;
        if let Ok(rd) = fs::read_dir(path) {
            for e in rd.flatten() {
                total += entry_size(&e.path());
            }
        }
        total
    } else {
        meta.len()
    }
}

/// Remove a store entry, dir or file.
fn remove_path(path: &Path) -> io::Result<()> {
    let meta = fs::symlink_metadata(path)?;
    if meta.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
}

fn count_entries(dir: &Path) -> usize {
    fs::read_dir(dir).map(|rd| rd.flatten().count()).unwrap_or(0)
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Atomic write: temp file on the same directory, then rename; fsync the file
/// and best-effort the directory (durable, crash never leaves a partial).
fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp = parent.join(format!(".tmp-db-{}-{n}", std::process::id()));
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    if let Ok(f) = fs::File::open(parent) {
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
        "GC root symlinks require a unix host",
    ))
}

#[cfg(test)]
mod tests;
