//! vfs-core — the backend-agnostic heart of the OROS VFS.
//!
//! This crate holds the VFS logic that is **pure over an abstract filesystem
//! backend**, so it can be unit-tested on the host without a kernel, a block
//! device, or QEMU. The kernel (`kernel/src/vfs.rs`) supplies the concrete
//! [`FsBackend`] (an `rfs2::Rfs2` volume) and enforces capabilities on the
//! syscall boundary; nothing capability-related lives here.
//!
//! Two pieces (design: `docs/plans/mount-syscall-shade-store.md`):
//!
//! - [`MountTable`] — a generic mount table with **longest-prefix** path
//!   routing. Multiple backends mount at distinct paths; a lookup resolves to
//!   the covering backend plus the backend-relative path. Not a special-cased
//!   path check — N mounts, first-class.
//! - [`RealizeGuard`] — read-only-after-realize for the `/shade/store` mount:
//!   temp paths are writable, the atomic rename onto a `<digest>-<name>-
//!   <version>` name *seals* it, and the sealed contents are then immutable.
//!   Concurrent realizes of the same digest converge to one sealed object.
//!
//! `no_std + alloc`: the kernel links this; host tests run under `std` via the
//! standard test harness (which provides `std` regardless of `no_std`).

#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod backend;
pub mod mount;
pub mod realize;

#[cfg(test)]
mod testutil;

pub use backend::{DirEntry, FsBackend, FsError, InodeMeta};
pub use mount::{BackendId, BackendStore, MountError, MountId, MountTable};
pub use realize::{RealizeGuard, RenameOutcome};
