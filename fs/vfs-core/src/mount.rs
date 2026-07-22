//! The mount table: longest-prefix path routing over [`FsBackend`]s.
//!
//! Generic — any number of backends mount at distinct paths. A path resolves
//! to the covering mount (the one whose mount point is the **longest
//! component-prefix** of the path) plus the backend-relative remainder. Root
//! (`/`) is just the mount with the empty prefix; `/shade/store` is a longer
//! prefix that wins for paths beneath it. No path is special-cased.

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::backend::{FsBackend, FsError};

/// Why a mount-table operation failed. The kernel folds these to errno
/// (`docs/plans/mount-syscall-shade-store.md` §3.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MountError {
    /// Mount point / path is not a valid absolute path.
    Invalid,
    /// A mount already exists at this exact mount point.
    AlreadyMounted,
    /// No covering filesystem holds the mount point, or its parent path does
    /// not exist there.
    NoSuchPath,
    /// The mount point exists but is not a directory.
    NotDir,
    /// No mount covers the path (not even root), or unmount of an absent point.
    NotMounted,
    /// The covering backend raised an error while validating the mount point.
    Backend(FsError),
}

/// Stable, global identity of one mounted backend instance. Assigned by
/// [`BackendStore`] at install and copied (`Copy`) into every [`MountTable`]
/// route that reaches it, so it stays valid across routing changes — and, when
/// per-task mount namespaces land, across the several tables that share one
/// backend (docs/plans/per-task-mount-namespace.md §5.1).
pub type BackendId = u64;

/// Back-compat alias for the identity the kernel fd table records to re-address
/// a backend. That identity **is** the [`BackendId`]: one value playing two
/// roles — a mount's routing target *and* an open fd's / realize guard's
/// re-addressing key. Kept as an alias so call sites reasoning about "which
/// mount" and "which backend" name the same thing (§5.1 folds the old
/// `MountId`-uniqueness question into the `BackendId` scheme).
pub type MountId = BackendId;

/// The single owner of every live backend, keyed by [`BackendId`]. Held by the
/// VFS state as a field **disjoint** from any [`MountTable`]: resolving a route
/// borrows only the table (and returns a `Copy` [`BackendId`], holding no
/// backend borrow), while taking a backend `&mut` for one operation borrows
/// only this store. That disjointness is the structural expression of the
/// "at most one `&mut`, for one operation, never across a nested resolve"
/// discipline (§5.1) — satisfied by lexical scoping, with no `RefCell` and thus
/// no possible runtime borrow panic.
#[derive(Default)]
pub struct BackendStore {
    backends: BTreeMap<BackendId, Box<dyn FsBackend>>,
    next_id: BackendId,
}

impl BackendStore {
    pub fn new() -> Self {
        BackendStore { backends: BTreeMap::new(), next_id: 0 }
    }

    /// Install `backend`, assigning it a fresh monotonic [`BackendId`]. Ids are
    /// never reused, so a stale id (fd to a since-freed backend) resolves to
    /// `None` rather than aliasing a later mount.
    pub fn insert(&mut self, backend: Box<dyn FsBackend>) -> BackendId {
        let id = self.next_id;
        self.next_id += 1;
        self.backends.insert(id, backend);
        id
    }

    /// Borrow one backend by id for a single operation. `None` if it was freed
    /// underneath a still-open fd. The returned `&mut` is the *only* live
    /// borrow into the store for the duration of that one operation.
    pub fn get(&mut self, id: BackendId) -> Option<&mut (dyn FsBackend + 'static)> {
        self.backends.get_mut(&id).map(|b| b.as_mut())
    }

    /// Free a backend, returning its box so the caller can drop (unmount) it.
    /// Call only once no [`MountTable`] still routes to `id`
    /// ([`MountTable::routes_to`]).
    pub fn remove(&mut self, id: BackendId) -> Option<Box<dyn FsBackend>> {
        self.backends.remove(&id)
    }

    /// Whether `id` still names a live backend.
    pub fn contains(&self, id: BackendId) -> bool {
        self.backends.contains_key(&id)
    }

    /// Number of live backends.
    pub fn len(&self) -> usize {
        self.backends.len()
    }

    pub fn is_empty(&self) -> bool {
        self.backends.is_empty()
    }
}

struct Mount {
    /// Canonical mount-point components (empty = root `/`).
    at: Vec<String>,
    /// The backend this prefix routes to — an id into the disjoint
    /// [`BackendStore`], never an owning pointer, so a table can be cloned
    /// (future namespace work) without copying a live filesystem.
    backend: BackendId,
}

/// A set of mounts with longest-prefix routing. Holds **only** routing metadata
/// (`prefix → BackendId`); the backends themselves live in the disjoint
/// [`BackendStore`]. Cloning this table (namespaces) is a memcpy of `Copy`
/// ids — it aliases no backend.
#[derive(Default)]
pub struct MountTable {
    mounts: Vec<Mount>,
}

/// Split an absolute path into non-empty components, or `None` if it is not
/// absolute. `"/"` and `""`-tail forms collapse to the empty component list.
fn components(path: &str) -> Option<Vec<&str>> {
    if !path.starts_with('/') {
        return None;
    }
    Some(path.split('/').filter(|c| !c.is_empty()).collect())
}

/// Is `prefix` a component-wise prefix of `full`?
fn is_prefix(prefix: &[String], full: &[&str]) -> bool {
    prefix.len() <= full.len() && prefix.iter().zip(full.iter()).all(|(a, b)| a == b)
}

/// Rebuild a backend-relative absolute path from the components after the
/// mount prefix. Empty tail → `"/"` (the backend root).
fn rel_path(tail: &[&str]) -> String {
    if tail.is_empty() {
        return "/".to_string();
    }
    let mut s = String::new();
    for c in tail {
        s.push('/');
        s.push_str(c);
    }
    s
}

impl MountTable {
    pub fn new() -> Self {
        MountTable { mounts: Vec::new() }
    }

    /// Number of active mounts.
    pub fn len(&self) -> usize {
        self.mounts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.mounts.is_empty()
    }

    /// Index of the mount whose prefix is the longest component-prefix of
    /// `comps`, if any.
    fn covering_index(&self, comps: &[&str]) -> Option<usize> {
        let mut best: Option<usize> = None;
        for (i, m) in self.mounts.iter().enumerate() {
            if is_prefix(&m.at, comps) {
                let better = match best {
                    None => true,
                    Some(b) => m.at.len() > self.mounts[b].at.len(),
                };
                if better {
                    best = Some(i);
                }
            }
        }
        best
    }

    /// Install `backend` at `at`, taking ownership of it in `backends` and
    /// recording a route `at → BackendId`. Returns the fresh [`BackendId`].
    ///
    /// - Root (`/`) may be mounted only when the table has no root yet.
    /// - A non-root mount requires a covering filesystem that already has the
    ///   mount point as an existing directory (checked via that backend's
    ///   `stat`).
    ///
    /// The backend is inserted into `backends` **only on success**; on any
    /// validation failure `backend` is dropped here (a clean unmount of a
    /// never-routed filesystem), so nothing leaks and no id is consumed. This
    /// is the single point where a backend enters the store, keeping id
    /// assignment and routing one indivisible step.
    pub fn mount(
        &mut self,
        at: &str,
        backend: Box<dyn FsBackend>,
        backends: &mut BackendStore,
    ) -> Result<BackendId, MountError> {
        let comps = components(at).ok_or(MountError::Invalid)?;
        let owned: Vec<String> = comps.iter().map(|c| c.to_string()).collect();

        if self.mounts.iter().any(|m| m.at == owned) {
            return Err(MountError::AlreadyMounted);
        }

        if !owned.is_empty() {
            // A covering backend must hold the mount point as a directory. The
            // covering mount is the longest *strict* prefix (len < comps.len()).
            // Resolve to its `BackendId` (Copy) first — the routing borrow ends
            // there — then borrow that one backend from the disjoint store for
            // the single `stat` op.
            let cover = self.covering_index(&comps).ok_or(MountError::NoSuchPath)?;
            let cover_id = self.mounts[cover].backend;
            let rel = rel_path(&comps[self.mounts[cover].at.len()..]);
            let cover_be = backends.get(cover_id).ok_or(MountError::NoSuchPath)?;
            match cover_be.stat(&rel) {
                Ok(meta) if meta.is_dir => {}
                Ok(_) => return Err(MountError::NotDir),
                Err(FsError::NotFound) | Err(FsError::NotDir) => {
                    return Err(MountError::NoSuchPath)
                }
                Err(e) => return Err(MountError::Backend(e)),
            }
        }

        let id = backends.insert(backend);
        self.mounts.push(Mount { at: owned, backend: id });
        Ok(id)
    }

    /// Resolve `path` to the [`BackendId`] of its covering mount and the
    /// backend-relative path. Pure routing: borrows `&self` only and returns a
    /// `Copy` id, holding **no** backend borrow — the caller then indexes the
    /// disjoint [`BackendStore`] for the one operation it performs. This is the
    /// first of the three lexical steps in the §5.1 borrow discipline.
    pub fn resolve(&self, path: &str) -> Result<(BackendId, String), MountError> {
        let comps = components(path).ok_or(MountError::Invalid)?;
        let idx = self.covering_index(&comps).ok_or(MountError::NotMounted)?;
        let rel = rel_path(&comps[self.mounts[idx].at.len()..]);
        Ok((self.mounts[idx].backend, rel))
    }

    /// Whether any route still reaches `backend`. Checked at unmount before
    /// freeing the backend from the [`BackendStore`], so a shared backend
    /// (future namespaces) is dropped only when its last route goes away —
    /// never leaving a dangling [`BackendId`] in a live route.
    pub fn routes_to(&self, backend: BackendId) -> bool {
        self.mounts.iter().any(|m| m.backend == backend)
    }

    /// Whether a mount is installed at exactly `at`.
    pub fn is_mounted_at(&self, at: &str) -> bool {
        match components(at) {
            Some(comps) => {
                let owned: Vec<String> = comps.iter().map(|c| c.to_string()).collect();
                self.mounts.iter().any(|m| m.at == owned)
            }
            None => false,
        }
    }

    /// Remove the route at exactly `at`, returning the [`BackendId`] it routed
    /// to. Does not check for mounts nested beneath it (the kernel policy layer
    /// decides whether that is allowed), and does **not** free the backend —
    /// the caller frees it from the [`BackendStore`] once [`routes_to`] reports
    /// no remaining route, so a backend shared by several tables outlives the
    /// removal of any one route.
    ///
    /// [`routes_to`]: Self::routes_to
    pub fn unmount(&mut self, at: &str) -> Result<BackendId, MountError> {
        let comps = components(at).ok_or(MountError::Invalid)?;
        let owned: Vec<String> = comps.iter().map(|c| c.to_string()).collect();
        let pos = self
            .mounts
            .iter()
            .position(|m| m.at == owned)
            .ok_or(MountError::NotMounted)?;
        Ok(self.mounts.remove(pos).backend)
    }
}

#[cfg(test)]
mod tests;
