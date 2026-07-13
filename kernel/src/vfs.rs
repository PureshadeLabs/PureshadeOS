//! VFS glue: routes the kernel's FS surface (the `SYS_OPEN`…`SYS_SEEK` +
//! `SYS_MOUNT` handlers in `syscall.rs` plus `load_file` for exec) through a
//! generic mount table (`fs/vfs-core`) to mounted RFS V2 volumes (`fs/rfs2`).
//!
//! This module is integration only — routing logic lives in `vfs-core`
//! (host-tested), FS logic in `rfs2`. It owns:
//!
//! - the concrete backends: [`VirtioDisk`] (root volume) and [`RamDisk`]
//!   (PMM-backed, for `SYS_MOUNT` source `MOUNT_SRC_RFS2_RAM`), both behind
//!   `rfs2::BlockDevice`, and the [`Rfs2Backend`] adapter that puts an
//!   `rfs2::Rfs2` behind `vfs_core::FsBackend`;
//! - the open-file table (fd → mount + inode + offset), driving the V2
//!   open-handle pins (doc 06 §5);
//! - symlink-following path resolution over the mount table (per-hop, so a
//!   symlink target crossing a mount boundary re-routes correctly);
//! - errno mapping to the canonical `abi/lythos-abi/src/errno.rs` values
//!   (the V1-era local scheme is gone — followup item 10);
//! - the commit policy: every mutating operation commits immediately on its
//!   backend (doc 04 §3; commit-per-syscall, matching V1-observed durability).
//!
//! Capability enforcement does NOT live here — `SYS_MOUNT`'s Filesystem-cap
//! gate is in `syscall.rs`, on the syscall boundary
//! (docs/plans/mount-syscall-shade-store.md §3.4).
//!
//! Concurrency model: same as `virtio_blk::DevState` — the kernel is
//! single-threaded and in-kernel FS paths are not preempted, so a plain
//! `UnsafeCell` global suffices.

#![allow(dead_code)]

extern crate alloc;
use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use rfs2::{BlockDevice, Error, IdentityTransform, Rfs2};
use vfs_core::{
    FsBackend, FsError, InodeMeta, MountError, MountId, MountTable, RealizeGuard, RenameOutcome,
};

use crate::virtio_blk;

// ── Error codes (canonical abi/lythos-abi/src/errno.rs values) ────────────────
//
// The kernel crate has no dependency on lythos-abi, so the values are
// restated here — they MUST stay identical to `abi/lythos-abi/src/errno.rs`
// and the table in `docs/spec/syscalls.md`. The old V1-era local scheme
// (`ENODEV=-1`, `EISDIR=-7`) collided with the ABI table
// (docs/plans/followup-code-tasks.md item 10 — resolved 2026-07-13):
// device/integrity faults now report `EIO=-17` and directory-vs-file misuse
// `EISDIR=-15`; `FsError::NotEmpty` gets its own `ENOTEMPTY=-16` instead of
// folding into EINVAL.

pub const EINVAL:    i64 = -4;
pub const ENOENT:    i64 = -5;
pub const EBADF:     i64 = -6;
pub const ENOTDIR:   i64 = -8;
pub const ENOMNT:    i64 = -9;
pub const EMFILE:    i64 = -10;
pub const EEXIST:    i64 = -11;
pub const ENOSPC:    i64 = -12;
/// A mount already exists at the mount point (SYS_MOUNT).
pub const EMOUNTED:  i64 = -13;
/// Write to a read-only / sealed path (read-only-after-realize, stage 2).
pub const EROFS:     i64 = -14;
/// Path is a directory where a regular file is required.
pub const EISDIR:    i64 = -15;
/// Directory not empty (rename onto a non-empty dir; future rmdir).
pub const ENOTEMPTY: i64 = -16;
/// I/O / integrity fault: device error, failed auth, corrupt structure.
pub const EIO:       i64 = -17;

/// `vfs_core::FsError` → errno.
fn errno_fs(e: FsError) -> i64 {
    match e {
        FsError::NotFound => ENOENT,
        FsError::Exists => EEXIST,
        FsError::NotDir => ENOTDIR,
        FsError::IsDir => EISDIR,
        FsError::NoSpace => ENOSPC,
        FsError::Invalid => EINVAL,
        FsError::NotEmpty => ENOTEMPTY,
        FsError::ReadOnly => EROFS,
        FsError::Device => EIO,
    }
}

/// `vfs_core::MountError` → errno (design §3.5).
fn errno_mount(e: MountError) -> i64 {
    match e {
        MountError::Invalid => EINVAL,
        MountError::AlreadyMounted => EMOUNTED,
        MountError::NoSuchPath => ENOENT,
        MountError::NotDir => ENOTDIR,
        MountError::NotMounted => ENOMNT,
        MountError::Backend(e) => errno_fs(e),
    }
}

/// `rfs2::Error` → `vfs_core::FsError` (the backend adapter's mapping).
fn fs_err(e: Error) -> FsError {
    match e {
        Error::NotFound => FsError::NotFound,
        Error::Exists => FsError::Exists,
        Error::NotDir => FsError::NotDir,
        Error::IsDir => FsError::IsDir,
        Error::NotEmpty => FsError::NotEmpty,
        Error::NoSpace | Error::GenExhausted => FsError::NoSpace,
        // A volume mounted read-only (unknown ro_compat bit) now reports
        // EROFS instead of V1's EINVAL fold — strictly more precise.
        Error::ReadOnly => FsError::ReadOnly,
        Error::Invalid | Error::Perm => FsError::Invalid,
        // Device / integrity faults: detected, never silent (CONSIST-2).
        Error::Io
        | Error::Auth { .. }
        | Error::BadHeader
        | Error::Unsupported
        | Error::NoSuperblock
        | Error::Corrupt => FsError::Device,
    }
}

// ── Stat / DirEntry (ABI-identical to the V1 rfs.rs types) ────────────────────

// V1 inode flag bits, kept as the `Stat.flags` ABI (docs/spec/syscalls.md).
pub const INODE_USED:    u32 = 1 << 0;
pub const INODE_DIR:     u32 = 1 << 1;
pub const INODE_SYMLINK: u32 = 1 << 2;

pub const FT_REG:     u8 = 1;
pub const FT_DIR:     u8 = 2;
pub const FT_SYMLINK: u8 = 3;

#[derive(Clone, Copy, Default)]
pub struct Stat {
    pub size:  u64,
    pub flags: u32,
    pub mode:  u16,
    pub uid:   u32,
    pub gid:   u32,
    pub nlink: u32,
    pub mtime: u64,
    pub ctime: u64,
}

impl From<&InodeMeta> for Stat {
    fn from(m: &InodeMeta) -> Self {
        let mut flags = INODE_USED;
        if m.is_dir {
            flags |= INODE_DIR;
        }
        if m.is_symlink {
            flags |= INODE_SYMLINK;
        }
        Stat {
            size: m.size,
            flags,
            mode: m.mode & 0o7777, // V1 stored permission bits only; type is in `flags`
            uid: m.uid,
            gid: m.gid,
            nlink: m.nlink,
            // Backends carry nanoseconds; the syscall ABI is epoch
            // milliseconds (docs/spec/syscalls.md SYS_STAT layout).
            mtime: m.mtime / 1_000_000,
            ctime: m.ctime / 1_000_000,
        }
    }
}

pub struct DirEntry {
    pub ino:       u32,
    pub file_type: u8,
    pub name:      String,
}

// ── Block devices ─────────────────────────────────────────────────────────────

const BLOCK_SIZE: usize = 4096;
const SECTORS_PER_BLOCK: u64 = (BLOCK_SIZE / virtio_blk::SECTOR_SIZE) as u64; // 8

/// virtio-blk behind `rfs2::BlockDevice` (the root volume's device).
pub struct VirtioDisk {
    blocks: u64,
}

impl BlockDevice for VirtioDisk {
    fn total_blocks(&self) -> u64 {
        self.blocks
    }

    fn read_block(&mut self, block: u64, buf: &mut [u8]) -> rfs2::Result<()> {
        debug_assert_eq!(buf.len(), BLOCK_SIZE);
        if virtio_blk::read_sectors(block * SECTORS_PER_BLOCK, buf) {
            Ok(())
        } else {
            Err(Error::Io)
        }
    }

    fn write_block(&mut self, block: u64, buf: &[u8]) -> rfs2::Result<()> {
        debug_assert_eq!(buf.len(), BLOCK_SIZE);
        if virtio_blk::write_sectors(block * SECTORS_PER_BLOCK, buf) {
            Ok(())
        } else {
            Err(Error::Io)
        }
    }

    /// Write barrier (device.rs contract): durable on return. Backed by
    /// VIRTIO_BLK_T_FLUSH when F_FLUSH was negotiated; correct no-op on a
    /// write-through device. The commit pointer-flip (COW-3) rides on this.
    fn flush(&mut self) -> rfs2::Result<()> {
        if virtio_blk::flush() {
            Ok(())
        } else {
            Err(Error::Io)
        }
    }
}

/// PMM-backed RAM block device: one 4 KiB physical frame per block, accessed
/// through the kernel's 0→`vmm::IDENTITY_MAP_LIMIT` identity map. Every frame
/// is **validated against that limit before its first access** — the PMM
/// bitmap spans 4 GiB, so with enough guest RAM `alloc_frame` can return a
/// frame no kernel direct map covers; without the check the first touch would
/// page-fault instead of failing the mount cleanly. The PMM allocator is
/// lowest-frame-first, so the first out-of-range frame means low memory is
/// exhausted — there is nothing to skip to; allocation fails as a whole.
/// Keeps the kernel heap out of the picture — only the frame-address Vec
/// lives on the heap.
///
/// Frames are never returned to the PMM after a successful mount: a mounted
/// volume lives for the machine's uptime (no unmount syscall yet; noted in
/// the design doc).
pub struct RamDisk {
    frames: Vec<u64>, // physical frame addresses, one per block
}

/// Blocks per RAM-backed volume: 4096 × 4 KiB = 16 MiB. Tunable.
const RAM_DISK_BLOCKS: usize = 4096;

/// Whether the 4 KiB frame at `pa` lies wholly inside the kernel's identity
/// map — the only window [`RamDisk`] uses. Pure predicate so the boot probe
/// can exercise the rejection boundary without allocating >1 GiB of RAM.
pub fn frame_in_direct_map(pa: u64) -> bool {
    pa % BLOCK_SIZE as u64 == 0
        && pa
            .checked_add(BLOCK_SIZE as u64)
            .is_some_and(|end| end <= crate::vmm::IDENTITY_MAP_LIMIT)
}

impl RamDisk {
    /// Allocate and zero a `blocks`-block RAM disk. `None` if the PMM cannot
    /// supply enough frames, or supplies a frame outside the identity map
    /// (checked BEFORE the frame is touched — a clean failure, never a
    /// fault). Rolls back every frame taken on failure.
    fn new(blocks: usize) -> Option<RamDisk> {
        let mut frames = Vec::with_capacity(blocks);
        let fail = |frames: &Vec<u64>, extra: Option<u64>| {
            if let Some(pa) = extra {
                crate::pmm::free_frame(crate::pmm::PhysAddr(pa));
            }
            for &pa in frames {
                crate::pmm::free_frame(crate::pmm::PhysAddr(pa));
            }
        };
        for _ in 0..blocks {
            match crate::pmm::alloc_frame() {
                Some(pa) => {
                    let pa = pa.as_u64();
                    if !frame_in_direct_map(pa) {
                        crate::kprintln!(
                            "[vfs] ram disk: frame {:#x} is outside the {} MiB identity map — \
                             refusing cleanly (mount fails, no fault)",
                            pa,
                            crate::vmm::IDENTITY_MAP_LIMIT >> 20,
                        );
                        fail(&frames, Some(pa));
                        return None;
                    }
                    // Safe to touch: the whole frame is inside the direct map.
                    unsafe { core::ptr::write_bytes(pa as *mut u8, 0, BLOCK_SIZE) };
                    frames.push(pa);
                }
                None => {
                    fail(&frames, None);
                    return None;
                }
            }
        }
        Some(RamDisk { frames })
    }
}

impl BlockDevice for RamDisk {
    fn total_blocks(&self) -> u64 {
        self.frames.len() as u64
    }

    fn read_block(&mut self, block: u64, buf: &mut [u8]) -> rfs2::Result<()> {
        debug_assert_eq!(buf.len(), BLOCK_SIZE);
        let &pa = self.frames.get(block as usize).ok_or(Error::Io)?;
        // `new` validated every frame; this guards against construction paths
        // that bypass it.
        debug_assert!(frame_in_direct_map(pa), "RamDisk frame {pa:#x} outside direct map");
        unsafe { core::ptr::copy_nonoverlapping(pa as *const u8, buf.as_mut_ptr(), BLOCK_SIZE) };
        Ok(())
    }

    fn write_block(&mut self, block: u64, buf: &[u8]) -> rfs2::Result<()> {
        debug_assert_eq!(buf.len(), BLOCK_SIZE);
        let &pa = self.frames.get(block as usize).ok_or(Error::Io)?;
        debug_assert!(frame_in_direct_map(pa), "RamDisk frame {pa:#x} outside direct map");
        unsafe { core::ptr::copy_nonoverlapping(buf.as_ptr(), pa as *mut u8, BLOCK_SIZE) };
        Ok(())
    }

    /// RAM is always "durable" for the volume's lifetime; no barrier needed.
    fn flush(&mut self) -> rfs2::Result<()> {
        Ok(())
    }
}

/// Wall clock for inode timestamps: nanoseconds, as `MkfsOptions::now`
/// documents. The kernel's anchor has millisecond resolution.
fn clock_ns() -> u64 {
    crate::time::epoch_ms() * 1_000_000
}

// ── Rfs2 behind vfs_core::FsBackend ───────────────────────────────────────────

/// Adapter: any `Rfs2<D, IdentityTransform>` as a boxable `FsBackend`.
struct Rfs2Backend<D: BlockDevice> {
    fs: Rfs2<D, IdentityTransform>,
}

fn meta_from(n: &rfs2::Inode, ino: u64) -> InodeMeta {
    InodeMeta {
        ino,
        size: n.size,
        is_dir: n.is_dir(),
        is_symlink: n.is_symlink(),
        mode: n.mode,
        uid: n.uid,
        gid: n.gid,
        nlink: n.nlink,
        mtime: n.mtime,
        ctime: n.ctime,
    }
}

impl<D: BlockDevice> FsBackend for Rfs2Backend<D> {
    fn lookup(&mut self, path: &str) -> Result<u64, FsError> {
        self.fs.lookup(path).map_err(fs_err)
    }
    fn stat(&mut self, path: &str) -> Result<InodeMeta, FsError> {
        let ino = self.fs.lookup(path).map_err(fs_err)?;
        let n = self.fs.stat(path).map_err(fs_err)?;
        Ok(meta_from(&n, ino))
    }
    fn readlink(&mut self, path: &str) -> Result<String, FsError> {
        self.fs.readlink(path).map_err(fs_err)
    }
    fn read_at(&mut self, ino: u64, off: u64, out: &mut [u8]) -> Result<usize, FsError> {
        self.fs.read_at(ino, off, out).map_err(fs_err)
    }
    fn write_at(&mut self, ino: u64, off: u64, data: &[u8]) -> Result<(), FsError> {
        self.fs.write_at(ino, off, data).map_err(fs_err)
    }
    fn create(&mut self, path: &str) -> Result<u64, FsError> {
        self.fs.create(path).map_err(fs_err)
    }
    fn mkdir(&mut self, path: &str) -> Result<u64, FsError> {
        self.fs.mkdir(path).map_err(fs_err)
    }
    fn unlink(&mut self, path: &str) -> Result<(), FsError> {
        self.fs.unlink(path).map_err(fs_err)
    }
    fn rename(&mut self, old: &str, new: &str) -> Result<(), FsError> {
        self.fs.rename(old, new).map_err(fs_err)
    }
    fn readdir(&mut self, path: &str) -> Result<Vec<vfs_core::DirEntry>, FsError> {
        let entries = self.fs.readdir(path).map_err(fs_err)?;
        Ok(entries
            .into_iter()
            .map(|e| vfs_core::DirEntry { ino: e.ino, file_type: e.file_type, name: e.name })
            .collect())
    }
    fn pin(&mut self, ino: u64) -> Result<(), FsError> {
        self.fs.pin(ino).map_err(fs_err)
    }
    fn unpin(&mut self, ino: u64) -> Result<(), FsError> {
        self.fs.unpin(ino).map_err(fs_err)
    }
    fn commit(&mut self) -> Result<(), FsError> {
        self.fs.commit().map_err(fs_err)
    }
    fn has_staged_changes(&self) -> bool {
        self.fs.has_staged_changes()
    }
    fn generation(&self) -> u64 {
        self.fs.generation()
    }
}

// ── Global state ──────────────────────────────────────────────────────────────

const MAX_FDS: usize = 64;
const MAX_SYMLINK_HOPS: usize = 8;

#[derive(Clone)]
struct OpenFile {
    /// Which mount the fd was opened on — fds keep addressing their backend
    /// even as other mounts come and go.
    mount:    MountId,
    ino:      u64,
    offset:   u64,
    /// Cached size for append/seek — same staleness the V1 fd table had.
    size:     u64,
    writable: bool,
    /// First component of the backend-relative path this fd was opened at —
    /// the store name a realize-guarded mount seals on. Lets `write` reject a
    /// stale writable fd into an entry sealed *after* the fd was opened
    /// (temp fd kept across the realize rename).
    top:      Option<String>,
}

struct Vfs {
    table:   MountTable,
    root_id: MountId,
    fds:     [Option<OpenFile>; MAX_FDS],
    /// Read-only-after-realize guards, present only for mounts created with
    /// MOUNT_STORE (stage 2 — docs/plans/mount-syscall-shade-store.md §4.2).
    guards:  BTreeMap<MountId, RealizeGuard>,
}

struct VfsState(core::cell::UnsafeCell<Option<Vfs>>);

// SAFETY: single-threaded kernel — no concurrent access (see module docs).
unsafe impl Sync for VfsState {}

static STATE: VfsState = VfsState(core::cell::UnsafeCell::new(None));

#[inline]
fn state() -> Option<&'static mut Vfs> {
    unsafe { (*STATE.0.get()).as_mut() }
}

// ── Init / mount ──────────────────────────────────────────────────────────────

/// Mount the root RFS V2 volume (virtio-blk) at `/`. Mount-time recovery —
/// slot selection, mark-and-sweep free-space reconstruction, orphan reclaim —
/// runs inside `Rfs2::mount` (doc 09 §4). Returns `true` on success.
pub fn init() -> bool {
    if !virtio_blk::is_present() {
        return false;
    }
    let sectors = virtio_blk::capacity_sectors();
    let disk = VirtioDisk { blocks: sectors / SECTORS_PER_BLOCK };
    match Rfs2::mount(disk, IdentityTransform, clock_ns) {
        Ok(fs) => {
            crate::kprintln!(
                "[vfs] rfs2 root mounted: gen={} slot={} blocks={}/{} inodes={}{}{}",
                fs.generation(),
                if fs.current_slot() == 1 { "A" } else { "B" },
                fs.superblock().block_count,
                fs.header().total_blocks,
                fs.superblock().inode_count,
                if fs.is_read_only() { " RO" } else { "" },
                if fs.block_count_mismatch() { " (block_count mismatch, resynced)" } else { "" },
            );
            let mut table = MountTable::new();
            let root_id = match table.mount("/", Box::new(Rfs2Backend { fs })) {
                Ok(id) => id,
                Err(_) => return false, // impossible: empty table accepts "/"
            };
            unsafe {
                *STATE.0.get() = Some(Vfs {
                    table,
                    root_id,
                    fds: [const { None }; MAX_FDS],
                    guards: BTreeMap::new(),
                })
            };
            true
        }
        Err(e) => {
            crate::kprintln!("[vfs] rfs2 root mount failed: {:?}", e);
            false
        }
    }
}

/// SYS_MOUNT backend selector values (ABI: abi/lythos-abi/src/syscall.rs).
pub const MOUNT_SRC_RFS2_RAM: u64 = 0;
/// SYS_MOUNT flags (ABI): attach the realize-guard (read-only-after-realize
/// store semantics) to this mount.
pub const MOUNT_STORE: u64 = 1 << 0;

/// Mount a new backend instance at `at`. Capability enforcement happens in
/// the SYS_MOUNT handler (syscall boundary), not here.
///
/// `source` must be `MOUNT_SRC_RFS2_RAM`; `flags` may set `MOUNT_STORE` to
/// attach a [`RealizeGuard`] (read-only-after-realize, design §4.2).
pub fn mount(at: &str, source: u64, flags: u64) -> i64 {
    let v = match state() {
        Some(v) => v,
        None => return ENOMNT,
    };
    if flags & !MOUNT_STORE != 0 {
        return EINVAL;
    }
    if source != MOUNT_SRC_RFS2_RAM {
        return EINVAL;
    }

    // Reject early (cheaply) before allocating 16 MiB of frames.
    if v.table.is_mounted_at(at) {
        return EMOUNTED;
    }

    let Some(mut dev) = RamDisk::new(RAM_DISK_BLOCKS) else {
        return ENOSPC;
    };
    let opts = rfs2::MkfsOptions { uuid: [0u8; 16], label: "ram", now: clock_ns() };
    if rfs2::mkfs(&mut dev, &IdentityTransform, &opts).is_err() {
        return EIO;
    }
    let fs = match Rfs2::mount(dev, IdentityTransform, clock_ns) {
        Ok(fs) => fs,
        Err(e) => {
            crate::kprintln!("[vfs] ram rfs2 mount failed: {:?}", e);
            return EIO;
        }
    };
    match v.table.mount(at, Box::new(Rfs2Backend { fs })) {
        Ok(id) => {
            let store = flags & MOUNT_STORE != 0;
            if store {
                v.guards.insert(id, RealizeGuard::new());
            }
            crate::kprintln!(
                "[vfs] rfs2 ram volume mounted at {} ({} blocks{})",
                at,
                RAM_DISK_BLOCKS,
                if store { ", store semantics" } else { "" },
            );
            0
        }
        Err(e) => errno_mount(e),
    }
}

// ── Path resolution (symlink following, mount-aware) ──────────────────────────

/// Canonicalize `path`, following symlinks in every component (hop-capped).
/// Every stat/readlink routes through the mount table, so each hop lands on
/// the covering backend — including a symlink target that crosses a mount
/// boundary. Returns a full (mount-side) path whose components are fully
/// resolved.
fn resolve(v: &mut Vfs, path: &str, hops: usize) -> Result<String, i64> {
    if hops > MAX_SYMLINK_HOPS {
        return Err(EINVAL);
    }
    let comps: Vec<&str> = path.split('/').filter(|c| !c.is_empty()).collect();
    let mut resolved = String::new(); // "" ≡ "/"
    let mut i = 0;
    while i < comps.len() {
        let comp = comps[i];
        i += 1;
        let mut cand = resolved.clone();
        cand.push('/');
        cand.push_str(comp);
        let (be, rel) = v.table.resolve(&cand).map_err(errno_mount)?;
        let meta = be.stat(&rel).map_err(errno_fs)?;
        if meta.is_symlink {
            let target = be.readlink(&rel).map_err(errno_fs)?;
            let mut next = if target.starts_with('/') {
                target
            } else {
                let mut p = resolved.clone();
                p.push('/');
                p.push_str(&target);
                p
            };
            for rest in &comps[i..] {
                next.push('/');
                next.push_str(rest);
            }
            return resolve(v, &next, hops + 1);
        }
        resolved = cand;
    }
    if resolved.is_empty() {
        resolved.push('/');
    }
    Ok(resolved)
}

/// Resolve the parent directory (following symlinks) and re-attach the final
/// component unresolved — for create/mkdir/unlink/rename, where the final
/// component must not be followed.
fn resolve_parent(v: &mut Vfs, path: &str) -> Result<String, i64> {
    if !path.starts_with('/') || path.ends_with('/') {
        return Err(EINVAL);
    }
    let idx = path.rfind('/').unwrap();
    let (parent, name) = (&path[..idx], &path[idx + 1..]);
    if name.is_empty() {
        return Err(EINVAL);
    }
    let mut base = resolve(v, if parent.is_empty() { "/" } else { parent }, 0)?;
    if !base.ends_with('/') {
        base.push('/');
    }
    base.push_str(name);
    Ok(base)
}

fn path_str(path: &[u8]) -> Option<&str> {
    core::str::from_utf8(path).ok()
}

// ── VFS operations (ABI mirrors the retired V1 rfs.rs) ────────────────────────

/// Open a regular file for reading. Returns fd (≥ 0) or a negative error code.
pub fn open(path: &[u8]) -> i64 {
    let v = match state() {
        Some(v) => v,
        None => return ENOMNT,
    };
    let p = match path_str(path) {
        Some(s) => s,
        None => return EINVAL,
    };
    let resolved = match resolve(v, p, 0) {
        Ok(r) => r,
        Err(e) => return e,
    };
    let (mount, be, rel) = match v.table.resolve_full(&resolved) {
        Ok(t) => t,
        Err(e) => return errno_mount(e),
    };
    let meta = match be.stat(&rel) {
        Ok(m) => m,
        Err(e) => return errno_fs(e),
    };
    if meta.is_dir {
        return EISDIR;
    }
    if let Err(e) = be.pin(meta.ino) {
        return errno_fs(e);
    }
    let Some(slot) = v.fds.iter().position(|s| s.is_none()) else {
        // Undo the pin we just took; the fd table is full.
        if let Some(be) = v.table.backend_mut(mount) {
            let _ = be.unpin(meta.ino);
        }
        return EMFILE;
    };
    let top = vfs_core::realize::top_component(&rel).map(|c| c.to_string());
    v.fds[slot] = Some(OpenFile {
        mount,
        ino: meta.ino,
        offset: 0,
        size: meta.size,
        writable: false,
        top,
    });
    slot as i64
}

/// Read up to `buf.len()` bytes from `fd` at its current offset.
pub fn read(fd: u64, buf: &mut [u8]) -> i64 {
    let v = match state() {
        Some(v) => v,
        None => return ENOMNT,
    };
    let of = match v.fds.get(fd as usize).and_then(|s| s.as_ref()) {
        Some(f) => f.clone(),
        None => return EBADF,
    };
    let Some(be) = v.table.backend_mut(of.mount) else {
        return EBADF;
    };
    match be.read_at(of.ino, of.offset, buf) {
        Ok(n) => {
            if let Some(Some(f)) = v.fds.get_mut(fd as usize) {
                f.offset += n as u64;
            }
            n as i64
        }
        Err(e) => errno_fs(e),
    }
}

/// Append `buf` to the file (fd must be writable — V1 write semantics: data
/// always lands at end-of-file and the offset moves to the new end).
pub fn write(fd: u64, buf: &[u8]) -> i64 {
    let v = match state() {
        Some(v) => v,
        None => return ENOMNT,
    };
    let of = match v.fds.get(fd as usize).and_then(|s| s.as_ref()) {
        Some(f) => f.clone(),
        None => return EBADF,
    };
    if !of.writable {
        return EBADF;
    }
    // Read-only-after-realize: a writable fd staged into an entry that has
    // since been sealed (temp fd held across the realize rename) must not
    // mutate the now-immutable object.
    if let Some(g) = v.guards.get(&of.mount) {
        if of.top.as_deref().is_some_and(|t| g.is_sealed_name(t)) {
            return EROFS;
        }
    }
    if buf.is_empty() {
        return 0;
    }
    let Some(be) = v.table.backend_mut(of.mount) else {
        return EBADF;
    };
    if let Err(e) = be.write_at(of.ino, of.size, buf) {
        return errno_fs(e);
    }
    if let Err(e) = be.commit() {
        return errno_fs(e);
    }
    if let Some(Some(f)) = v.fds.get_mut(fd as usize) {
        f.size += buf.len() as u64;
        f.offset = f.size;
    }
    buf.len() as i64
}

/// Release `fd`, dropping its open-handle pin. The last pin on an unlinked
/// inode stages its freeing (doc 06 §5), committed here.
pub fn close(fd: u64) -> i64 {
    let v = match state() {
        Some(v) => v,
        None => return ENOMNT,
    };
    let of = match v.fds.get(fd as usize).and_then(|s| s.as_ref()) {
        Some(f) => f.clone(),
        None => return EBADF,
    };
    v.fds[fd as usize] = None;
    let Some(be) = v.table.backend_mut(of.mount) else {
        return 0; // backend unmounted underneath the fd — nothing to unpin
    };
    let _ = be.unpin(of.ino);
    if be.has_staged_changes() {
        if let Err(e) = be.commit() {
            return errno_fs(e);
        }
    }
    0
}

/// Create a new empty regular file. Returns a writable fd (≥ 0).
///
/// TODO(open): ownership at create — see docs/plans/followup-code-tasks.md
/// (rfs2 exposes no uid/gid at create; args accepted for ABI compat).
pub fn create(path: &[u8], _uid: u32, _gid: u32) -> i64 {
    let v = match state() {
        Some(v) => v,
        None => return ENOMNT,
    };
    let p = match path_str(path) {
        Some(s) => s,
        None => return EINVAL,
    };
    let target = match resolve_parent(v, p) {
        Ok(t) => t,
        Err(e) => return e,
    };
    let (mount, _, rel) = match v.table.resolve_full(&target) {
        Ok(t) => t,
        Err(e) => return errno_mount(e),
    };
    if let Some(g) = v.guards.get(&mount) {
        if let Err(e) = g.check_mutate(&rel) {
            return errno_fs(e);
        }
    }
    let Some(be) = v.table.backend_mut(mount) else {
        return ENOMNT;
    };
    let ino = match be.create(&rel) {
        Ok(i) => i,
        Err(e) => return errno_fs(e),
    };
    if let Err(e) = be.pin(ino) {
        return errno_fs(e);
    }
    if let Err(e) = be.commit() {
        return errno_fs(e);
    }
    let Some(slot) = v.fds.iter().position(|s| s.is_none()) else {
        if let Some(be) = v.table.backend_mut(mount) {
            let _ = be.unpin(ino);
            let _ = be.commit();
        }
        return EMFILE;
    };
    let top = vfs_core::realize::top_component(&rel).map(|c| c.to_string());
    v.fds[slot] = Some(OpenFile { mount, ino, offset: 0, size: 0, writable: true, top });
    slot as i64
}

/// Create a directory. Returns 0 on success.
pub fn mkdir(path: &[u8], _uid: u32, _gid: u32) -> i64 {
    let v = match state() {
        Some(v) => v,
        None => return ENOMNT,
    };
    let p = match path_str(path) {
        Some(s) => s,
        None => return EINVAL,
    };
    let target = match resolve_parent(v, p) {
        Ok(t) => t,
        Err(e) => return e,
    };
    let (mount, _, rel) = match v.table.resolve_full(&target) {
        Ok(t) => t,
        Err(e) => return errno_mount(e),
    };
    if let Some(g) = v.guards.get(&mount) {
        if let Err(e) = g.check_mutate(&rel) {
            return errno_fs(e);
        }
    }
    let Some(be) = v.table.backend_mut(mount) else {
        return ENOMNT;
    };
    if let Err(e) = be.mkdir(&rel) {
        return errno_fs(e);
    }
    match be.commit() {
        Ok(()) => 0,
        Err(e) => errno_fs(e),
    }
}

/// Unlink a non-directory. The final component is not followed: unlinking a
/// symlink removes the link, not its target.
pub fn unlink(path: &[u8]) -> i64 {
    let v = match state() {
        Some(v) => v,
        None => return ENOMNT,
    };
    let p = match path_str(path) {
        Some(s) => s,
        None => return EINVAL,
    };
    let target = match resolve_parent(v, p) {
        Ok(t) => t,
        Err(e) => return e,
    };
    let (mount, _, rel) = match v.table.resolve_full(&target) {
        Ok(t) => t,
        Err(e) => return errno_mount(e),
    };
    if let Some(g) = v.guards.get(&mount) {
        if let Err(e) = g.check_mutate(&rel) {
            return errno_fs(e);
        }
    }
    let Some(be) = v.table.backend_mut(mount) else {
        return ENOMNT;
    };
    if let Err(e) = be.unlink(&rel) {
        return errno_fs(e);
    }
    match be.commit() {
        Ok(()) => 0,
        Err(e) => errno_fs(e),
    }
}

/// Rename. One staged transaction, atomically visible at its commit. Both
/// paths must land on the same mount — a cross-mount rename is EINVAL
/// (renames are a single-backend transaction; realize atomicity depends on
/// this).
pub fn rename(old_path: &[u8], new_path: &[u8]) -> i64 {
    let v = match state() {
        Some(v) => v,
        None => return ENOMNT,
    };
    let (op, np) = match (path_str(old_path), path_str(new_path)) {
        (Some(a), Some(b)) => (a, b),
        _ => return EINVAL,
    };
    let old_r = match resolve_parent(v, op) {
        Ok(t) => t,
        Err(e) => return e,
    };
    let new_r = match resolve_parent(v, np) {
        Ok(t) => t,
        Err(e) => return e,
    };
    let (old_mount, _, old_rel) = match v.table.resolve_full(&old_r) {
        Ok(t) => t,
        Err(e) => return errno_mount(e),
    };
    let (new_mount, _, new_rel) = match v.table.resolve_full(&new_r) {
        Ok(t) => t,
        Err(e) => return errno_mount(e),
    };
    if old_mount != new_mount {
        return EINVAL;
    }

    // Read-only-after-realize (design §4.2): on a guarded (store) mount, the
    // rename onto a final store name is the realize commit point.
    // First rename wins and seals; a second rename onto the sealed name is
    // the idempotent re-realize — a no-op success, the caller drops its
    // redundant temp. Mutating a sealed entry (moving it away, or renaming
    // into its interior) is EROFS.
    let seal_after = match v.guards.get(&new_mount) {
        Some(g) => match g.check_rename(&old_rel, &new_rel) {
            Err(e) => return errno_fs(e),
            Ok(RenameOutcome::NoOp) => return 0,
            Ok(RenameOutcome::Proceed { store_name }) => Some(store_name),
        },
        None => None,
    };

    let Some(be) = v.table.backend_mut(new_mount) else {
        return ENOMNT;
    };
    if let Err(e) = be.rename(&old_rel, &new_rel) {
        return errno_fs(e);
    }
    if let Err(e) = be.commit() {
        return errno_fs(e);
    }
    // Seal only after the rename + commit succeeded — the atomic rename is
    // the commit point; a failed rename must not leave a phantom seal.
    if let Some(name) = seal_after {
        if let Some(g) = v.guards.get_mut(&new_mount) {
            g.seal(&name);
        }
        // Retarget open fds that were staged under the renamed entry: their
        // recorded top component still names the temp, but the object they
        // address now lives under the sealed name. Without this, a writable
        // fd held across the realize rename would slip past the seal check
        // (write matches fd.top against sealed names).
        let old_top = vfs_core::realize::top_component(&old_rel).map(|c| c.to_string());
        if old_top.is_some() {
            for slot in v.fds.iter_mut().flatten() {
                if slot.mount == new_mount && slot.top == old_top {
                    slot.top = Some(name.clone());
                }
            }
        }
    }
    0
}

/// Seek within an open fd. `whence`: 0=SET, 1=CUR, 2=END. Returns new offset.
pub fn seek(fd: u64, offset: i64, whence: u32) -> i64 {
    let v = match state() {
        Some(v) => v,
        None => return ENOMNT,
    };
    let of = match v.fds.get_mut(fd as usize).and_then(|s| s.as_mut()) {
        Some(f) => f,
        None => return EBADF,
    };
    let new_offset: i64 = match whence {
        0 => offset,
        1 => of.offset as i64 + offset,
        2 => of.size as i64 + offset,
        _ => return EINVAL,
    };
    if new_offset < 0 {
        return EINVAL;
    }
    of.offset = new_offset as u64;
    new_offset
}

/// Fill `out` with stat info for `path` (symlinks fully followed, as V1 did).
pub fn stat_path(path: &[u8], out: &mut Stat) -> bool {
    let v = match state() {
        Some(v) => v,
        None => return false,
    };
    let Some(p) = path_str(path) else { return false };
    let Ok(resolved) = resolve(v, p, 0) else { return false };
    let Ok((be, rel)) = v.table.resolve(&resolved) else { return false };
    match be.stat(&rel) {
        Ok(meta) => {
            *out = Stat::from(&meta);
            true
        }
        Err(_) => false,
    }
}

/// Directory entries for `path`, or `None` on error.
pub fn readdir_path(path: &[u8]) -> Option<Vec<DirEntry>> {
    let v = state()?;
    let p = path_str(path)?;
    let resolved = resolve(v, p, 0).ok()?;
    let (be, rel) = v.table.resolve(&resolved).ok()?;
    let entries = be.readdir(&rel).ok()?;
    Some(
        entries
            .into_iter()
            .map(|e| DirEntry { ino: e.ino as u32, file_type: e.file_type, name: e.name })
            .collect(),
    )
}

/// Read a whole file into a `Vec<u8>` (exec path). Capped at 32 MiB.
pub fn load_file(path: &str) -> Option<Vec<u8>> {
    let v = state()?;
    let resolved = resolve(v, path, 0).ok()?;
    let (be, rel) = v.table.resolve(&resolved).ok()?;
    let meta = be.stat(&rel).ok()?;
    if meta.is_dir {
        return None;
    }
    let size = meta.size as usize;
    if size == 0 || size > 32 * 1024 * 1024 {
        return None;
    }
    let mut buf = alloc::vec![0u8; size];
    let n = be.read_at(meta.ino, 0, &mut buf).ok()?;
    if n < size {
        return None;
    }
    Some(buf)
}

/// Current committed generation of the ROOT volume (crash-consistency probes).
pub fn generation() -> Option<u64> {
    let v = state()?;
    Some(v.table.backend_mut(v.root_id)?.generation())
}

/// Committed generation of the mount covering `path` (boot probes: proves a
/// mounted volume is a distinct backend instance from root).
pub fn generation_at(path: &str) -> Option<u64> {
    let v = state()?;
    let (be, _) = v.table.resolve(path).ok()?;
    Some(be.generation())
}

/// Whether a mount is installed at exactly `at` (boot probes).
pub fn is_mounted_at(at: &str) -> bool {
    match state() {
        Some(v) => v.table.is_mounted_at(at),
        None => false,
    }
}
