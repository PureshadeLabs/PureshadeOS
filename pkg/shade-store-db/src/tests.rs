//! Store-db + GC tests. They exercise the three things GC safety rests on:
//! reference recording (declared ∪ scanned), the roots model (direct,
//! indirect/build-lock, dangling prune), and the mark-and-sweep — proving GC
//! never collects a rooted or reference-reachable path. Everything runs
//! through the injected [`StoreFs`] seam (HostFs over a throwaway dir); the
//! lock tests prove the exclusive-create acquire is genuinely atomic.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use super::*;

/// A throwaway `/shade`-shaped prefix under the OS temp dir; removed on drop.
struct TmpShade(PathBuf);
impl TmpShade {
    fn new(tag: &str) -> Self {
        static C: AtomicU64 = AtomicU64::new(0);
        let n = C.fetch_add(1, Ordering::Relaxed);
        let p = std::env::temp_dir().join(format!(
            "shade-db-test-{tag}-{}-{n}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(p.join("store")).unwrap();
        TmpShade(p)
    }
    fn db(&self) -> StoreDb<HostFs> {
        StoreDb::new(&self.0)
    }
    fn root(&self) -> String {
        self.0.to_str().unwrap().into()
    }
    fn store(&self) -> String {
        format!("{}/store", self.root())
    }
}
impl Drop for TmpShade {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Create a store output directory with a valid-shaped name and some files.
/// Returns `(digest, store_name, out_path)`.
fn mk_entry(
    store: &str,
    seed: char,
    name: &str,
    files: &[(&str, &[u8])],
) -> (String, String, String) {
    let digest: String = std::iter::repeat(seed).take(32).collect();
    assert!(digest.bytes().all(|b| BASE32_ALPHABET.contains(&b)), "seed in alphabet");
    let store_name = format!("{digest}-{name}-1.0");
    let dir = format!("{store}/{store_name}");
    fs::create_dir_all(&dir).unwrap();
    for (rel, content) in files {
        let p = PathBuf::from(&dir).join(rel);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&p, content).unwrap();
    }
    (digest, store_name, dir)
}

fn exists(path: impl AsRef<std::path::Path>) -> bool {
    path.as_ref().exists()
}

#[test]
fn register_records_declared_and_scanned_refs() {
    let tmp = TmpShade::new("refs");
    let db = tmp.db();
    let store = tmp.store();

    // A: a leaf dependency.
    let (a_digest, a_name, a_path) = mk_entry(&store, 'a', "depa", &[("bin/a", b"leaf")]);
    db.register(&a_path, &a_digest, &a_name, &"aa".repeat(32), &[]).unwrap();

    // C: a declared-but-not-embedded dependency.
    let (c_digest, c_name, c_path) = mk_entry(&store, 'c', "depc", &[("bin/c", b"leaf")]);
    db.register(&c_path, &c_digest, &c_name, &"cc".repeat(32), &[]).unwrap();

    // B: embeds A's store path in a binary (scanned ref) and declares C
    // (declared ref it does not embed). Its record must carry BOTH.
    let embedded = format!("{store}/{a_name}");
    let (b_digest, b_name, b_path) = mk_entry(
        &store,
        'b',
        "top",
        &[("bin/b", format!("run {embedded} now").as_bytes())],
    );
    let declared = vec![format!("{store}/{c_name}")];
    let rec = db.register(&b_path, &b_digest, &b_name, &"bb".repeat(32), &declared).unwrap();

    let refs = db.read_refs(&b_digest).unwrap();
    assert!(refs.contains(&a_digest), "scanned ref recorded: {refs:?}");
    assert!(refs.contains(&c_digest), "declared ref recorded: {refs:?}");
    assert!(!refs.contains(&b_digest), "never self-references");
    assert_eq!(rec.refs, refs, "returned record mirrors refs file");

    // Valid record round-trips.
    let v = db.read_valid(&b_digest).unwrap().expect("valid record");
    assert_eq!(v.cdf_hash, "bb".repeat(32));
    assert_eq!(v.name, b_name);
    assert!(v.registered > 0);
}

#[test]
fn gc_keeps_rooted_and_reachable_collects_the_rest() {
    let tmp = TmpShade::new("gc");
    let db = tmp.db();
    let store = tmp.store();

    // A (leaf) <- B (refs A). D is unrelated garbage.
    let (a_digest, a_name, a_path) = mk_entry(&store, 'a', "depa", &[("bin/a", b"leaf")]);
    db.register(&a_path, &a_digest, &a_name, &"aa".repeat(32), &[]).unwrap();
    let embedded = format!("{store}/{a_name}");
    let (b_digest, b_name, b_path) =
        mk_entry(&store, 'b', "top", &[("bin/b", embedded.as_bytes())]);
    db.register(&b_path, &b_digest, &b_name, &"bb".repeat(32), &[]).unwrap();
    let (d_digest, d_name, d_path) = mk_entry(&store, 'd', "dead", &[("bin/d", b"orphan")]);
    db.register(&d_path, &d_digest, &d_name, &"dd".repeat(32), &[]).unwrap();

    // Root B only. A is reachable via B's reference; D is not.
    db.add_root("me-top", &b_path).unwrap();
    let report = db.gc(&GcOptions::default()).unwrap();

    assert!(exists(format!("{store}/{b_name}")), "rooted B kept");
    assert!(exists(format!("{store}/{a_name}")), "reference-reachable A kept");
    assert!(!exists(format!("{store}/{d_name}")), "unreachable D collected");
    assert_eq!(report.collected, vec![d_name.clone()]);
    assert_eq!(report.kept, 2);
    assert!(report.freed_bytes > 0, "swept D's bytes are sized through the seam");
    // D's db records went with it; A's and B's stayed.
    assert!(!db.is_valid(&d_digest));
    assert!(db.is_valid(&a_digest) && db.is_valid(&b_digest));

    // Remove the root: now the whole B→A closure is unreachable and collected.
    db.remove_root("me-top").unwrap();
    let report2 = db.gc(&GcOptions::default()).unwrap();
    assert!(!exists(format!("{store}/{b_name}")) && !exists(format!("{store}/{a_name}")));
    assert_eq!(report2.collected, vec![a_name, b_name]);
    assert_eq!(report2.kept, 0);
}

#[test]
fn build_lock_is_an_indirect_root() {
    let tmp = TmpShade::new("lock");
    let db = tmp.db();
    let store = tmp.store();

    let (a_digest, a_name, a_path) = mk_entry(&store, 'a', "input", &[("bin/a", b"x")]);
    db.register(&a_path, &a_digest, &a_name, &"aa".repeat(32), &[]).unwrap();
    let (b_digest, b_name, _) = mk_entry(&store, 'b', "junk", &[("bin/b", b"y")]);
    let _ = b_digest;

    // A build in flight holds a lock naming A (its input). GC refuses without
    // force...
    let lock = db.lock_build("build-123", &[a_path.clone()]).unwrap();
    let err = db.gc(&GcOptions::default()).unwrap_err();
    assert!(matches!(err, DbError::Busy(_)), "in-flight build blocks gc: {err}");

    // ...and under force, the lock keeps A alive (indirect root) while the
    // unrooted, unreferenced B is still collected.
    let report = db.gc(&GcOptions { force: true, dry_run: false }).unwrap();
    assert!(exists(format!("{store}/{a_name}")), "locked input kept even under --force");
    assert!(!exists(format!("{store}/{b_name}")), "unlocked junk collected");
    let _ = report;

    // Releasing the lock, A has no root and is now collectable.
    lock.release();
    assert_eq!(db.builds_in_flight(), 0);
    let report2 = db.gc(&GcOptions::default()).unwrap();
    assert!(report2.collected.contains(&a_name));
}

#[test]
fn dangling_root_is_pruned_and_grammar_violations_swept() {
    let tmp = TmpShade::new("prune");
    let db = tmp.db();
    let store = tmp.store();

    // A live, rooted entry.
    let (a_digest, a_name, a_path) = mk_entry(&store, 'a', "live", &[("f", b"x")]);
    db.register(&a_path, &a_digest, &a_name, &"aa".repeat(32), &[]).unwrap();
    db.add_root("keep", &a_path).unwrap();

    // A dangling root: points at a store path that does not exist.
    db.add_root("stale", &format!("{store}/00000000000000000000000000000000-gone-1.0"))
        .unwrap();

    // A store entry whose name is not a valid store name (02 §2 grammar).
    fs::create_dir_all(format!("{store}/not-a-store-name")).unwrap();
    fs::write(format!("{store}/not-a-store-name/junk"), b"z").unwrap();

    let report = db.gc(&GcOptions::default()).unwrap();

    assert!(exists(format!("{store}/{a_name}")), "rooted entry kept");
    assert!(!exists(format!("{store}/not-a-store-name")), "grammar violation swept");
    assert_eq!(report.pruned_roots, 1, "dangling root pruned");
    assert!(db.list_roots().unwrap().iter().any(|(n, _)| n == "keep"));
    assert!(!db.list_roots().unwrap().iter().any(|(n, _)| n == "stale"));
}

#[test]
fn dry_run_collects_nothing() {
    let tmp = TmpShade::new("dry");
    let db = tmp.db();
    let store = tmp.store();

    let (d_digest, d_name, d_path) = mk_entry(&store, 'd', "dead", &[("f", b"x")]);
    db.register(&d_path, &d_digest, &d_name, &"dd".repeat(32), &[]).unwrap();

    let report = db.gc(&GcOptions { dry_run: true, force: false }).unwrap();
    assert_eq!(report.collected, vec![d_name.clone()], "reports what it would collect");
    assert!(report.dry_run);
    assert!(exists(format!("{store}/{d_name}")), "dry-run deletes nothing");
    assert!(db.is_valid(&d_digest), "dry-run keeps db records");
}

#[test]
fn generation_manifest_and_profile_are_roots() {
    let tmp = TmpShade::new("gen");
    let db = tmp.db();
    let store = tmp.store();

    let (a_digest, a_name, a_path) = mk_entry(&store, 'a', "installed", &[("bin/a", b"x")]);
    db.register(&a_path, &a_digest, &a_name, &"aa".repeat(32), &[]).unwrap();
    let (d_digest, d_name, d_path) = mk_entry(&store, 'd', "dead", &[("f", b"x")]);
    db.register(&d_path, &d_digest, &d_name, &"dd".repeat(32), &[]).unwrap();

    // A generation manifest naming A's store path (the shade-gen canonical
    // record; the scan is byte-based and format-agnostic either way).
    let manifest = PathBuf::from(tmp.root()).join("gen/system/1/manifest");
    fs::create_dir_all(manifest.parent().unwrap()).unwrap();
    fs::write(
        &manifest,
        format!("shade-gen=1\npackage.0.path={store}/{a_name}\n"),
    )
    .unwrap();

    let report = db.gc(&GcOptions::default()).unwrap();
    assert!(exists(format!("{store}/{a_name}")), "generation-referenced A kept");
    assert!(!exists(format!("{store}/{d_name}")), "unreferenced D collected");
    assert!(report.collected.contains(&d_name));
}

// ── Lock atomicity (A1 exclusive-create through the seam) ─────────────────────

#[test]
fn db_lock_concurrent_acquire_has_exactly_one_winner() {
    // The lock primitive itself: N threads race create_exclusive on the same
    // path. Exactly one wins; every loser sees Exists — never a corrupted
    // "both hold it" state. On target the same call is SYS_CREATE, whose
    // exclusive-create guarantee the kernel boot probe verifies.
    let tmp = TmpShade::new("race");
    let lock_path = format!("{}/db/lock", tmp.root());
    fs::create_dir_all(format!("{}/db", tmp.root())).unwrap();

    const N: usize = 16;
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(N));
    let mut handles = Vec::new();
    for i in 0..N {
        let barrier = barrier.clone();
        let lock_path = lock_path.clone();
        handles.push(std::thread::spawn(move || {
            barrier.wait();
            let mut fs_backend = HostFs;
            fs_backend.create_exclusive(&lock_path, format!("{i}\n").as_bytes())
        }));
    }
    let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    let winners = results.iter().filter(|r| r.is_ok()).count();
    let losers = results
        .iter()
        .filter(|r| matches!(r, Err(FsError::Exists)))
        .count();
    assert_eq!(winners, 1, "exactly one holder: {results:?}");
    assert_eq!(losers, N - 1, "every loser gets Exists (EEXIST): {results:?}");
}

#[test]
fn held_db_lock_makes_second_acquire_busy_then_free_after_release() {
    let tmp = TmpShade::new("busy");
    let db_root = format!("{}/db", tmp.root());
    let lock_path = format!("{db_root}/lock");

    // First acquire wins.
    let mut fs_backend = HostFs;
    let guard = acquire_lock(&mut fs_backend, &db_root, &lock_path, 50).unwrap();
    // Second acquire spins past its (short) deadline and reports Busy.
    let err = acquire_lock(&mut HostFs, &db_root, &lock_path, 50).unwrap_err();
    assert!(matches!(err, DbError::Busy(_)), "{err}");
    // Releasing the guard removes db/lock; the next acquire succeeds.
    drop(guard);
    let guard2 = acquire_lock(&mut HostFs, &db_root, &lock_path, 50).unwrap();
    drop(guard2);
    assert!(!exists(&lock_path), "lock file removed on release");
}
