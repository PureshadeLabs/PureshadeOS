//! Namespace-registry tests (host, `MemBackend`). Stage 1 covers the reachable
//! surface: the pinned root namespace, `NsId` lookup, routing/mount/unmount
//! delegated per namespace, and the `any_routes_to` teardown gate. Tests that
//! two namespaces share one backend (the load-bearing cross-namespace teardown)
//! arrive with the create/enter API in the next stage — there is no way to make
//! a second namespace yet, and this file does not fake one.

use alloc::boxed::Box;

use super::*;
use crate::mount::BackendStore;
use crate::testutil::MemBackend;

/// A registry whose root namespace has a `MemBackend` mounted at `/` (with an
/// empty `/mnt` dir ready to receive a mount), plus the backing store.
fn root_with_fs() -> (Namespaces, BackendStore) {
    let mut root = MemBackend::new();
    root.mkdirs("/mnt");
    let mut store = BackendStore::new();
    let mut ns = Namespaces::new();
    ns.root_table_mut().mount("/", Box::new(root), &mut store).unwrap();
    (ns, store)
}

#[test]
fn fresh_registry_holds_only_pinned_root() {
    let ns = Namespaces::new();
    assert!(ns.contains(ROOT_NS), "root namespace must exist from construction");
    assert!(!ns.contains(ROOT_NS + 1), "no other namespace exists yet");
    assert_eq!(ns.len(), 1);
    assert!(!ns.is_empty());
}

#[test]
fn resolve_in_empty_root_is_not_mounted() {
    // Root exists but nothing is mounted into it: no covering mount.
    let ns = Namespaces::new();
    match ns.resolve(ROOT_NS, "/x") {
        Err(MountError::NotMounted) => {}
        other => panic!("expected NotMounted, got {other:?}"),
    }
}

#[test]
fn resolve_in_unknown_namespace_is_not_mounted() {
    // A namespace that was never created has no view, so nothing covers a path.
    let (ns, _store) = root_with_fs();
    match ns.resolve(999, "/") {
        Err(MountError::NotMounted) => {}
        other => panic!("expected NotMounted for unknown ns, got {other:?}"),
    }
}

#[test]
fn mount_resolve_roundtrip_in_root() {
    let (mut ns, mut store) = root_with_fs();
    let mnt_id = ns.mount(ROOT_NS, "/mnt", Box::new(MemBackend::new()), &mut store).unwrap();
    assert!(ns.is_mounted_at(ROOT_NS, "/mnt"));

    // Route crosses the boundary to the freshly-mounted backend.
    let (bid, rel) = ns.resolve(ROOT_NS, "/mnt/file").unwrap();
    assert_eq!(bid, mnt_id, "resolution addresses the /mnt backend by its id");
    assert_eq!(rel, "/file");

    // A file created through the resolved backend is invisible to root.
    {
        let be = store.get(bid).unwrap();
        be.create("/file").unwrap();
        be.commit().unwrap();
    }
    let (root_bid, _) = ns.resolve(ROOT_NS, "/").unwrap();
    assert_ne!(root_bid, mnt_id, "root and /mnt are distinct backends");
    let names: alloc::vec::Vec<_> =
        store.get(root_bid).unwrap().readdir("/").unwrap().into_iter().map(|e| e.name).collect();
    assert!(!names.iter().any(|n| n == "file"), "the /mnt write must not touch root");
}

#[test]
fn unmount_teardown_gate() {
    let (mut ns, mut store) = root_with_fs();
    let mnt_id = ns.mount(ROOT_NS, "/mnt", Box::new(MemBackend::new()), &mut store).unwrap();
    assert!(ns.any_routes_to(mnt_id), "a mounted backend is routed to");

    // Drop the route; the backend is not yet freed.
    let freed = ns.unmount(ROOT_NS, "/mnt").unwrap();
    assert_eq!(freed, mnt_id);
    assert!(!ns.is_mounted_at(ROOT_NS, "/mnt"));
    assert!(
        !ns.any_routes_to(freed),
        "no namespace may still route to the unmounted backend"
    );
    // The teardown gate now permits freeing it from the store.
    assert!(store.remove(freed).is_some(), "backend freed cleanly once unrouted");
    assert!(!store.contains(freed));

    // Root is unaffected: /mnt now routes back to root.
    let (_, rel) = ns.resolve(ROOT_NS, "/mnt/x").unwrap();
    assert_eq!(rel, "/mnt/x");
}

#[test]
fn unmount_absent_is_not_mounted() {
    let (mut ns, _store) = root_with_fs();
    match ns.unmount(ROOT_NS, "/mnt") {
        Err(MountError::NotMounted) => {}
        other => panic!("expected NotMounted, got {other:?}"),
    }
}
