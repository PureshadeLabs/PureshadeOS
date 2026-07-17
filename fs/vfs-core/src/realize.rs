//! Read-only-after-realize for the `/shade/store` mount.
//!
//! Store paths are `<digest>-<name>-<version>` (the "store name" — the first
//! path component under the store mount). A store name is **sealed** the moment
//! a temp directory is atomically renamed onto it; once sealed, its contents
//! are immutable. This mirrors `shade_store::realize`: "exists ⇒ complete",
//! writes never overwrite, re-realize is a no-op.
//!
//! The guard is **pure over backend-relative path strings** (paths already
//! stripped of the `/shade/store` mount prefix by the mount table). It holds no
//! backend and enforces no capabilities — the kernel glue consults it before
//! delegating a mutation to the store backend, and calls [`seal`](RealizeGuard::seal)
//! after a successful realize rename.
//!
//! ## Concurrency (design §2.3)
//!
//! Two writers realizing the same digest are, by input-addressing, writing
//! identical bytes. They converge to one sealed object:
//!
//! 1. temp writes are per-writer — distinct `.tmp-*` names, no collision;
//! 2. the atomic rename onto the final name is the sole commit/seal point;
//! 3. the first rename wins and seals;
//! 4. a second rename onto an already-sealed name is a **no-op success**
//!    ([`RenameOutcome::NoOp`]) — not `ReadOnly` — because it is the same
//!    content at the same digest. The loser drops its redundant temp.
//!
//! This is distinct from *writing into* a sealed path, which is always
//! [`FsError::ReadOnly`].

use alloc::collections::BTreeSet;
use alloc::string::{String, ToString};

use crate::backend::{FsError, FsResult};

/// What the caller should do about a realize rename onto the store mount.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenameOutcome {
    /// The destination store name is fresh: perform the rename, then
    /// [`seal`](RealizeGuard::seal) `store_name`.
    Proceed { store_name: String },
    /// The destination store name is already sealed and the destination is
    /// exactly that top-level name: idempotent re-realize. Do **not** rename;
    /// drop the redundant temp. No error.
    NoOp,
}

/// The set of sealed store names on one mount.
#[derive(Debug, Default, Clone)]
pub struct RealizeGuard {
    sealed: BTreeSet<String>,
}

/// The first component of a backend-relative path (`"/a/b"` → `Some("a")`),
/// or `None` for the mount root (`"/"`).
pub fn top_component(rel: &str) -> Option<&str> {
    rel.split('/').find(|c| !c.is_empty())
}

/// Does `rel` have more than one component (i.e. point *inside* its top-level
/// entry rather than at it)?
fn is_nested(rel: &str) -> bool {
    rel.split('/').filter(|c| !c.is_empty()).count() > 1
}

impl RealizeGuard {
    pub fn new() -> Self {
        RealizeGuard { sealed: BTreeSet::new() }
    }

    /// Whether `store_name` (a top-level component) is sealed.
    pub fn is_sealed_name(&self, store_name: &str) -> bool {
        self.sealed.contains(store_name)
    }

    /// Whether `rel` lies under a sealed store name.
    pub fn is_sealed_path(&self, rel: &str) -> bool {
        top_component(rel).is_some_and(|c| self.sealed.contains(c))
    }

    /// Number of sealed entries.
    pub fn sealed_count(&self) -> usize {
        self.sealed.len()
    }

    /// Seal a store name (called after a successful realize rename). Idempotent.
    pub fn seal(&mut self, store_name: &str) {
        self.sealed.insert(store_name.to_string());
    }

    /// Forget a store name as the final step of whole-path removal (the kernel
    /// `store_remove_tree` / SYS_STORE_REMOVE lifecycle). This is **not** an
    /// unseal: it is never a way to make sealed content writable — the seal is
    /// absolute and has no in-place exit. It is called only once the entire
    /// tree has been deleted below the seal, so the now-nonexistent name should
    /// no longer be reported sealed. Content-addressing keeps removal safe: a
    /// later realize of the same digest reproduces byte-identical bytes.
    /// Returns whether `store_name` was sealed.
    pub fn forget(&mut self, store_name: &str) -> bool {
        self.sealed.remove(store_name)
    }

    /// Gate a write / create / mkdir / unlink whose target is `rel`. Rejected
    /// with [`FsError::ReadOnly`] iff `rel` lies under a sealed store name;
    /// temp / unsealed paths are writable.
    pub fn check_mutate(&self, rel: &str) -> FsResult<()> {
        if self.is_sealed_path(rel) {
            Err(FsError::ReadOnly)
        } else {
            Ok(())
        }
    }

    /// Gate a `rename(src → dst)` on the store mount — the realize commit point.
    ///
    /// - Moving a *sealed* source is forbidden (immutability): `ReadOnly`.
    /// - Renaming *into* a sealed entry (nested destination) is forbidden:
    ///   `ReadOnly`.
    /// - Renaming onto an already-sealed top-level name is the idempotent
    ///   re-realize: [`RenameOutcome::NoOp`].
    /// - Otherwise [`RenameOutcome::Proceed`]; the caller renames and then
    ///   seals the destination store name.
    pub fn check_rename(&self, src_rel: &str, dst_rel: &str) -> FsResult<RenameOutcome> {
        // A sealed object is immutable — it cannot be moved away.
        if self.is_sealed_path(src_rel) {
            return Err(FsError::ReadOnly);
        }
        let dst_name = top_component(dst_rel).ok_or(FsError::Invalid)?;
        if self.sealed.contains(dst_name) {
            if is_nested(dst_rel) {
                // Writing a new path *inside* a sealed object.
                return Err(FsError::ReadOnly);
            }
            // Rename onto the exact sealed name: idempotent no-op.
            return Ok(RenameOutcome::NoOp);
        }
        Ok(RenameOutcome::Proceed { store_name: dst_name.to_string() })
    }
}

#[cfg(test)]
mod tests;
