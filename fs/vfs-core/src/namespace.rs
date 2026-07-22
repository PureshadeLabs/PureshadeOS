//! Per-task mount namespaces: a registry of independent [`MountTable`] routing
//! views over the one shared [`BackendStore`].
//!
//! Design: `docs/plans/per-task-mount-namespace.md`. A **namespace** is a
//! `MountTable` (prefix → `BackendId` routes) plus a refcount; the backends it
//! routes to live in the single global [`BackendStore`], shared by reference
//! across every namespace (§5.1). Cloning a namespace's routing is therefore a
//! memcpy of `Copy` ids, never a copy of a live filesystem.
//!
//! **Stage 1 (this landing)** introduces the registry, `NsId` allocation, and
//! the pinned root namespace [`ROOT_NS`] that holds the current global view —
//! so boot and the `/shade/store` mount are unaffected (the doc's migration
//! section). The create / enter / teardown operations (§1.1, §4) land in a
//! later stage; this module deliberately ships only the data structures plus
//! the routing/teardown helpers that `SYS_UNMOUNT` needs.
//!
//! Borrow discipline (§5.1) is preserved by construction: [`resolve`] borrows
//! `&self` and returns owned `(BackendId, String)` holding **no** backend
//! borrow, and [`mount`] takes the disjoint [`BackendStore`] by `&mut` so the
//! two borrows at a kernel call site name different fields.
//!
//! [`resolve`]: Namespaces::resolve
//! [`mount`]: Namespaces::mount

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::string::String;

use crate::backend::FsBackend;
use crate::mount::{BackendId, BackendStore, MountError, MountTable};

/// Identity of one mount namespace. [`ROOT_NS`] (0) is the pinned system view.
pub type NsId = u64;

/// The root / initial namespace: holds `/` and `/shade/store`, pinned so it can
/// never be torn down (§4 root-namespace pinning). Every task boots in it; the
/// pre-namespace system is exactly "everyone shares `ROOT_NS`".
pub const ROOT_NS: NsId = 0;

struct Namespace {
    table: MountTable,
    /// How many tasks currently point at this namespace. Teardown (the next
    /// stage's create/enter/exit lifecycle) frees the namespace when this
    /// reaches 0; `ROOT_NS` is pinned so its count never drops below 1. Carried
    /// from stage 1 so the data structure is complete; the lifecycle that reads
    /// it lands next.
    #[allow(dead_code)]
    refcount: u32,
}

/// The registry of live namespaces, keyed by [`NsId`]. Owns the per-namespace
/// routing tables; the backends they route to live in the disjoint global
/// [`BackendStore`], so a mount/unmount here only ever moves `Copy` ids.
pub struct Namespaces {
    map: BTreeMap<NsId, Namespace>,
    /// Monotonic `NsId` allocator; ids are never reused. Consumed by
    /// `SYS_NS_CREATE` in the next stage — carried now so allocation is a
    /// single, obvious extension point.
    #[allow(dead_code)]
    next_id: NsId,
}

impl Default for Namespaces {
    fn default() -> Self {
        Self::new()
    }
}

impl Namespaces {
    /// A fresh registry holding only the pinned, empty root namespace. The
    /// kernel then mounts `/` (and later `/shade/store`) into it via
    /// [`root_table_mut`](Self::root_table_mut).
    pub fn new() -> Self {
        let mut map = BTreeMap::new();
        map.insert(ROOT_NS, Namespace { table: MountTable::new(), refcount: 1 });
        Namespaces { map, next_id: ROOT_NS + 1 }
    }

    /// Mutable access to the root namespace's table, for the boot-time mounts
    /// (`/`, `/shade/store`) that populate the initial global view. Root is
    /// pinned, so this never faults.
    pub fn root_table_mut(&mut self) -> &mut MountTable {
        &mut self.map.get_mut(&ROOT_NS).expect("root namespace is pinned").table
    }

    /// Whether `ns` names a live namespace.
    pub fn contains(&self, ns: NsId) -> bool {
        self.map.contains_key(&ns)
    }

    /// Number of live namespaces.
    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// Resolve `path` in namespace `ns` to the covering backend's [`BackendId`]
    /// and the backend-relative path. Pure routing over that namespace's table:
    /// borrows `&self`, returns owned values holding **no** backend borrow — the
    /// caller then indexes the disjoint [`BackendStore`] for its one operation.
    /// A missing namespace resolves to [`MountError::NotMounted`] (no view ⇒
    /// nothing covers the path).
    pub fn resolve(&self, ns: NsId, path: &str) -> Result<(BackendId, String), MountError> {
        self.map.get(&ns).ok_or(MountError::NotMounted)?.table.resolve(path)
    }

    /// Install `backend` at `at` in namespace `ns`, recording the route and
    /// taking ownership of the backend in `store`. Delegates to
    /// [`MountTable::mount`]; the two `&mut` borrows at the call site (this
    /// registry, the store) name disjoint kernel fields, so the §5.1 discipline
    /// holds by lexical scoping.
    pub fn mount(
        &mut self,
        ns: NsId,
        at: &str,
        backend: Box<dyn FsBackend>,
        store: &mut BackendStore,
    ) -> Result<BackendId, MountError> {
        self.map.get_mut(&ns).ok_or(MountError::NotMounted)?.table.mount(at, backend, store)
    }

    /// Remove the route at exactly `at` in namespace `ns`, returning the
    /// [`BackendId`] it named. Does **not** free the backend — the caller frees
    /// it from the [`BackendStore`] only once [`any_routes_to`] reports no
    /// namespace still routes to it, so a backend shared across namespaces
    /// outlives the removal of any one route.
    ///
    /// [`any_routes_to`]: Self::any_routes_to
    pub fn unmount(&mut self, ns: NsId, at: &str) -> Result<BackendId, MountError> {
        self.map.get_mut(&ns).ok_or(MountError::NotMounted)?.table.unmount(at)
    }

    /// Whether namespace `ns` has a mount at exactly `at`.
    pub fn is_mounted_at(&self, ns: NsId, at: &str) -> bool {
        self.map.get(&ns).is_some_and(|n| n.table.is_mounted_at(at))
    }

    /// Whether **any** live namespace still routes to `backend`. The teardown
    /// gate: a backend is freed from the [`BackendStore`] only when this is
    /// false, so a backend shared by several namespaces is never freed out from
    /// under another namespace's still-live route (§5.1 teardown invariant).
    /// With one root namespace (stage 1) this is exactly that table's
    /// `routes_to`; it becomes load-bearing once namespaces share backends.
    pub fn any_routes_to(&self, backend: BackendId) -> bool {
        self.map.values().any(|n| n.table.routes_to(backend))
    }
}

#[cfg(test)]
mod tests;
