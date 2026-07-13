//! shade-store — input-addressed store realization.
//!
//! Realizes a CDF derivation ([`shade_cdf`]) into the store at
//! `/shade/store/<digest>-<name>-<version>` per `docs/shade-pkg/02-store.md`
//! §2 (path format) and §3 (input-addressing). This is **track 1**: the
//! digest, the path, and the idempotent atomic write. It does **not** run
//! builds — build dispatch (track 2) produces the output tree and hands it to
//! [`realize`] as a staged directory.
//!
//! ## The filesystem seam
//!
//! All realization I/O goes through the injected [`StoreFs`] backend
//! ([`backend`]) — the crate itself is `no_std + alloc` and touches no
//! filesystem directly. [`HostFs`] (feature `std`, default) backs the host
//! suite and host tooling; `OrosFs` (feature `oros`) backs the same logic on
//! the Lythos ABI. Paths are plain `/`-separated strings on both sides.
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
//!   dir, never a partial `<out_path>` (so "exists ⇒ complete"). The rename
//!   is the sole seal: content never appears at a final store path any other
//!   way (this is what the kernel-side `RealizeGuard` keys on);
//! - writes the `.drv` (the exact elided CDF bytes) atomically via
//!   temp-file + rename;
//! - is **idempotent**: if `<out_path>` already exists it is a no-op; it never
//!   overwrites (immutability), and it verifies any existing `.drv` matches.

#![cfg_attr(not(any(test, feature = "std")), no_std)]

extern crate alloc;

pub mod backend;
#[cfg(feature = "std")]
mod host;
#[cfg(feature = "oros")]
mod oros;

pub use backend::{FsError, FsResult, NodeKind, NodeMeta, StoreFs};
#[cfg(feature = "std")]
pub use host::HostFs;
#[cfg(feature = "oros")]
pub use oros::OrosFs;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

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
    DrvMismatch(String),
    /// A backend filesystem operation failed during realization.
    Fs {
        op: &'static str,
        path: String,
        err: FsError,
    },
}

impl core::fmt::Display for StoreError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            StoreError::Cdf(e) => write!(f, "cdf: {e}"),
            StoreError::OutputPathNotElidable(k) => write!(
                f,
                "key {k:?} names a resolved output path; use the literal $out token (03 §5.2) — \
                 the store path can never be a hash input"
            ),
            StoreError::DrvMismatch(p) => write!(
                f,
                "existing .drv at {p} differs from this derivation (digest collision or corruption)"
            ),
            StoreError::Fs { op, path, err } => write!(f, "fs: {op} {path}: {err}"),
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for StoreError {}

impl From<CdfError> for StoreError {
    fn from(e: CdfError) -> Self {
        StoreError::Cdf(e)
    }
}

/// Shorthand: tag a backend failure with the operation and target path.
fn fs_op(op: &'static str, path: &str) -> impl FnOnce(FsError) -> StoreError {
    let path = String::from(path);
    move |err| StoreError::Fs { op, path, err }
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
    pub out_path: String,
    /// `<out_path>.drv` — the derivation, exact elided CDF bytes.
    pub drv_path: String,
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
    pub fn resolve(&mut self, store_root: &str) -> StorePaths {
        let digest = self.digest();
        let store_name = format!("{digest}-{}-{}", self.name, self.version);
        let out_path = backend::join(store_root, &store_name);
        let drv_path = format!("{out_path}.drv");
        // Backfill: record the resolved path on the derivation. This is the
        // value the builder substitutes for `$out` at build time (track 2);
        // it deliberately does *not* re-enter `cdf`, so the digest is stable.
        self.out_path = Some(out_path.clone());
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

/// Outcome of [`realize`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Realized {
    pub paths: StorePaths,
    /// True if `<out_path>` already existed and realization was a no-op
    /// (idempotent hit). False if this call installed it.
    pub already_present: bool,
}

/// Realize `drv` into the store under `store_root` on the injected backend
/// `fs`, installing the staged output tree at `staged` as the immutable
/// `<out_path>` and writing the `.drv`.
///
/// Guarantees (02 §2):
/// - **Atomic**: the output tree is copied into a sibling temp dir and
///   `rename`d into place; the `.drv` is written to a temp file and `rename`d.
///   A crash mid-realize leaves only `.tmp-*` entries — never a partial
///   `<out_path>` or `.drv`. The rename is the sole seal.
/// - **Idempotent**: if `<out_path>` already exists, this is a no-op
///   (`already_present = true`); the tree is never overwritten (immutability).
/// - **Consistent `.drv`**: an existing `.drv` must byte-match; otherwise
///   [`StoreError::DrvMismatch`].
///
/// `drv` must already be [`resolve`](Derivation::resolve)d against the same
/// `store_root` (or is resolved here). `staged` is produced by track 2; for
/// track 1 self-containment it is any prepared directory tree.
pub fn realize(
    fs: &mut dyn StoreFs,
    store_root: &str,
    drv: &mut Derivation,
    staged: &str,
) -> Result<Realized, StoreError> {
    backend::create_dir_all(fs, store_root).map_err(fs_op("create_dir_all", store_root))?;
    let paths = drv.resolve(store_root);
    let cdf_bytes = drv.digest_cdf();
    let already_present = install(fs, store_root, &paths, &cdf_bytes, staged)?;
    Ok(Realized { paths, already_present })
}

/// Store paths for already-canonical CDF bytes under `store_root` — the same
/// addressing as [`Derivation::resolve`], but for bytes that come straight
/// from a CDF producer (the shade evaluator) rather than a [`Derivation`]
/// builder. `name` is normalized and `version` validated (02 §2); the digest
/// is `store_digest(cdf_bytes)`, independent of `store_root`.
pub fn store_paths_at(
    store_root: &str,
    name: &str,
    version: &str,
    cdf_bytes: &[u8],
) -> Result<StorePaths, StoreError> {
    let name = shade_cdf::normalize_name(name)?;
    shade_cdf::validate_version(version)?;
    let digest = store_digest(cdf_bytes);
    let store_name = format!("{digest}-{name}-{version}");
    let out_path = backend::join(store_root, &store_name);
    let drv_path = format!("{out_path}.drv");
    Ok(StorePaths { digest, out_path, drv_path, store_name })
}

/// Realize already-canonical CDF bytes: the [`realize`] contract, but keyed
/// on `(name, version, cdf_bytes)` from a CDF producer instead of a
/// [`Derivation`]. Same atomicity/idempotence/`.drv`-consistency guarantees.
/// This is the integration point for `shade build`: the evaluator emits the
/// CDF, the store realizes it.
pub fn realize_cdf(
    fs: &mut dyn StoreFs,
    store_root: &str,
    name: &str,
    version: &str,
    cdf_bytes: &[u8],
    staged: &str,
) -> Result<Realized, StoreError> {
    backend::create_dir_all(fs, store_root).map_err(fs_op("create_dir_all", store_root))?;
    let paths = store_paths_at(store_root, name, version, cdf_bytes)?;
    let already_present = install(fs, store_root, &paths, cdf_bytes, staged)?;
    Ok(Realized { paths, already_present })
}

/// The atomic install core shared by [`realize`] and [`realize_cdf`]: stage
/// the output tree into a sibling temp dir, `rename` it into `<out_path>`,
/// write the `.drv`. Returns `already_present` (idempotent hit). `store_root`
/// must exist.
fn install(
    fs: &mut dyn StoreFs,
    store_root: &str,
    paths: &StorePaths,
    cdf_bytes: &[u8],
    staged: &str,
) -> Result<bool, StoreError> {
    if fs.exists(&paths.out_path) {
        // Immutable: never rewrite. Just make sure the .drv is present and
        // matches, then report the idempotent hit.
        ensure_drv(fs, &paths.drv_path, cdf_bytes)?;
        return Ok(true);
    }

    // Stage into a temp dir on the same filesystem, then atomically install.
    let tmp_dir = backend::temp_sibling(fs, store_root, &paths.store_name, "d");
    // A leftover from a crashed prior attempt would collide; clear it
    // (best-effort — on a backend without rmdir the file contents go and the
    // empty dir skeleton is reused by the copy below).
    backend::remove_tree(fs, &tmp_dir);
    backend::copy_tree(fs, staged, &tmp_dir).map_err(fs_op("copy_tree", &tmp_dir))?;
    let _ = fs.sync_dir(&tmp_dir);

    match fs.rename(&tmp_dir, &paths.out_path) {
        Ok(()) => {}
        Err(err) => {
            // Lost a race (another realizer won) — immutability makes the
            // winner's tree authoritative; drop ours and treat as a hit.
            backend::remove_tree(fs, &tmp_dir);
            if fs.exists(&paths.out_path) {
                ensure_drv(fs, &paths.drv_path, cdf_bytes)?;
                return Ok(true);
            }
            return Err(StoreError::Fs {
                op: "rename",
                path: paths.out_path.clone(),
                err,
            });
        }
    }
    let _ = fs.sync_dir(store_root);

    ensure_drv(fs, &paths.drv_path, cdf_bytes)?;
    Ok(false)
}

/// Write the `.drv` atomically if absent; verify a byte match if present.
fn ensure_drv(
    fs: &mut dyn StoreFs,
    drv_path: &str,
    cdf_bytes: &[u8],
) -> Result<(), StoreError> {
    if fs.exists(drv_path) {
        let existing = fs.read_file(drv_path).map_err(fs_op("read", drv_path))?;
        if existing != cdf_bytes {
            return Err(StoreError::DrvMismatch(String::from(drv_path)));
        }
        return Ok(());
    }
    let (parent, file_name) = backend::split_parent(drv_path);
    let tmp = backend::temp_sibling(fs, parent, file_name, "f");
    fs.write_file(&tmp, cdf_bytes, false).map_err(fs_op("write", &tmp))?;
    match fs.rename(&tmp, drv_path) {
        Ok(()) => {}
        Err(err) => {
            let _ = fs.unlink(&tmp);
            // If another writer got there first with matching bytes, fine.
            if fs.exists(drv_path)
                && fs.read_file(drv_path).map_err(fs_op("read", drv_path))? == cdf_bytes
            {
                return Ok(());
            }
            return Err(StoreError::Fs { op: "rename", path: String::from(drv_path), err });
        }
    }
    let _ = fs.sync_dir(parent);
    Ok(())
}

#[cfg(test)]
mod memfs;

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use crate::memfs::MemFs;

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
        /// A path under this dir, as the string form the seam API takes.
        fn sub(&self, rest: &str) -> String {
            self.0.join(rest).to_str().unwrap().into()
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

    fn stage_tree(root: &Path) -> String {
        let staged = root.join("staged");
        fs::create_dir_all(staged.join("bin")).unwrap();
        fs::write(staged.join("bin/rkilo"), b"\x7fELF fake binary").unwrap();
        staged.to_str().unwrap().into()
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
        let pa = a.resolve("/shade/store");
        let pb = b.resolve("/tmp/elsewhere/store");
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
        let store_root = tmp.sub("store");
        let staged = stage_tree(tmp.path());
        let mut d = rkilo();
        let r = realize(&mut HostFs, &store_root, &mut d, &staged).unwrap();
        let drv_bytes = fs::read(&r.paths.drv_path).unwrap();
        let drv_text = String::from_utf8(drv_bytes).unwrap();
        assert!(drv_text.contains("$out/bin/rkilo"), "literal token preserved");
        assert!(
            !drv_text.contains(&r.paths.out_path),
            "concrete output path must not appear in the derivation body"
        );
        // First line is the format header.
        assert!(drv_text.starts_with(&format!("{}=1\n", shade_cdf::HEADER_KEY)));
    }

    #[test]
    fn realize_is_idempotent() {
        let tmp = TmpDir::new("idem");
        let store_root = tmp.sub("store");
        let staged = stage_tree(tmp.path());

        let mut d1 = rkilo();
        let r1 = realize(&mut HostFs, &store_root, &mut d1, &staged).unwrap();
        assert!(!r1.already_present);
        let installed = backend::join(&r1.paths.out_path, "bin/rkilo");
        assert!(Path::new(&installed).exists());

        // Re-realizing the same derivation is a no-op and must not error on
        // immutability. Even a *different* staged tree does not overwrite.
        let other = tmp.path().join("staged2");
        fs::create_dir_all(other.join("bin")).unwrap();
        fs::write(other.join("bin/rkilo"), b"DIFFERENT CONTENT").unwrap();
        let mut d2 = rkilo();
        let r2 = realize(&mut HostFs, &store_root, &mut d2, other.to_str().unwrap()).unwrap();
        assert!(r2.already_present);
        assert_eq!(r1.paths, r2.paths);
        // Original content is untouched — immutable.
        assert_eq!(fs::read(&installed).unwrap(), b"\x7fELF fake binary");
    }

    #[test]
    fn atomic_write_partial_failure_recovers() {
        let tmp = TmpDir::new("atomic");
        let store_root = tmp.sub("store");
        fs::create_dir_all(&store_root).unwrap();
        let staged = stage_tree(tmp.path());

        let mut probe = rkilo();
        let paths = probe.resolve(&store_root);

        // Simulate a crashed prior realize: a leftover temp dir with partial,
        // garbage contents and NO final out_path. This is the only state a
        // mid-realize crash can leave (the rename never happened).
        let stale = Path::new(&store_root).join(format!(".tmp-d-{}-999-999", paths.store_name));
        fs::create_dir_all(stale.join("bin")).unwrap();
        fs::write(stale.join("bin/rkilo"), b"HALF-WRITTEN GARBAGE").unwrap();

        // The final path must not exist yet — no partial ever surfaces there.
        assert!(!Path::new(&paths.out_path).exists());

        // Recovery: a fresh realize completes cleanly and the final path holds
        // the correct, complete tree.
        let mut d = rkilo();
        let r = realize(&mut HostFs, &store_root, &mut d, &staged).unwrap();
        assert!(!r.already_present);
        assert_eq!(
            fs::read(backend::join(&r.paths.out_path, "bin/rkilo")).unwrap(),
            b"\x7fELF fake binary"
        );
        assert_eq!(fs::read(&r.paths.drv_path).unwrap(), d.digest_cdf());
    }

    #[test]
    fn drv_mismatch_is_reported() {
        let tmp = TmpDir::new("mismatch");
        let store_root = tmp.sub("store");
        let staged = stage_tree(tmp.path());
        let mut d = rkilo();
        let r = realize(&mut HostFs, &store_root, &mut d, &staged).unwrap();

        // Corrupt the .drv, then re-realize: the mismatch must be caught, not
        // silently overwritten (store paths are immutable).
        fs::write(&r.paths.drv_path, b"corrupted\n").unwrap();
        // out_path still exists, so realize takes the idempotent branch and
        // checks the .drv.
        let mut d2 = rkilo();
        let err = realize(&mut HostFs, &store_root, &mut d2, &staged).unwrap_err();
        assert!(matches!(err, StoreError::DrvMismatch(_)));
    }

    // ── Seam tests: the same realization logic on a pure in-memory backend ──

    /// Stage the rkilo tree on a MemFs (what track 2 would produce on target).
    fn mem_stage(fs: &mut MemFs) -> String {
        backend::create_dir_all(fs, "/build/staged/bin").unwrap();
        fs.write_file("/build/staged/bin/rkilo", b"\x7fELF fake binary", true).unwrap();
        String::from("/build/staged")
    }

    #[test]
    fn realize_through_mem_backend() {
        let mut fs = MemFs::new();
        let staged = mem_stage(&mut fs);
        let mut d = rkilo();
        let r = realize(&mut fs, "/shade/store", &mut d, &staged).unwrap();
        assert!(!r.already_present);
        let installed = backend::join(&r.paths.out_path, "bin/rkilo");
        assert_eq!(fs.read_file(&installed).unwrap(), b"\x7fELF fake binary");
        assert!(fs.metadata(&installed).unwrap().exec, "exec bit survives realization");
        assert_eq!(fs.read_file(&r.paths.drv_path).unwrap(), d.digest_cdf());

        // Idempotent second realize through the same seam.
        let mut d2 = rkilo();
        let r2 = realize(&mut fs, "/shade/store", &mut d2, &staged).unwrap();
        assert!(r2.already_present);
        assert_eq!(r.paths, r2.paths);
    }

    #[test]
    fn rename_is_the_sole_seal_on_the_seam() {
        // The kernel RealizeGuard seals a store name on the rename — so the
        // realization logic must never write/mkdir directly at or under the
        // final out_path. The MemFs op log proves it: out_path appears only
        // as a rename destination.
        let mut fs = MemFs::new();
        let staged = mem_stage(&mut fs);
        let mut d = rkilo();
        fs.ops.clear();
        let r = realize(&mut fs, "/shade/store", &mut d, &staged).unwrap();

        let out = &r.paths.out_path;
        for op in &fs.ops {
            let (name, target) = op;
            if target == out || target.starts_with(&format!("{out}/")) {
                assert_eq!(
                    name, "rename_dst",
                    "only a rename may target the final store path, got {name} {target}"
                );
            }
        }
        // And the seal rename actually happened.
        assert!(fs
            .ops
            .iter()
            .any(|(name, target)| name == "rename_dst" && target == out));
    }

    #[test]
    fn mem_backend_race_loss_converges() {
        // Losing the seal race must converge to the winner's tree: simulate
        // by pre-realizing (winner), then realizing again with a *different*
        // staged tree (loser path takes the exists-hit branch).
        let mut fs = MemFs::new();
        let staged = mem_stage(&mut fs);
        let mut d1 = rkilo();
        let r1 = realize(&mut fs, "/shade/store", &mut d1, &staged).unwrap();

        backend::create_dir_all(&mut fs, "/build/other/bin").unwrap();
        fs.write_file("/build/other/bin/rkilo", b"DIFFERENT", false).unwrap();
        let mut d2 = rkilo();
        let r2 = realize(&mut fs, "/shade/store", &mut d2, "/build/other").unwrap();
        assert!(r2.already_present);
        assert_eq!(
            fs.read_file(&backend::join(&r1.paths.out_path, "bin/rkilo")).unwrap(),
            b"\x7fELF fake binary",
            "winner's sealed tree is authoritative"
        );
    }

    #[test]
    fn mem_backend_symlink_roundtrip() {
        // The seam carries symlinks (host + mem; the oros backend reports
        // Unsupported until the ABI grows symlink syscalls).
        let mut fs = MemFs::new();
        let staged = mem_stage(&mut fs);
        fs.symlink("bin/rkilo", "/build/staged/default").unwrap();
        let mut d = rkilo();
        let r = realize(&mut fs, "/shade/store", &mut d, &staged).unwrap();
        assert_eq!(
            fs.read_link(&backend::join(&r.paths.out_path, "default")).unwrap(),
            "bin/rkilo"
        );
    }
}
