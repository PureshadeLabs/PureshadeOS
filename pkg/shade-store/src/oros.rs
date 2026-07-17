//! `OrosFs` — the on-target [`StoreFs`] over the raw Lythos VFS syscalls
//! (feature `oros`, x86_64 only — `lythos-syscall` is empty elsewhere).
//!
//! Raw `lythos-syscall` stubs are used instead of the `lythos-rt` wrappers
//! because the wrappers fold every failure to `Err(())`: `create_dir_all`
//! needs `EEXIST` vs. everything-else, and the realize race fallback needs
//! `EROFS`/`EEXIST` fidelity. Errnos map per the `docs/spec/syscalls.md`
//! error table (`lythos-abi::errno`).
//!
//! ABI gaps surface as [`FsError::Unsupported`]: there is no rmdir syscall
//! (`SYS_UNLINK` deletes regular files only; errno table notes "future
//! rmdir"), so `remove_tree` leaves empty temp dir skeletons for the store
//! GC. Symlink create/read landed with `SYS_SYMLINK`/`SYS_READLINK` (the
//! B-series end-to-end glue) — profile forests, `current` flips, and direct
//! GC roots now work on target.
//!
//! Durability: `sync_dir` is a no-op — RFS2 commits transactionally inside
//! the kernel (virtio-blk flush on commit); there is no userspace fsync
//! surface to call.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use lythos_abi::errno as e;
use lythos_abi::syscall as nr;
use lythos_syscall::{syscall0, syscall1, syscall2, syscall3, syscall4};

use crate::backend::{FsError, FsResult, NodeKind, NodeMeta, StoreFs};

/// The Lythos on-target backend. Stateless — all state is kernel-side.
#[derive(Debug, Default, Clone, Copy)]
pub struct OrosFs;

fn fs_err(r: u64) -> FsError {
    match r {
        e::ENOENT => FsError::NotFound,
        e::EEXIST => FsError::Exists,
        e::ENOTDIR => FsError::NotDir,
        e::EISDIR => FsError::IsDir,
        e::ENOTEMPTY => FsError::NotEmpty,
        e::ENOSPC => FsError::NoSpace,
        e::EINVAL => FsError::Invalid,
        e::EROFS => FsError::ReadOnly,
        _ => FsError::Device,
    }
}

fn check(r: u64) -> FsResult<u64> {
    if e::is_err(r) { Err(fs_err(r)) } else { Ok(r) }
}

/// `FileStat` wire layout (48 bytes, `docs/spec/syscalls.md`): flags is the
/// u32 at offset 24; bit 1 = dir, bit 2 = symlink (RFS `INODE_*`).
const STAT_SIZE: usize = 48;
const FLAG_DIR: u32 = 1 << 1;
const FLAG_SYMLINK: u32 = 1 << 2;

/// Readdir wire slot (264 bytes): ino u32, file_type u8 (1 = regular,
/// 2 = dir, 3 = symlink), name_len u8, pad, name[256] at offset 8.
const DIR_ENTRY_SIZE: usize = 264;
/// One-shot `SYS_READDIR` has no offset/continuation argument; staged store
/// trees stay far below this.
const MAX_DIR_ENTRIES: usize = 1024;

const READ_CHUNK: usize = 64 * 1024;

impl StoreFs for OrosFs {
    fn metadata(&mut self, path: &str) -> FsResult<NodeMeta> {
        let mut buf = [0u8; STAT_SIZE];
        check(unsafe {
            syscall3(nr::SYS_STAT, path.as_ptr() as u64, path.len() as u64, buf.as_mut_ptr() as u64)
        })?;
        let flags = u32::from_le_bytes(buf[24..28].try_into().unwrap());
        let kind = if flags & FLAG_SYMLINK != 0 {
            NodeKind::Symlink
        } else if flags & FLAG_DIR != 0 {
            NodeKind::Dir
        } else {
            NodeKind::File
        };
        // mode is the u16 at offset 40; exec = any of 0o111. The kernel does
        // not yet enforce exec bits, but preserve what stat reports.
        let mode = u16::from_le_bytes(buf[40..42].try_into().unwrap());
        // size is the u64 at offset 0.
        let size = u64::from_le_bytes(buf[0..8].try_into().unwrap());
        Ok(NodeMeta {
            kind,
            exec: kind == NodeKind::File && mode & 0o111 != 0,
            len: if kind == NodeKind::File { size } else { 0 },
        })
    }

    fn read_file(&mut self, path: &str) -> FsResult<Vec<u8>> {
        let fd = check(unsafe { syscall2(nr::SYS_OPEN, path.as_ptr() as u64, path.len() as u64) })?;
        let mut out = Vec::new();
        let mut chunk = vec![0u8; READ_CHUNK];
        loop {
            let r = unsafe {
                syscall3(nr::SYS_READ, fd, chunk.as_mut_ptr() as u64, chunk.len() as u64)
            };
            if e::is_err(r) {
                unsafe { syscall1(nr::SYS_CLOSE, fd) };
                return Err(fs_err(r));
            }
            let n = r as usize;
            if n == 0 {
                break;
            }
            out.extend_from_slice(&chunk[..n]);
        }
        unsafe { syscall1(nr::SYS_CLOSE, fd) };
        Ok(out)
    }

    /// `exec` is accepted but cannot be applied — the ABI has no chmod/mode
    /// argument on `SYS_CREATE` (deferred with the exec-bit enforcement gap).
    fn write_file(&mut self, path: &str, data: &[u8], _exec: bool) -> FsResult<()> {
        // SYS_CREATE is atomic exclusive-create: `Exists` here is a real
        // collision, never truncate-and-continue.
        let fd =
            check(unsafe { syscall2(nr::SYS_CREATE, path.as_ptr() as u64, path.len() as u64) })?;
        let mut off = 0;
        while off < data.len() {
            let r = unsafe {
                syscall3(
                    nr::SYS_WRITE,
                    fd,
                    data[off..].as_ptr() as u64,
                    (data.len() - off) as u64,
                )
            };
            if e::is_err(r) || r == 0 {
                unsafe { syscall1(nr::SYS_CLOSE, fd) };
                return Err(if e::is_err(r) { fs_err(r) } else { FsError::Device });
            }
            off += r as usize;
        }
        unsafe { syscall1(nr::SYS_CLOSE, fd) };
        Ok(())
    }

    /// `SYS_CREATE` *is* atomic exclusive-create — same code path as
    /// [`write_file`](StoreFs::write_file), stated separately because callers
    /// of this method rely on the exactly-one-winner guarantee.
    fn create_exclusive(&mut self, path: &str, data: &[u8]) -> FsResult<()> {
        self.write_file(path, data, false)
    }

    fn mkdir(&mut self, path: &str) -> FsResult<()> {
        check(unsafe { syscall2(nr::SYS_MKDIR, path.as_ptr() as u64, path.len() as u64) })?;
        Ok(())
    }

    fn rename(&mut self, old: &str, new: &str) -> FsResult<()> {
        check(unsafe {
            syscall4(
                nr::SYS_RENAME,
                old.as_ptr() as u64,
                old.len() as u64,
                new.as_ptr() as u64,
                new.len() as u64,
            )
        })?;
        Ok(())
    }

    fn unlink(&mut self, path: &str) -> FsResult<()> {
        check(unsafe { syscall2(nr::SYS_UNLINK, path.as_ptr() as u64, path.len() as u64) })?;
        Ok(())
    }

    fn rmdir(&mut self, _path: &str) -> FsResult<()> {
        Err(FsError::Unsupported)
    }

    fn remove_store_path(&mut self, path: &str) -> FsResult<()> {
        // SYS_STORE_REMOVE removes the whole store tree below the seal in one
        // call (the kernel unlinks files, rmdirs dirs, frees blocks, and drops
        // the in-kernel seal as the last lifecycle step). It is gated on the
        // Filesystem capability (store-owner authority); ENOPERM here means the
        // caller holds no such cap — the entry is not reclaimed and the sealed
        // tree stays intact. There is no userspace rmdir/unlink path that could
        // touch a sealed tree, so this is the only way GC frees a store path on
        // target.
        check(unsafe {
            syscall2(nr::SYS_STORE_REMOVE, path.as_ptr() as u64, path.len() as u64)
        })?;
        Ok(())
    }

    fn read_dir(&mut self, path: &str) -> FsResult<Vec<(String, NodeKind)>> {
        let mut buf = vec![0u8; MAX_DIR_ENTRIES * DIR_ENTRY_SIZE];
        let count = check(unsafe {
            syscall4(
                nr::SYS_READDIR,
                path.as_ptr() as u64,
                path.len() as u64,
                buf.as_mut_ptr() as u64,
                buf.len() as u64,
            )
        })? as usize;
        let mut out = Vec::with_capacity(count);
        for i in 0..count {
            let s = &buf[i * DIR_ENTRY_SIZE..(i + 1) * DIR_ENTRY_SIZE];
            let name_len = s[5] as usize;
            let name = core::str::from_utf8(&s[8..8 + name_len])
                .map_err(|_| FsError::Invalid)?;
            // The kernel's SYS_READDIR yields "." and ".."; the StoreFs seam
            // contract excludes them (HostFs/MemFs never do). Filtering here
            // keeps callers — copy_tree, gc's store scan, generation-number
            // parsing — backend-agnostic. Without it copy_tree would recurse
            // into ".." and mkdir("..") → EINVAL.
            if name == "." || name == ".." {
                continue;
            }
            let kind = match s[4] {
                1 => NodeKind::File,
                2 => NodeKind::Dir,
                3 => NodeKind::Symlink,
                _ => NodeKind::Other,
            };
            out.push((String::from(name), kind));
        }
        Ok(out)
    }

    fn read_link(&mut self, path: &str) -> FsResult<String> {
        // Targets ≤ 4096 (SYS_READLINK buf cap); a longer return than the
        // buffer means truncation — treat as invalid rather than corrupt it.
        let mut buf = vec![0u8; 4096];
        let n = check(unsafe {
            syscall4(
                nr::SYS_READLINK,
                path.as_ptr() as u64,
                path.len() as u64,
                buf.as_mut_ptr() as u64,
                buf.len() as u64,
            )
        })? as usize;
        if n > buf.len() {
            return Err(FsError::Invalid);
        }
        buf.truncate(n);
        String::from_utf8(buf).map_err(|_| FsError::Invalid)
    }

    fn symlink(&mut self, target: &str, link: &str) -> FsResult<()> {
        check(unsafe {
            syscall4(
                nr::SYS_SYMLINK,
                target.as_ptr() as u64,
                target.len() as u64,
                link.as_ptr() as u64,
                link.len() as u64,
            )
        })?;
        Ok(())
    }

    fn unique_token(&mut self) -> u64 {
        // Boot-relative nanoseconds: distinct across concurrent realizers
        // (there is no getpid syscall). Transient, never hashed.
        unsafe { syscall0(nr::SYS_TIME) }
    }
}
