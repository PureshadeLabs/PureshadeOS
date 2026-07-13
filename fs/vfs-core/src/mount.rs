//! The mount table: longest-prefix path routing over [`FsBackend`]s.
//!
//! Generic — any number of backends mount at distinct paths. A path resolves
//! to the covering mount (the one whose mount point is the **longest
//! component-prefix** of the path) plus the backend-relative remainder. Root
//! (`/`) is just the mount with the empty prefix; `/shade/store` is a longer
//! prefix that wins for paths beneath it. No path is special-cased.

use alloc::boxed::Box;
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

/// Stable identity of one mount, valid until that mount is unmounted. The
/// kernel's fd table records this so an open file keeps addressing the right
/// backend even as other mounts come and go.
pub type MountId = u64;

struct Mount {
    id: MountId,
    /// Canonical mount-point components (empty = root `/`).
    at: Vec<String>,
    backend: Box<dyn FsBackend>,
}

/// A set of mounts with longest-prefix routing.
#[derive(Default)]
pub struct MountTable {
    mounts: Vec<Mount>,
    next_id: MountId,
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
        MountTable { mounts: Vec::new(), next_id: 0 }
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

    /// Install `backend` at `at`.
    ///
    /// - Root (`/`) may be mounted only when the table has no root yet.
    /// - A non-root mount requires a covering filesystem that already has the
    ///   mount point as an existing directory (checked via that backend's
    ///   `stat`).
    pub fn mount(&mut self, at: &str, backend: Box<dyn FsBackend>) -> Result<MountId, MountError> {
        let comps = components(at).ok_or(MountError::Invalid)?;
        let owned: Vec<String> = comps.iter().map(|c| c.to_string()).collect();

        if self.mounts.iter().any(|m| m.at == owned) {
            return Err(MountError::AlreadyMounted);
        }

        if !owned.is_empty() {
            // A covering backend must hold the mount point as a directory. The
            // covering mount is the longest *strict* prefix (len < comps.len()).
            let cover = self.covering_index(&comps).ok_or(MountError::NoSuchPath)?;
            let rel = rel_path(&comps[self.mounts[cover].at.len()..]);
            match self.mounts[cover].backend.stat(&rel) {
                Ok(meta) if meta.is_dir => {}
                Ok(_) => return Err(MountError::NotDir),
                Err(FsError::NotFound) | Err(FsError::NotDir) => {
                    return Err(MountError::NoSuchPath)
                }
                Err(e) => return Err(MountError::Backend(e)),
            }
        }

        let id = self.next_id;
        self.next_id += 1;
        self.mounts.push(Mount { id, at: owned, backend });
        Ok(id)
    }

    /// Resolve `path` to its covering backend and the backend-relative path.
    pub fn resolve(&mut self, path: &str) -> Result<(&mut dyn FsBackend, String), MountError> {
        let (_, be, rel) = self.resolve_full(path)?;
        Ok((be, rel))
    }

    /// [`resolve`](Self::resolve), also returning the covering mount's stable
    /// [`MountId`] — for callers (the kernel fd table) that must re-address the
    /// same backend later without re-walking a path.
    pub fn resolve_full(
        &mut self,
        path: &str,
    ) -> Result<(MountId, &mut dyn FsBackend, String), MountError> {
        let comps = components(path).ok_or(MountError::Invalid)?;
        let idx = self.covering_index(&comps).ok_or(MountError::NotMounted)?;
        let rel = rel_path(&comps[self.mounts[idx].at.len()..]);
        let m = &mut self.mounts[idx];
        Ok((m.id, m.backend.as_mut(), rel))
    }

    /// The backend of a live mount by id, or `None` if it was unmounted.
    pub fn backend_mut(&mut self, id: MountId) -> Option<&mut (dyn FsBackend + 'static)> {
        self.mounts
            .iter_mut()
            .find(|m| m.id == id)
            .map(move |m| m.backend.as_mut())
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

    /// Remove the mount at exactly `at`. Does not check for mounts nested
    /// beneath it (the kernel policy layer decides whether that is allowed).
    pub fn unmount(&mut self, at: &str) -> Result<Box<dyn FsBackend>, MountError> {
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
