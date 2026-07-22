//! Mount-table routing tests (host, `MemBackend`). Cover the stage-1
//! requirements: mount registers a backend, resolution crosses the mount
//! boundary correctly, double-mount is rejected, a mount point with no parent
//! is rejected, and the root FS is unaffected.
//!
//! Since the §5.1 refactor the table holds only `prefix → BackendId` routes and
//! the backends live in a disjoint [`BackendStore`]; each test threads both, so
//! the tests also exercise the borrow discipline they encode — resolve to a
//! `Copy` id, then borrow that one backend from the store for one operation.

use alloc::boxed::Box;

use super::*;
use crate::backend::FsBackend;
use crate::testutil::MemBackend;

/// A `(table, store)` pair with a root `MemBackend` mounted at `/`, containing
/// an empty `/mnt` directory ready to receive a mount.
fn table_with_root() -> (MountTable, BackendStore) {
    let mut root = MemBackend::new();
    root.mkdirs("/mnt");
    let mut store = BackendStore::new();
    let mut t = MountTable::new();
    t.mount("/", Box::new(root), &mut store).unwrap();
    (t, store)
}

/// Resolve `path` and borrow the covering backend for one op — the two-step the
/// kernel handlers follow (route → `BackendId` → backend borrow).
fn readdir_names(
    t: &MountTable,
    store: &mut BackendStore,
    path: &str,
) -> alloc::vec::Vec<alloc::string::String> {
    let (bid, rel) = t.resolve(path).unwrap();
    let be = store.get(bid).unwrap();
    let mut names: alloc::vec::Vec<_> =
        be.readdir(&rel).unwrap().into_iter().map(|e| e.name).collect();
    names.sort();
    names
}

#[test]
fn resolve_without_root_is_not_mounted() {
    let t = MountTable::new();
    match t.resolve("/x") {
        Err(MountError::NotMounted) => {}
        Err(other) => panic!("expected NotMounted, got {other:?}"),
        Ok(_) => panic!("expected NotMounted, got Ok"),
    }
}

#[test]
fn mount_registers_backend() {
    let (mut t, mut store) = table_with_root();
    // Mount a second, distinct backend at /mnt.
    let second = MemBackend::new();
    let mnt_id = t.mount("/mnt", Box::new(second), &mut store).unwrap();
    assert_eq!(t.len(), 2);
    assert_eq!(store.len(), 2, "the store owns exactly the two live backends");
    assert!(t.is_mounted_at("/mnt"));

    // The route names the freshly-assigned backend id.
    let (bid, rel) = t.resolve("/mnt/file").unwrap();
    assert_eq!(bid, mnt_id, "resolution addresses the /mnt backend by its id");
    assert_eq!(rel, "/file");
    // A file created through /mnt lands in the second backend, not root.
    {
        let be = store.get(bid).unwrap();
        be.create(&rel).unwrap();
        be.commit().unwrap();
    }
    // Visible via the mount (the file lives in the second backend, reached
    // only by crossing the boundary — the root backend never sees it).
    assert_eq!(readdir_names(&t, &mut store, "/mnt"), alloc::vec!["file"]);
}

#[test]
fn resolution_crosses_boundary() {
    let (mut t, mut store) = table_with_root();
    t.mount("/mnt", Box::new(MemBackend::new()), &mut store).unwrap();

    // Create /a on root and /mnt/b on the second backend.
    {
        let (bid, rel) = t.resolve("/a").unwrap();
        assert_eq!(rel, "/a");
        let be = store.get(bid).unwrap();
        be.create("/a").unwrap();
        be.commit().unwrap();
    }
    {
        let (bid, rel) = t.resolve("/mnt/b").unwrap();
        assert_eq!(rel, "/b");
        let be = store.get(bid).unwrap();
        be.create("/b").unwrap();
        be.commit().unwrap();
    }

    // Root sees a + mnt (the mount-point dir), never b.
    assert_eq!(readdir_names(&t, &mut store, "/"), alloc::vec!["a", "mnt"]);
    // The /mnt backend sees b, never a.
    assert_eq!(readdir_names(&t, &mut store, "/mnt"), alloc::vec!["b"]);
}

#[test]
fn longest_prefix_not_string_prefix() {
    // A mount at /shade must not capture /shadestore (component-wise routing).
    let mut root = MemBackend::new();
    root.mkdirs("/shade");
    root.create("/shadestore").unwrap();
    let mut store = BackendStore::new();
    let mut t = MountTable::new();
    t.mount("/", Box::new(root), &mut store).unwrap();
    t.mount("/shade", Box::new(MemBackend::new()), &mut store).unwrap();

    // /shade/x → the /shade mount, rel /x
    let (_, rel) = t.resolve("/shade/x").unwrap();
    assert_eq!(rel, "/x");
    // /shadestore → still the root mount, rel /shadestore
    let (_, rel) = t.resolve("/shadestore").unwrap();
    assert_eq!(rel, "/shadestore");
}

#[test]
fn double_mount_rejected() {
    let (mut t, mut store) = table_with_root();
    t.mount("/mnt", Box::new(MemBackend::new()), &mut store).unwrap();
    let backends_before = store.len();
    match t.mount("/mnt", Box::new(MemBackend::new()), &mut store) {
        Err(MountError::AlreadyMounted) => {}
        other => panic!("expected AlreadyMounted, got {other:?}"),
    }
    // Re-mounting root also rejected.
    match t.mount("/", Box::new(MemBackend::new()), &mut store) {
        Err(MountError::AlreadyMounted) => {}
        other => panic!("expected AlreadyMounted for root, got {other:?}"),
    }
    // A rejected mount drops its backend — it never enters the store.
    assert_eq!(store.len(), backends_before, "rejected mounts leak no backend");
}

#[test]
fn mount_missing_parent_rejected() {
    let (mut t, mut store) = table_with_root();
    let backends_before = store.len();
    // No /nope directory on root.
    match t.mount("/nope", Box::new(MemBackend::new()), &mut store) {
        Err(MountError::NoSuchPath) => {}
        other => panic!("expected NoSuchPath, got {other:?}"),
    }
    assert_eq!(store.len(), backends_before, "a failed mount leaks no backend");
}

#[test]
fn mount_on_file_is_notdir() {
    let (mut t, mut store) = table_with_root();
    {
        let (bid, _) = t.resolve("/afile").unwrap();
        store.get(bid).unwrap().create("/afile").unwrap();
    }
    match t.mount("/afile", Box::new(MemBackend::new()), &mut store) {
        Err(MountError::NotDir) => {}
        other => panic!("expected NotDir, got {other:?}"),
    }
}

#[test]
fn mount_without_root_rejected() {
    // Non-root mount with no covering filesystem.
    let mut store = BackendStore::new();
    let mut t = MountTable::new();
    match t.mount("/mnt", Box::new(MemBackend::new()), &mut store) {
        Err(MountError::NoSuchPath) => {}
        other => panic!("expected NoSuchPath, got {other:?}"),
    }
    assert!(store.is_empty(), "a failed mount leaks no backend");
}

#[test]
fn root_unaffected_by_second_mount() {
    let (mut t, mut store) = table_with_root();
    // Snapshot root state via a file created before the second mount.
    {
        let (bid, _) = t.resolve("/keep").unwrap();
        let be = store.get(bid).unwrap();
        be.create("/keep").unwrap();
        be.commit().unwrap();
    }
    let gen_before = {
        let (bid, _) = t.resolve("/").unwrap();
        store.get(bid).unwrap().generation()
    };

    t.mount("/mnt", Box::new(MemBackend::new()), &mut store).unwrap();

    // Root's file and generation are untouched by the mount operation.
    let (bid, _) = t.resolve("/").unwrap();
    let be = store.get(bid).unwrap();
    assert!(be.lookup("/keep").is_ok());
    assert_eq!(be.generation(), gen_before, "mounting must not mutate root");
}

#[test]
fn unmount_removes_route() {
    let (mut t, mut store) = table_with_root();
    let mnt_id = t.mount("/mnt", Box::new(MemBackend::new()), &mut store).unwrap();
    assert!(t.is_mounted_at("/mnt"));

    // Unmount returns the backend id the route named; teardown frees it from
    // the store only once no route still reaches it (there is none here).
    let freed = t.unmount("/mnt").unwrap();
    assert_eq!(freed, mnt_id);
    assert!(!t.routes_to(freed), "no route may dangle to the unmounted backend");
    assert!(store.remove(freed).is_some(), "the backend is freed cleanly");
    assert!(!store.contains(freed));

    assert!(!t.is_mounted_at("/mnt"));
    // /mnt now routes back to root (rel "/mnt").
    let (_, rel) = t.resolve("/mnt/x").unwrap();
    assert_eq!(rel, "/mnt/x");
    match t.unmount("/mnt") {
        Err(MountError::NotMounted) => {}
        Err(other) => panic!("expected NotMounted, got {other:?}"),
        Ok(_) => panic!("expected NotMounted, got Ok"),
    }
}
