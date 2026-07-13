//! The abstract filesystem backend the mount table routes to.
//!
//! [`FsBackend`] is object-safe so a `Box<dyn FsBackend>` can sit in the mount
//! table regardless of the concrete backend's generics (the kernel's is
//! `rfs2::Rfs2<D, T>`; tests use a `MemBackend`). It exposes exactly the
//! operations the VFS surface needs — the same calls `kernel/src/vfs.rs`
//! already makes on `Rfs2` — and nothing more.
//!
//! [`FsError`] is vfs-core's own error surface. The kernel maps `rfs2::Error`
//! into it in its `FsBackend` impl; keeping a local enum means this crate has
//! no dependency on `rfs2` and its tests are self-contained. The variants line
//! up with the errno fold in `docs/plans/mount-syscall-shade-store.md` §3.5.

use alloc::string::String;
use alloc::vec::Vec;

/// Backend-level error. A subset mirror of `rfs2::Error`, plus the VFS-level
/// `ReadOnly` used by the realize-guard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsError {
    /// No such file or directory.
    NotFound,
    /// Target already exists.
    Exists,
    /// A path component is not a directory.
    NotDir,
    /// Target is a directory where a non-directory was required.
    IsDir,
    /// Directory not empty.
    NotEmpty,
    /// No space left.
    NoSpace,
    /// Malformed argument (bad path, name too long, cross-mount rename, …).
    Invalid,
    /// Write to an immutable / sealed path (read-only-after-realize, or a
    /// read-only mount).
    ReadOnly,
    /// Device / integrity fault (I/O, auth, corruption) — detected, never
    /// silent.
    Device,
}

pub type FsResult<T> = core::result::Result<T, FsError>;

/// Slim, `Copy` inode metadata — the fields the VFS surface consumes. Avoids
/// handing a whole `rfs2::Inode` across the trait boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct InodeMeta {
    pub ino: u64,
    pub size: u64,
    pub is_dir: bool,
    pub is_symlink: bool,
    pub mode: u16,
    pub uid: u32,
    pub gid: u32,
    pub nlink: u32,
    /// Backend-native timestamps (rfs2: nanoseconds); unit conversion is the
    /// caller's concern.
    pub mtime: u64,
    pub ctime: u64,
}

/// One directory entry (mirror of `rfs2::DirEntryOut`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    pub ino: u64,
    pub file_type: u8,
    pub name: String,
}

/// The operations the mount table routes to a backend. Paths are always
/// **backend-relative** (the mount prefix already stripped by
/// [`MountTable::resolve`](crate::MountTable::resolve)) and absolute within the
/// backend (leading `/`).
pub trait FsBackend {
    fn lookup(&mut self, path: &str) -> FsResult<u64>;
    fn stat(&mut self, path: &str) -> FsResult<InodeMeta>;
    fn readlink(&mut self, path: &str) -> FsResult<String>;
    fn read_at(&mut self, ino: u64, off: u64, out: &mut [u8]) -> FsResult<usize>;
    fn write_at(&mut self, ino: u64, off: u64, data: &[u8]) -> FsResult<()>;
    fn create(&mut self, path: &str) -> FsResult<u64>;
    fn mkdir(&mut self, path: &str) -> FsResult<u64>;
    fn unlink(&mut self, path: &str) -> FsResult<()>;
    fn rename(&mut self, old: &str, new: &str) -> FsResult<()>;
    fn readdir(&mut self, path: &str) -> FsResult<Vec<DirEntry>>;
    fn pin(&mut self, ino: u64) -> FsResult<()>;
    fn unpin(&mut self, ino: u64) -> FsResult<()>;
    fn commit(&mut self) -> FsResult<()>;
    fn has_staged_changes(&self) -> bool;
    fn generation(&self) -> u64;
}
