//! shade-store — input-addressed store realization.
//!
//! Realizes a CDF derivation ([`shade_cdf`]) into the store at
//! `/shade/store/<digest>-<name>-<version>` per `docs/shade-pkg/02-store.md`
//! §2 (path format) and §3 (input-addressing). This is **track 1**: the
//! digest, the path, and the idempotent atomic write. It does **not** run
//! builds — build dispatch (track 2) produces the output tree and hands it to
//! [`realize`] as a staged directory.
//!
//! ## Input-addressing and the output-path hole (02 §3, 03 §5.2)
//!
//! The digest is `BLAKE3-160(CDF)` over the **elided** CDF — the form that
//! carries no resolved output store path. In shade the elision is structural,
//! not a blanking pass: the concrete `$out` path can never be a hash input,
//! so recipes emit the **literal token `$out`** (03 §5.2) and the CDF only
//! ever contains that token. [`Derivation`] enforces this — it rejects any
//! attempt to bake a resolved store path into a hashed field (`out`,
//! `outpath`). The output path is the "hole": computed from the digest, then
//! backfilled by [`Derivation::resolve`] in one pass. Same recipe + same
//! resolved inputs ⇒ same elided CDF ⇒ same digest ⇒ same path.
//!
//! ## Realization contract (02 §2)
//!
//! Store paths are immutable once realized. [`realize`] therefore:
//! - stages the output tree into a temp dir on the same filesystem, then
//!   **atomically renames** it into `<out_path>` — a crash leaves only a temp
//!   dir, never a partial `<out_path>` (so "exists ⇒ complete");
//! - writes the `.drv` (the exact elided CDF bytes) atomically via
//!   temp-file + rename;
//! - is **idempotent**: if `<out_path>` already exists it is a no-op; it never
//!   overwrites (immutability), and it verifies any existing `.drv` matches.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use shade_cdf::{store_digest, CdfBuilder, CdfError};

/// The canonical store prefix (02 §2). [`realize`] takes a `store_root`
/// argument so tests and host tooling can target a directory other than the
/// real `/shade/store`; this constant is the production value.
pub const CANONICAL_STORE_ROOT: &str = "/shade/store";

/// Keys that name a *resolved* output store path. These are the "output-path
/// fields" that must be elided from the hash (02 §3, Nix's approach): a store
/// path can never be an input to its own digest. [`Derivation::insert`]
/// rejects them — recipes use the literal `$out` token instead (03 §5.2).
const ELIDED_OUTPUT_KEYS: &[&str] = &["out", "outpath", "output_path", "outputpath"];

#[derive(Debug)]
pub enum StoreError {
    /// A CDF-level problem (bad key/name/version, duplicate key).
    Cdf(CdfError),
    /// Attempt to insert a resolved output-path field into the hashed CDF
    /// (see [`ELIDED_OUTPUT_KEYS`]); the store path is derived, never hashed.
    OutputPathNotElidable(String),
    /// A `.drv` already exists at the target path but its bytes differ from
    /// the derivation being realized. Under input-addressing this is a digest
    /// collision or store corruption — never a normal condition.
    DrvMismatch(PathBuf),
    /// Filesystem error during realization.
    Io(io::Error),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::Cdf(e) => write!(f, "cdf: {e}"),
            StoreError::OutputPathNotElidable(k) => write!(
                f,
                "key {k:?} names a resolved output path; use the literal $out token (03 §5.2) — \
                 the store path can never be a hash input"
            ),
            StoreError::DrvMismatch(p) => write!(
                f,
                "existing .drv at {} differs from this derivation (digest collision or corruption)",
                p.display()
            ),
            StoreError::Io(e) => write!(f, "io: {e}"),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<CdfError> for StoreError {
    fn from(e: CdfError) -> Self {
        StoreError::Cdf(e)
    }
}
impl From<io::Error> for StoreError {
    fn from(e: io::Error) -> Self {
        StoreError::Io(e)
    }
}

/// A derivation ready to be addressed and realized: the elided (hashed) CDF
/// plus its identity, with the output store path as a hole filled by
/// [`resolve`](Derivation::resolve).
///
/// The builder collects only input-addressed keys — the concrete output path
/// is never among them (enforced by [`insert`](Derivation::insert)), so
/// [`digest_cdf`](Derivation::digest_cdf) *is* the elided form with no
/// separate blanking pass.
#[derive(Debug, Clone)]
pub struct Derivation {
    name: String,
    version: String,
    cdf: CdfBuilder,
    /// The hole: `None` until `resolve` computes the digest and backfills it.
    out_path: Option<String>,
}

/// The addressing result: digest and the two store paths derived from it
/// (02 §2 — one digest, two entries).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorePaths {
    pub digest: String,
    /// `<store_root>/<digest>-<name>-<version>` — the output directory.
    pub out_path: PathBuf,
    /// `<out_path>.drv` — the derivation, exact elided CDF bytes.
    pub drv_path: PathBuf,
    /// The final path component `<digest>-<name>-<version>`.
    pub store_name: String,
}

impl Derivation {
    /// Start a derivation. `name`/`version` are normalized/validated per
    /// 03 §2 / 02 §2 and inserted as the `name`/`version` CDF keys.
    pub fn new(name: &str, version: &str) -> Result<Self, StoreError> {
        let name = shade_cdf::normalize_name(name)?;
        shade_cdf::validate_version(version)?;
        let mut cdf = CdfBuilder::new();
        cdf.insert("name", &name)?;
        cdf.insert("version", version)?;
        Ok(Derivation {
            name,
            version: String::from(version),
            cdf,
            out_path: None,
        })
    }

    /// Insert an input-addressed CDF key. Rejects the reserved header, bad
    /// keys, duplicates (via [`CdfBuilder`]), and any resolved output-path
    /// field ([`ELIDED_OUTPUT_KEYS`]) — the latter keeps the store path out of
    /// its own hash. `output.<i>` *declarations* (`bin/rkilo`, 03 §6) are
    /// normal hashed inputs and are allowed.
    pub fn insert(&mut self, key: &str, value: &str) -> Result<(), StoreError> {
        if ELIDED_OUTPUT_KEYS.contains(&key) {
            return Err(StoreError::OutputPathNotElidable(String::from(key)));
        }
        self.cdf.insert(key, value)?;
        Ok(())
    }

    /// The elided CDF bytes: exactly what is hashed and exactly what the
    /// `.drv` stores (02 §3.2 — the `.drv` content is the CDF bytes). Contains
    /// no resolved output path; only the literal `$out` token appears in
    /// phases/env (03 §5.2).
    pub fn digest_cdf(&self) -> Vec<u8> {
        self.cdf.build()
    }

    /// The store digest: `BLAKE3-160(elided CDF)`, base32 over the pinned
    /// alphabet (02 §2). Pure function of the inputs — no clock, no network,
    /// no ambient state.
    pub fn digest(&self) -> String {
        store_digest(&self.digest_cdf())
    }

    /// Compute the digest, construct the store paths under `store_root`, and
    /// backfill the resolved output path into this derivation (the hole).
    /// One pass — the digest is known before the path is named, so there is no
    /// two-phase resolve. Idempotent: calling again recomputes identically.
    pub fn resolve(&mut self, store_root: &Path) -> StorePaths {
        let digest = self.digest();
        let store_name = format!("{digest}-{}-{}", self.name, self.version);
        let out_path = store_root.join(&store_name);
        let drv_path = with_drv_ext(&out_path);
        // Backfill: record the resolved path on the derivation. This is the
        // value the builder substitutes for `$out` at build time (track 2);
        // it deliberately does *not* re-enter `cdf`, so the digest is stable.
        self.out_path = Some(out_path.to_string_lossy().into_owned());
        StorePaths { digest, out_path, drv_path, store_name }
    }

    /// The backfilled output path, if [`resolve`](Derivation::resolve) has run.
    pub fn out_path(&self) -> Option<&str> {
        self.out_path.as_deref()
    }

    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn version(&self) -> &str {
        &self.version
    }
}

fn with_drv_ext(out_path: &Path) -> PathBuf {
    let mut s = out_path.as_os_str().to_os_string();
    s.push(".drv");
    PathBuf::from(s)
}

/// Outcome of [`realize`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Realized {
    pub paths: StorePaths,
    /// True if `<out_path>` already existed and realization was a no-op
    /// (idempotent hit). False if this call installed it.
    pub already_present: bool,
}

/// Realize `drv` into the store under `store_root`, installing the staged
/// output tree at `staged` as the immutable `<out_path>` and writing the
/// `.drv`.
///
/// Guarantees (02 §2):
/// - **Atomic**: the output tree is copied into a sibling temp dir and
///   `rename`d into place; the `.drv` is written to a temp file and `rename`d.
///   A crash mid-realize leaves only `.tmp-*` entries — never a partial
///   `<out_path>` or `.drv`.
/// - **Idempotent**: if `<out_path>` already exists, this is a no-op
///   (`already_present = true`); the tree is never overwritten (immutability).
/// - **Consistent `.drv`**: an existing `.drv` must byte-match; otherwise
///   [`StoreError::DrvMismatch`].
///
/// `drv` must already be [`resolve`](Derivation::resolve)d against the same
/// `store_root` (or is resolved here). `staged` is produced by track 2; for
/// track 1 self-containment it is any prepared directory tree.
pub fn realize(
    store_root: &Path,
    drv: &mut Derivation,
    staged: &Path,
) -> Result<Realized, StoreError> {
    fs::create_dir_all(store_root)?;
    let paths = drv.resolve(store_root);
    let cdf_bytes = drv.digest_cdf();
    let already_present = install(store_root, &paths, &cdf_bytes, staged)?;
    Ok(Realized { paths, already_present })
}

/// Store paths for already-canonical CDF bytes under `store_root` — the same
/// addressing as [`Derivation::resolve`], but for bytes that come straight
/// from a CDF producer (the shade evaluator) rather than a [`Derivation`]
/// builder. `name` is normalized and `version` validated (02 §2); the digest
/// is `store_digest(cdf_bytes)`, independent of `store_root`.
pub fn store_paths_at(
    store_root: &Path,
    name: &str,
    version: &str,
    cdf_bytes: &[u8],
) -> Result<StorePaths, StoreError> {
    let name = shade_cdf::normalize_name(name)?;
    shade_cdf::validate_version(version)?;
    let digest = store_digest(cdf_bytes);
    let store_name = format!("{digest}-{name}-{version}");
    let out_path = store_root.join(&store_name);
    let drv_path = with_drv_ext(&out_path);
    Ok(StorePaths { digest, out_path, drv_path, store_name })
}

/// Realize already-canonical CDF bytes: the [`realize`] contract, but keyed
/// on `(name, version, cdf_bytes)` from a CDF producer instead of a
/// [`Derivation`]. Same atomicity/idempotence/`.drv`-consistency guarantees.
/// This is the integration point for `shade build`: the evaluator emits the
/// CDF, the store realizes it.
pub fn realize_cdf(
    store_root: &Path,
    name: &str,
    version: &str,
    cdf_bytes: &[u8],
    staged: &Path,
) -> Result<Realized, StoreError> {
    fs::create_dir_all(store_root)?;
    let paths = store_paths_at(store_root, name, version, cdf_bytes)?;
    let already_present = install(store_root, &paths, cdf_bytes, staged)?;
    Ok(Realized { paths, already_present })
}

/// The atomic install core shared by [`realize`] and [`realize_cdf`]: stage
/// the output tree into a sibling temp dir, `rename` it into `<out_path>`,
/// write the `.drv`. Returns `already_present` (idempotent hit). `store_root`
/// must exist.
fn install(
    store_root: &Path,
    paths: &StorePaths,
    cdf_bytes: &[u8],
    staged: &Path,
) -> Result<bool, StoreError> {
    if paths.out_path.exists() {
        // Immutable: never rewrite. Just make sure the .drv is present and
        // matches, then report the idempotent hit.
        ensure_drv(&paths.drv_path, cdf_bytes)?;
        return Ok(true);
    }

    // Stage into a temp dir on the same filesystem, then atomically install.
    let tmp_dir = temp_sibling(store_root, &paths.store_name, "d");
    // A leftover from a crashed prior attempt would collide; clear it.
    let _ = fs::remove_dir_all(&tmp_dir);
    copy_tree(staged, &tmp_dir)?;
    fsync_dir(&tmp_dir)?;

    match fs::rename(&tmp_dir, &paths.out_path) {
        Ok(()) => {}
        Err(e) => {
            // Lost a race (another realizer won) — immutability makes the
            // winner's tree authoritative; drop ours and treat as a hit.
            let _ = fs::remove_dir_all(&tmp_dir);
            if paths.out_path.exists() {
                ensure_drv(&paths.drv_path, cdf_bytes)?;
                return Ok(true);
            }
            return Err(StoreError::Io(e));
        }
    }
    fsync_dir(store_root)?;

    ensure_drv(&paths.drv_path, cdf_bytes)?;
    Ok(false)
}

/// Write the `.drv` atomically if absent; verify a byte match if present.
fn ensure_drv(drv_path: &Path, cdf_bytes: &[u8]) -> Result<(), StoreError> {
    if drv_path.exists() {
        let existing = fs::read(drv_path)?;
        if existing != cdf_bytes {
            return Err(StoreError::DrvMismatch(drv_path.to_path_buf()));
        }
        return Ok(());
    }
    let parent = drv_path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = drv_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = temp_sibling(parent, &file_name, "f");
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(cdf_bytes)?;
        f.sync_all()?;
    }
    match fs::rename(&tmp, drv_path) {
        Ok(()) => {}
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            // If another writer got there first with matching bytes, fine.
            if drv_path.exists() && fs::read(drv_path)? == cdf_bytes {
                return Ok(());
            }
            return Err(StoreError::Io(e));
        }
    }
    fsync_dir(parent)?;
    Ok(())
}

/// A unique temp path sibling to `final_name` under `dir`. The uniqueness
/// source (pid + counter) is transient and never enters any hash (02 §3.3
/// excludes build-machine identity).
fn temp_sibling(dir: &Path, final_name: &str, kind: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    dir.join(format!(".tmp-{kind}-{final_name}-{pid}-{n}"))
}

/// Recursively copy a tree (files, dirs, symlinks) from `src` to `dst`.
fn copy_tree(src: &Path, dst: &Path) -> io::Result<()> {
    let meta = fs::symlink_metadata(src)?;
    let ft = meta.file_type();
    if ft.is_symlink() {
        let target = fs::read_link(src)?;
        symlink(&target, dst)?;
    } else if ft.is_dir() {
        fs::create_dir_all(dst)?;
        for entry in fs::read_dir(src)? {
            let entry = entry?;
            copy_tree(&entry.path(), &dst.join(entry.file_name()))?;
        }
    } else {
        if let Some(parent) = dst.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(src, dst)?;
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
        "symlinks in staged output require a unix host",
    ))
}

/// Best-effort directory fsync (forces the rename/commit durable, 02 §6.3).
/// Ignored where the platform disallows opening a directory for fsync.
fn fsync_dir(dir: &Path) -> io::Result<()> {
    match fs::File::open(dir) {
        Ok(f) => match f.sync_all() {
            Ok(()) => Ok(()),
            // Some platforms reject fsync on a directory fd; not fatal.
            Err(_) => Ok(()),
        },
        Err(_) => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A throwaway unique directory under the OS temp dir; removed on drop.
    struct TmpDir(PathBuf);
    impl TmpDir {
        fn new(tag: &str) -> Self {
            static C: AtomicU64 = AtomicU64::new(0);
            let n = C.fetch_add(1, Ordering::Relaxed);
            let p = std::env::temp_dir().join(format!(
                "shade-store-test-{tag}-{}-{n}",
                std::process::id()
            ));
            let _ = fs::remove_dir_all(&p);
            fs::create_dir_all(&p).unwrap();
            TmpDir(p)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TmpDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// A representative rkilo-shaped derivation. Phases use the literal `$out`
    /// token (03 §5.2) — the elision hole.
    fn rkilo() -> Derivation {
        let mut d = Derivation::new("rkilo", "1.2.0").unwrap();
        d.insert("system", "x86_64-oros").unwrap();
        d.insert("toolchain", "rustc-1.86.0-adf2135f0").unwrap();
        d.insert("sandbox", "1").unwrap();
        d.insert("dep.0", "/shade/store/c4fq3m2z7xj5kx2apwrn6uu3drhtbz3i-lythos-libstd-0.3.0")
            .unwrap();
        d.insert("output.0", "bin/rkilo").unwrap();
        d.insert("phase.0", "cargo build --release --offline --target x86_64-oros")
            .unwrap();
        d.insert(
            "phase.1",
            "install -m755 target/x86_64-oros/release/rkilo $out/bin/rkilo",
        )
        .unwrap();
        d
    }

    fn stage_tree(root: &Path) -> PathBuf {
        let staged = root.join("staged");
        fs::create_dir_all(staged.join("bin")).unwrap();
        fs::write(staged.join("bin/rkilo"), b"\x7fELF fake binary").unwrap();
        staged
    }

    #[test]
    fn digest_is_deterministic() {
        assert_eq!(rkilo().digest(), rkilo().digest());
        // 32 chars, pinned alphabet only.
        let d = rkilo().digest();
        assert_eq!(d.len(), 32);
        assert!(d.bytes().all(|c| shade_cdf::BASE32_ALPHABET.contains(&c)));
    }

    #[test]
    fn digest_uses_pinned_alphabet_no_words() {
        // The whole point of the Nix alphabet: e/o/t/u never appear.
        let d = rkilo().digest();
        assert!(!d.bytes().any(|c| matches!(c, b'e' | b'o' | b't' | b'u')));
    }

    #[test]
    fn digest_is_input_sensitive() {
        let base = rkilo().digest();

        // Build an rkilo variant with one (key,value) overridden, or — when
        // `over_key` is name/version — a different identity.
        let variant = |name: &str, version: &str, over: Option<(&str, &str)>| -> String {
            let mut d = Derivation::new(name, version).unwrap();
            for (k, v) in sample_entries() {
                let v = match over {
                    Some((ok, ov)) if ok == k => ov,
                    _ => v,
                };
                d.insert(k, v).unwrap();
            }
            d.digest()
        };

        assert_ne!(base, variant("rkilo", "1.2.1", None), "version change must move digest");
        assert_ne!(base, variant("rkilo2", "1.2.0", None), "name change must move digest");
        assert_ne!(
            base,
            variant("rkilo", "1.2.0", Some(("phase.1",
                "install -m755 target/x86_64-oros/release/rkilo $out/bin/rkilo2"))),
            "phase change must move digest"
        );
        assert_ne!(
            base,
            variant("rkilo", "1.2.0", Some(("dep.0",
                "/shade/store/00000000000000000000000000000000-lythos-libstd-0.3.0"))),
            "dep change must move digest"
        );
    }

    /// The non-name/version entries of [`rkilo`], for rebuilding variants.
    fn sample_entries() -> Vec<(&'static str, &'static str)> {
        vec![
            ("system", "x86_64-oros"),
            ("toolchain", "rustc-1.86.0-adf2135f0"),
            ("sandbox", "1"),
            ("dep.0", "/shade/store/c4fq3m2z7xj5kx2apwrn6uu3drhtbz3i-lythos-libstd-0.3.0"),
            ("output.0", "bin/rkilo"),
            ("phase.0", "cargo build --release --offline --target x86_64-oros"),
            ("phase.1", "install -m755 target/x86_64-oros/release/rkilo $out/bin/rkilo"),
        ]
    }

    #[test]
    fn resolved_output_path_does_not_affect_digest() {
        // Same derivation resolved under two different store roots yields the
        // same digest: the concrete output path is elided from the hash.
        let mut a = rkilo();
        let mut b = rkilo();
        let pa = a.resolve(Path::new("/shade/store"));
        let pb = b.resolve(Path::new("/tmp/elsewhere/store"));
        assert_eq!(pa.digest, pb.digest);
        // And the digest equals a straight hash of the elided CDF — nothing
        // path-derived feeds in.
        assert_eq!(pa.digest, store_digest(&a.digest_cdf()));
        // Backfilling the hole changed the paths but not the identity.
        assert_ne!(pa.out_path, pb.out_path);
    }

    #[test]
    fn output_path_field_is_rejected() {
        let mut d = Derivation::new("x", "1").unwrap();
        for k in ELIDED_OUTPUT_KEYS {
            assert!(matches!(
                d.insert(k, "/shade/store/zzzz-x-1"),
                Err(StoreError::OutputPathNotElidable(_))
            ));
        }
    }

    #[test]
    fn drv_stores_literal_out_not_resolved_path() {
        // Elision, on disk: the stored .drv carries the literal `$out` token,
        // never the concrete output directory path.
        let tmp = TmpDir::new("literal");
        let store_root = tmp.path().join("store");
        let staged = stage_tree(tmp.path());
        let mut d = rkilo();
        let r = realize(&store_root, &mut d, &staged).unwrap();
        let drv_bytes = fs::read(&r.paths.drv_path).unwrap();
        let drv_text = String::from_utf8(drv_bytes).unwrap();
        assert!(drv_text.contains("$out/bin/rkilo"), "literal token preserved");
        let concrete = r.paths.out_path.to_string_lossy().into_owned();
        assert!(
            !drv_text.contains(&concrete),
            "concrete output path must not appear in the derivation body"
        );
        // First line is the format header.
        assert!(drv_text.starts_with(&format!("{}=1\n", shade_cdf::HEADER_KEY)));
    }

    #[test]
    fn realize_is_idempotent() {
        let tmp = TmpDir::new("idem");
        let store_root = tmp.path().join("store");
        let staged = stage_tree(tmp.path());

        let mut d1 = rkilo();
        let r1 = realize(&store_root, &mut d1, &staged).unwrap();
        assert!(!r1.already_present);
        assert!(r1.paths.out_path.join("bin/rkilo").exists());

        // Re-realizing the same derivation is a no-op and must not error on
        // immutability. Even a *different* staged tree does not overwrite.
        let other = tmp.path().join("staged2");
        fs::create_dir_all(other.join("bin")).unwrap();
        fs::write(other.join("bin/rkilo"), b"DIFFERENT CONTENT").unwrap();
        let mut d2 = rkilo();
        let r2 = realize(&store_root, &mut d2, &other).unwrap();
        assert!(r2.already_present);
        assert_eq!(r1.paths, r2.paths);
        // Original content is untouched — immutable.
        assert_eq!(
            fs::read(r1.paths.out_path.join("bin/rkilo")).unwrap(),
            b"\x7fELF fake binary"
        );
    }

    #[test]
    fn atomic_write_partial_failure_recovers() {
        let tmp = TmpDir::new("atomic");
        let store_root = tmp.path().join("store");
        fs::create_dir_all(&store_root).unwrap();
        let staged = stage_tree(tmp.path());

        let mut probe = rkilo();
        let paths = probe.resolve(&store_root);

        // Simulate a crashed prior realize: a leftover temp dir with partial,
        // garbage contents and NO final out_path. This is the only state a
        // mid-realize crash can leave (the rename never happened).
        let stale = store_root.join(format!(".tmp-d-{}-999-999", paths.store_name));
        fs::create_dir_all(stale.join("bin")).unwrap();
        fs::write(stale.join("bin/rkilo"), b"HALF-WRITTEN GARBAGE").unwrap();

        // The final path must not exist yet — no partial ever surfaces there.
        assert!(!paths.out_path.exists());

        // Recovery: a fresh realize completes cleanly and the final path holds
        // the correct, complete tree.
        let mut d = rkilo();
        let r = realize(&store_root, &mut d, &staged).unwrap();
        assert!(!r.already_present);
        assert_eq!(
            fs::read(r.paths.out_path.join("bin/rkilo")).unwrap(),
            b"\x7fELF fake binary"
        );
        assert_eq!(fs::read(&r.paths.drv_path).unwrap(), d.digest_cdf());
    }

    #[test]
    fn drv_mismatch_is_reported() {
        let tmp = TmpDir::new("mismatch");
        let store_root = tmp.path().join("store");
        let staged = stage_tree(tmp.path());
        let mut d = rkilo();
        let r = realize(&store_root, &mut d, &staged).unwrap();

        // Corrupt the .drv, then re-realize: the mismatch must be caught, not
        // silently overwritten (store paths are immutable).
        fs::write(&r.paths.drv_path, b"corrupted\n").unwrap();
        // out_path still exists, so realize takes the idempotent branch and
        // checks the .drv.
        let mut d2 = rkilo();
        let err = realize(&store_root, &mut d2, &staged).unwrap_err();
        assert!(matches!(err, StoreError::DrvMismatch(_)));
    }
}
