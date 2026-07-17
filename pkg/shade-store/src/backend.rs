//! The filesystem seam realization writes through.
//!
//! [`StoreFs`] is the userspace mirror of `vfs-core::FsBackend`. The vfs-core
//! trait itself is not usable from ring 3: it is inode-addressed
//! (`read_at(ino, …)`, `pin`, `commit`, `generation`) — the shape the kernel
//! mount table routes to — and none of those operations have a syscall
//! surface. Ring-3 programs only have the path/fd VFS syscalls, so this trait
//! is path-addressed and whole-file, exposing exactly the operations
//! [`realize`](crate::realize) needs and nothing more.
//!
//! [`FsError`] carries the same vocabulary as `vfs-core::FsError` (the errno
//! fold in `docs/spec/syscalls.md`), plus [`Unsupported`](FsError::Unsupported)
//! for operations the Lythos ABI has no syscall for yet (rmdir, symlink
//! create/read — see the `oros` backend).
//!
//! Backends: `HostFs` (feature `std`, host suite + host tooling), `OrosFs`
//! (feature `oros`, raw Lythos syscalls), and the test `MemFs`. Injection is
//! by `&mut dyn StoreFs` — same-crate host and OROS-target builds share every
//! line of realization logic.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

/// Seam-level error. Mirrors `vfs-core::FsError` / the syscall errno fold;
/// `Unsupported` marks operations with no ABI surface on the target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsError {
    /// No such file or directory (ENOENT).
    NotFound,
    /// Target already exists (EEXIST).
    Exists,
    /// A path component is not a directory (ENOTDIR).
    NotDir,
    /// Target is a directory where a non-directory was required (EISDIR).
    IsDir,
    /// Directory not empty (ENOTEMPTY).
    NotEmpty,
    /// No space left (ENOSPC).
    NoSpace,
    /// Malformed argument (EINVAL).
    Invalid,
    /// Write to an immutable / sealed path (EROFS — read-only-after-realize).
    ReadOnly,
    /// Device / integrity fault (EIO and everything else) — detected, never
    /// silent.
    Device,
    /// The backend has no way to perform this operation (no syscall for it).
    Unsupported,
}

impl core::fmt::Display for FsError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let s = match self {
            FsError::NotFound => "not found",
            FsError::Exists => "already exists",
            FsError::NotDir => "not a directory",
            FsError::IsDir => "is a directory",
            FsError::NotEmpty => "directory not empty",
            FsError::NoSpace => "no space left",
            FsError::Invalid => "invalid argument",
            FsError::ReadOnly => "read-only (sealed) path",
            FsError::Device => "device or integrity fault",
            FsError::Unsupported => "operation unsupported by backend",
        };
        f.write_str(s)
    }
}

pub type FsResult<T> = core::result::Result<T, FsError>;

/// What a path points at (lstat semantics — a symlink is reported as itself).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    File,
    Dir,
    Symlink,
    Other,
}

/// lstat result: the node kind plus the one permission bit realization must
/// preserve — executability (a store-realized binary that loses its exec bit
/// is broken). Same shape as shadec's `EvalIo::FileMeta`. `len` is the file
/// size in bytes (0 for dirs/symlinks) — the store GC sizes swept entries
/// through the seam.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NodeMeta {
    pub kind: NodeKind,
    pub exec: bool,
    pub len: u64,
}

/// Path-addressed filesystem operations for store realization. Paths are
/// absolute, `/`-separated strings.
///
/// Contract notes:
/// - [`write_file`](StoreFs::write_file) creates the file, writes all bytes,
///   and makes them durable before returning. Callers only ever write fresh
///   temp names; whether an existing path is truncated (host) or rejected
///   `Exists` (Lythos `SYS_CREATE` is exclusive-create) is backend-defined.
/// - [`rename`](StoreFs::rename) is the atomic commit primitive — realization
///   depends on rename being the *only* way content appears at a final store
///   path (rename-as-seal).
/// - [`sync_dir`](StoreFs::sync_dir) is best-effort durability for a rename;
///   backends without directory fsync return `Ok(())`.
pub trait StoreFs {
    /// lstat: what is at `path`, without following a final symlink.
    fn metadata(&mut self, path: &str) -> FsResult<NodeMeta>;
    /// Whether anything is at `path` (default: [`metadata`](StoreFs::metadata)
    /// succeeds).
    fn exists(&mut self, path: &str) -> bool {
        self.metadata(path).is_ok()
    }
    fn read_file(&mut self, path: &str) -> FsResult<Vec<u8>>;
    /// Create `path` with `data`, marking it executable if `exec`. Backends
    /// whose ABI has no mode surface (OROS today — no chmod syscall) ignore
    /// `exec`.
    fn write_file(&mut self, path: &str, data: &[u8], exec: bool) -> FsResult<()>;
    /// **Atomic** create-if-absent: create `path` with `data` iff nothing is
    /// there, else fail [`Exists`](FsError::Exists) — exactly one concurrent
    /// caller wins. This is the lock primitive (the store db serializes
    /// mutations on it); backends must provide real atomicity, never a
    /// check-then-create. On Lythos this is `SYS_CREATE`'s exclusive-create
    /// guarantee; on the host, `OpenOptions::create_new`.
    fn create_exclusive(&mut self, path: &str, data: &[u8]) -> FsResult<()>;
    fn mkdir(&mut self, path: &str) -> FsResult<()>;
    fn rename(&mut self, old: &str, new: &str) -> FsResult<()>;
    fn unlink(&mut self, path: &str) -> FsResult<()>;
    fn rmdir(&mut self, path: &str) -> FsResult<()>;
    /// Entries of a directory as `(name, kind)`.
    fn read_dir(&mut self, path: &str) -> FsResult<Vec<(String, NodeKind)>>;
    fn read_link(&mut self, path: &str) -> FsResult<String>;
    fn symlink(&mut self, target: &str, link: &str) -> FsResult<()>;
    /// Best-effort directory fsync (forces a rename durable, 02 §6.3).
    fn sync_dir(&mut self, _path: &str) -> FsResult<()> {
        Ok(())
    }
    /// Remove an entire dead store path — the sole store-reclamation primitive
    /// (docs/shade/store-db-gc §4). The GC calls it on a DB-confirmed
    /// unreferenced top-level store entry; it deletes the whole tree and frees
    /// its blocks. It must operate **below** any realize-seal — it never opens a
    /// sealed file for write, and there is no way to make sealed content
    /// writable. Content-addressing keeps removal safe (a later realize of the
    /// same digest reproduces byte-identical content).
    ///
    /// Backends with no seal — the host `HostFs`, the in-memory `MemFs` — use
    /// this default: plain recursive delete (unlink files/symlinks, rmdir
    /// dirs). The on-target `OrosFs` overrides it to issue `SYS_STORE_REMOVE`
    /// (store-owner authority; the kernel removes the tree below the seal).
    /// Idempotent: an already-gone path is `Ok(())`.
    fn remove_store_path(&mut self, path: &str) -> FsResult<()> {
        match self.metadata(path).map(|m| m.kind) {
            Ok(NodeKind::Dir) => {
                if let Ok(entries) = self.read_dir(path) {
                    for (name, _) in entries {
                        self.remove_store_path(&join(path, &name))?;
                    }
                }
                match self.rmdir(path) {
                    Ok(()) | Err(FsError::NotFound) => Ok(()),
                    Err(e) => Err(e),
                }
            }
            Ok(_) => match self.unlink(path) {
                Ok(()) | Err(FsError::NotFound) => Ok(()),
                Err(e) => Err(e),
            },
            Err(FsError::NotFound) => Ok(()),
            Err(e) => Err(e),
        }
    }
    /// A per-caller uniqueness source for temp names (host: pid; OROS:
    /// boot-relative nanoseconds). Transient — never enters any hash
    /// (02 §3.3 excludes build-machine identity).
    fn unique_token(&mut self) -> u64 {
        0
    }
}

/// `dir` + `/` + `name`, without doubling separators.
pub fn join(dir: &str, name: &str) -> String {
    format!("{}/{}", dir.trim_end_matches('/'), name)
}

/// Split `path` into `(parent, file_name)`. `"/a/b"` → `("/a", "b")`;
/// a top-level `"/a"` → `("/", "a")`.
pub fn split_parent(path: &str) -> (&str, &str) {
    match path.trim_end_matches('/').rsplit_once('/') {
        Some(("", name)) => ("/", name),
        Some((parent, name)) => (parent, name),
        None => (".", path),
    }
}

/// `mkdir -p`: create every missing component of `path`. Tolerates existing
/// components (`Exists` from a concurrent creator included).
pub fn create_dir_all(fs: &mut dyn StoreFs, path: &str) -> FsResult<()> {
    let mut prefix = String::new();
    if path.starts_with('/') {
        prefix.push('/');
    }
    for comp in path.split('/').filter(|c| !c.is_empty()) {
        if !prefix.ends_with('/') {
            prefix.push('/');
        }
        prefix.push_str(comp);
        match fs.mkdir(&prefix) {
            Ok(()) | Err(FsError::Exists) => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Recursively copy a tree (files, dirs, symlinks) from `src` to `dst`,
/// preserving executability.
pub fn copy_tree(fs: &mut dyn StoreFs, src: &str, dst: &str) -> FsResult<()> {
    let meta = fs.metadata(src)?;
    match meta.kind {
        NodeKind::Symlink => {
            let target = fs.read_link(src)?;
            fs.symlink(&target, dst)
        }
        NodeKind::Dir => {
            create_dir_all(fs, dst)?;
            for (name, _) in fs.read_dir(src)? {
                copy_tree(fs, &join(src, &name), &join(dst, &name))?;
            }
            Ok(())
        }
        NodeKind::File | NodeKind::Other => {
            let (parent, _) = split_parent(dst);
            create_dir_all(fs, parent)?;
            let data = fs.read_file(src)?;
            fs.write_file(dst, &data, meta.exec)
        }
    }
}

/// Best-effort recursive delete for leftover temp trees. Never errors: on a
/// backend without rmdir (OROS today) the files are unlinked and the empty
/// dir skeleton is left for the store GC.
pub fn remove_tree(fs: &mut dyn StoreFs, path: &str) {
    match fs.metadata(path).map(|m| m.kind) {
        Ok(NodeKind::Dir) => {
            if let Ok(entries) = fs.read_dir(path) {
                for (name, _) in entries {
                    remove_tree(fs, &join(path, &name));
                }
            }
            let _ = fs.rmdir(path);
        }
        Ok(_) => {
            let _ = fs.unlink(path);
        }
        Err(_) => {}
    }
}

/// A unique temp path sibling to `final_name` under `dir`
/// (`.tmp-<kind>-<final_name>-<token>-<n>`).
pub fn temp_sibling(fs: &mut dyn StoreFs, dir: &str, final_name: &str, kind: &str) -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let token = fs.unique_token();
    join(dir, &format!(".tmp-{kind}-{final_name}-{token}-{n}"))
}
