//! Mount-table routing tests (host, `MemBackend`). Cover the stage-1
//! requirements: mount registers a backend, resolution crosses the mount
//! boundary correctly, double-mount is rejected, a mount point with no parent
//! is rejected, and the root FS is unaffected.

use alloc::boxed::Box;

use super::*;
use crate::backend::FsBackend;
use crate::testutil::MemBackend;

/// A table with a root `MemBackend` mounted at `/`, containing an empty `/mnt`
/// directory ready to receive a mount.
fn table_with_root() -> MountTable {
    let mut root = MemBackend::new();
    root.mkdirs("/mnt");
    let mut t = MountTable::new();
    t.mount("/", Box::new(root)).unwrap();
    t
}

fn readdir_names(t: &mut MountTable, path: &str) -> alloc::vec::Vec<alloc::string::String> {
    let (be, rel) = t.resolve(path).unwrap();
    let mut names: alloc::vec::Vec<_> = be.readdir(&rel).unwrap().into_iter().map(|e| e.name).collect();
    names.sort();
    names
}

#[test]
fn resolve_without_root_is_not_mounted() {
    let mut t = MountTable::new();
    match t.resolve("/x") {
        Err(MountError::NotMounted) => {}
        Err(other) => panic!("expected NotMounted, got {other:?}"),
        Ok(_) => panic!("expected NotMounted, got Ok"),
    }
}

#[test]
fn mount_registers_backend() {
    let mut t = table_with_root();
    // Mount a second, distinct backend at /mnt.
    let second = MemBackend::new();
    t.mount("/mnt", Box::new(second)).unwrap();
    assert_eq!(t.len(), 2);
    assert!(t.is_mounted_at("/mnt"));

    // A file created through /mnt lands in the second backend, not root.
    {
        let (be, rel) = t.resolve("/mnt/file").unwrap();
        assert_eq!(rel, "/file");
        be.create(&rel).unwrap();
        be.commit().unwrap();
    }
    // Visible via the mount (the file lives in the second backend, reached
    // only by crossing the boundary — the root backend never sees it).
    assert_eq!(readdir_names(&mut t, "/mnt"), alloc::vec!["file"]);
}

#[test]
fn resolution_crosses_boundary() {
    let mut t = table_with_root();
    t.mount("/mnt", Box::new(MemBackend::new())).unwrap();

    // Create /a on root and /mnt/b on the second backend.
    {
        let (be, rel) = t.resolve("/a").unwrap();
        assert_eq!(rel, "/a");
        be.create("/a").unwrap();
        be.commit().unwrap();
    }
    {
        let (be, rel) = t.resolve("/mnt/b").unwrap();
        assert_eq!(rel, "/b");
        be.create("/b").unwrap();
        be.commit().unwrap();
    }

    // Root sees a + mnt (the mount-point dir), never b.
    assert_eq!(readdir_names(&mut t, "/"), alloc::vec!["a", "mnt"]);
    // The /mnt backend sees b, never a.
    assert_eq!(readdir_names(&mut t, "/mnt"), alloc::vec!["b"]);
}

#[test]
fn longest_prefix_not_string_prefix() {
    // A mount at /shade must not capture /shadestore (component-wise routing).
    let mut root = MemBackend::new();
    root.mkdirs("/shade");
    root.create("/shadestore").unwrap();
    let mut t = MountTable::new();
    t.mount("/", Box::new(root)).unwrap();
    t.mount("/shade", Box::new(MemBackend::new())).unwrap();

    // /shade/x → the /shade mount, rel /x
    let (_, rel) = t.resolve("/shade/x").unwrap();
    assert_eq!(rel, "/x");
    // /shadestore → still the root mount, rel /shadestore
    let (_, rel) = t.resolve("/shadestore").unwrap();
    assert_eq!(rel, "/shadestore");
}

#[test]
fn double_mount_rejected() {
    let mut t = table_with_root();
    t.mount("/mnt", Box::new(MemBackend::new())).unwrap();
    match t.mount("/mnt", Box::new(MemBackend::new())) {
        Err(MountError::AlreadyMounted) => {}
        other => panic!("expected AlreadyMounted, got {other:?}"),
    }
    // Re-mounting root also rejected.
    match t.mount("/", Box::new(MemBackend::new())) {
        Err(MountError::AlreadyMounted) => {}
        other => panic!("expected AlreadyMounted for root, got {other:?}"),
    }
}

#[test]
fn mount_missing_parent_rejected() {
    let mut t = table_with_root();
    // No /nope directory on root.
    match t.mount("/nope", Box::new(MemBackend::new())) {
        Err(MountError::NoSuchPath) => {}
        other => panic!("expected NoSuchPath, got {other:?}"),
    }
}

#[test]
fn mount_on_file_is_notdir() {
    let mut t = table_with_root();
    {
        let (be, _) = t.resolve("/afile").unwrap();
        be.create("/afile").unwrap();
    }
    match t.mount("/afile", Box::new(MemBackend::new())) {
        Err(MountError::NotDir) => {}
        other => panic!("expected NotDir, got {other:?}"),
    }
}

#[test]
fn mount_without_root_rejected() {
    // Non-root mount with no covering filesystem.
    let mut t = MountTable::new();
    match t.mount("/mnt", Box::new(MemBackend::new())) {
        Err(MountError::NoSuchPath) => {}
        other => panic!("expected NoSuchPath, got {other:?}"),
    }
}

#[test]
fn root_unaffected_by_second_mount() {
    let mut t = table_with_root();
    // Snapshot root state via a file created before the second mount.
    {
        let (be, _) = t.resolve("/keep").unwrap();
        be.create("/keep").unwrap();
        be.commit().unwrap();
    }
    let gen_before = {
        let (be, _) = t.resolve("/").unwrap();
        be.generation()
    };

    t.mount("/mnt", Box::new(MemBackend::new())).unwrap();

    // Root's file and generation are untouched by the mount operation.
    let (be, _) = t.resolve("/").unwrap();
    assert!(be.lookup("/keep").is_ok());
    assert_eq!(be.generation(), gen_before, "mounting must not mutate root");
}

#[test]
fn unmount_removes_route() {
    let mut t = table_with_root();
    t.mount("/mnt", Box::new(MemBackend::new())).unwrap();
    assert!(t.is_mounted_at("/mnt"));
    t.unmount("/mnt").unwrap();
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
