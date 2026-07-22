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

use rfs2::{BlockDevice, Error, GcmTransform, IdentityTransform, Rfs2};
use vfs_core::{
    BackendId, BackendStore, FsBackend, FsError, InodeMeta, MountError, MountId, Namespaces, NsId,
    RealizeGuard, RenameOutcome, ROOT_NS,
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

/// virtio-blk behind `rfs2::BlockDevice`. `dev_index` selects the physical
/// device: [`virtio_blk::DEV_ROOT`] for the root volume, [`virtio_blk::DEV_STORE`]
/// for the persistent `/shade/store` backing.
pub struct VirtioDisk {
    dev_index: usize,
    blocks:    u64,
}

impl BlockDevice for VirtioDisk {
    fn total_blocks(&self) -> u64 {
        self.blocks
    }

    fn read_block(&mut self, block: u64, buf: &mut [u8]) -> rfs2::Result<()> {
        debug_assert_eq!(buf.len(), BLOCK_SIZE);
        if virtio_blk::read_sectors_dev(self.dev_index, block * SECTORS_PER_BLOCK, buf) {
            Ok(())
        } else {
            Err(Error::Io)
        }
    }

    fn write_block(&mut self, block: u64, buf: &[u8]) -> rfs2::Result<()> {
        debug_assert_eq!(buf.len(), BLOCK_SIZE);
        if virtio_blk::write_sectors_dev(self.dev_index, block * SECTORS_PER_BLOCK, buf) {
            Ok(())
        } else {
            Err(Error::Io)
        }
    }

    /// Write barrier (device.rs contract): durable on return. Backed by
    /// VIRTIO_BLK_T_FLUSH when F_FLUSH was negotiated; correct no-op on a
    /// write-through device. The commit pointer-flip (COW-3) rides on this.
    fn flush(&mut self) -> rfs2::Result<()> {
        if virtio_blk::flush_dev(self.dev_index) {
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
/// Frames are reclaimed to the PMM when the `RamDisk` is dropped (see
/// [`Drop for RamDisk`](RamDisk#impl-Drop)). Because the backend is owned by the
/// global [`BackendStore`] and freed there, an unmount (`SYS_UNMOUNT`, or the
/// namespace reaper) drops the backend box and runs that `Drop` — so a volatile
/// RAM scratch volume returns all its memory when torn down. Putting the reclaim
/// in `Drop` rather than an explicit free-on-unmount means no future call site
/// that drops a `RamDisk` can forget to free its frames.
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

impl Drop for RamDisk {
    /// Return every frame the volume held to the PMM. This is the *sole* reclaim
    /// path for RAM-disk frames: `new` inserts them into `frames` only on full
    /// success (its own failure rollback frees the partial set and never
    /// constructs a `RamDisk`, so those never reach here — no double free), and
    /// no other code frees a mounted volume's frames. `pmm::free_frame` itself
    /// panics on a double free, so an accidental second reclaim would be loud,
    /// not silent. Nothing outside `self.frames` holds the addresses — the
    /// backend box is the RAM disk's only owner, and open fds re-address by
    /// `BackendId` through the `BackendStore` (which no longer contains this
    /// backend once it is being dropped), so there is no use-after-free.
    fn drop(&mut self) {
        for &pa in &self.frames {
            crate::pmm::free_frame(crate::pmm::PhysAddr(pa));
        }
    }
}

/// Wall clock for inode timestamps: nanoseconds, as `MkfsOptions::now`
/// documents. The kernel's anchor has millisecond resolution.
fn clock_ns() -> u64 {
    crate::time::epoch_ms() * 1_000_000
}

// ── Rfs2 behind vfs_core::FsBackend ───────────────────────────────────────────

/// Adapter: any `Rfs2<D, T>` as a boxable `FsBackend`, over any block transform
/// — the plaintext [`IdentityTransform`] or the encrypting [`GcmTransform`]
/// (doc 08). Both coerce to `Box<dyn FsBackend>`, so a plaintext and an
/// encrypted volume mount through identical code.
struct Rfs2Backend<D: BlockDevice, T: rfs2::BlockTransform> {
    fs: Rfs2<D, T>,
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

impl<D: BlockDevice, T: rfs2::BlockTransform> FsBackend for Rfs2Backend<D, T> {
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
    fn symlink(&mut self, target: &str, link: &str) -> Result<u64, FsError> {
        self.fs.symlink(target, link).map_err(fs_err)
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
    fn rmdir(&mut self, path: &str) -> Result<(), FsError> {
        self.fs.rmdir(path).map_err(fs_err)
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

/// Set once the root volume mounts encrypted (doc 08). Drives the
/// single-passphrase store: when the root is encrypted, the persistent store is
/// too, keyed by a DEK persisted inside the (encrypted) root — one prompt
/// unlocks both. A plaintext root ⇒ a plaintext store (backward compatible).
static ROOT_ENCRYPTED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

/// Raw store DEK file inside the encrypted root (doc 08 §6, single passphrase).
const STORE_KEY_PATH: &str = "/lth/system/store.key";

#[derive(Clone)]
struct OpenFile {
    /// Which backend the fd was opened on — the `Copy` [`MountId`]/`BackendId`
    /// (one identity; see `vfs_core::mount`). An fd keeps addressing its backend
    /// through this id even as other mounts come and go, because backends live
    /// in the global [`BackendStore`] keyed by exactly this id, not in the
    /// (namespace-local, clonable) routing table.
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
    /// The single owner of every mounted backend, keyed by [`BackendId`]. A
    /// field **disjoint** from `table`: an FS handler resolves against `table`
    /// (borrowing it alone, taking a `Copy` id) and then borrows exactly one
    /// backend here for one operation — the two borrows never alias, so the
    /// "one `&mut`, one op" discipline holds by lexical scoping with no
    /// `RefCell` (docs/plans/per-task-mount-namespace.md §5.1).
    backends: BackendStore,
    /// The per-task mount namespaces (routing only: each is a `MountTable` of
    /// `prefix → BackendId`, owning no backend). Stage 1 holds a single pinned
    /// root namespace ([`ROOT_NS`]) carrying the whole-machine view; every FS
    /// syscall still resolves against it via [`current_ns`] until stage 3 makes
    /// resolution task-relative (docs/plans/per-task-mount-namespace.md §2).
    /// A field **disjoint** from `backends`, so route resolution and the one
    /// backend `&mut` never alias.
    namespaces: Namespaces,
    root_id: BackendId,
    fds:     [Option<OpenFile>; MAX_FDS],
    /// Read-only-after-realize guards, keyed by the guarded backend's id,
    /// present only for mounts created with MOUNT_STORE (stage 2 —
    /// docs/plans/mount-syscall-shade-store.md §4.2).
    guards:  BTreeMap<BackendId, RealizeGuard>,
}

struct VfsState(core::cell::UnsafeCell<Option<Vfs>>);

// SAFETY: single-threaded kernel — no concurrent access (see module docs).
unsafe impl Sync for VfsState {}

static STATE: VfsState = VfsState(core::cell::UnsafeCell::new(None));

#[inline]
fn state() -> Option<&'static mut Vfs> {
    unsafe { (*STATE.0.get()).as_mut() }
}

/// The namespace the current syscall resolves against. Stage 1: every task
/// still shares the root namespace, so this is always [`ROOT_NS`] and the
/// system behaves exactly as before namespaces existed. Stage 3 makes this the
/// calling task's own namespace (docs/plans/per-task-mount-namespace.md §2);
/// keeping it a single free function (borrowing no [`Vfs`]) means that
/// conversion touches exactly one body, and every FS handler already threads
/// its result.
#[inline]
fn current_ns() -> NsId {
    ROOT_NS
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
    let blocks = sectors / SECTORS_PER_BLOCK;

    // Peek the plaintext static header (block 0) to see whether the root volume
    // is encrypted (doc 08 §2: block 0 is always plaintext geometry + KDF
    // params). This decides the block transform before we mount.
    let mut disk = VirtioDisk { dev_index: virtio_blk::DEV_ROOT, blocks };
    let mut hbuf = alloc::vec![0u8; BLOCK_SIZE];
    let header = match disk.read_block(0, &mut hbuf) {
        Ok(()) => rfs2::StaticHeader::decode(&hbuf).ok(),
        Err(_) => None,
    };
    let encrypted = header
        .as_ref()
        .is_some_and(|h| h.feature_incompat & rfs2::layout::INCOMPAT_ENCRYPTION != 0);

    let backend: Box<dyn FsBackend> = if encrypted {
        // SAFETY of unwrap: `encrypted` implies the header decoded.
        match mount_encrypted_root(blocks, header.as_ref().unwrap()) {
            Some(b) => b,
            None => {
                crate::kprintln!("[vfs] root remains locked — no root filesystem mounted");
                return false;
            }
        }
    } else {
        match Rfs2::mount(disk, IdentityTransform, clock_ns) {
            Ok(fs) => {
                log_root_mounted(&fs);
                Box::new(Rfs2Backend { fs })
            }
            Err(e) => {
                crate::kprintln!("[vfs] rfs2 root mount failed: {:?}", e);
                return false;
            }
        }
    };

    // Remember the root's confidentiality for the store mount (single
    // passphrase): only reached here after a successful mount of either kind.
    ROOT_ENCRYPTED.store(encrypted, core::sync::atomic::Ordering::Relaxed);

    let mut namespaces = Namespaces::new();
    let mut backends = BackendStore::new();
    // `/` is mounted into the pinned root namespace; every other namespace is
    // derived from it later. Boot behaviour is unchanged — one table, one view.
    let root_id = match namespaces.root_table_mut().mount("/", backend, &mut backends) {
        Ok(id) => id,
        Err(_) => return false, // impossible: empty root namespace accepts "/"
    };
    unsafe {
        *STATE.0.get() = Some(Vfs {
            backends,
            namespaces,
            root_id,
            fds: [const { None }; MAX_FDS],
            guards: BTreeMap::new(),
        })
    };
    true
}

/// Log a freshly-mounted root volume (either transform).
fn log_root_mounted<D: BlockDevice, T: rfs2::BlockTransform>(fs: &Rfs2<D, T>) {
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
}

/// Prompt for the passphrase and mount the encrypted root (doc 08 §6). Up to
/// three attempts; a wrong passphrase is a loud retry, a post-unlock mount
/// failure aborts. On success `ROOT_ENCRYPTED` is set, which drives the
/// single-passphrase store: the store's own DEK is minted once and persisted
/// inside this now-decrypted root (see [`obtain_store_dek`]), so the one prompt
/// here unlocks both volumes without a second KDF.
fn mount_encrypted_root(blocks: u64, header: &rfs2::StaticHeader) -> Option<Box<dyn FsBackend>> {
    crate::kprintln!("[vfs] root volume is encrypted (AES-256-GCM) — unlock required");
    for attempt in 1..=3u32 {
        let mut pass = crate::kdf::read_passphrase("  root passphrase: ");
        let dek = crate::kdf::open_volume_dek(&pass, header);
        pass.iter_mut().for_each(|b| *b = 0); // zeroize passphrase bytes
        match dek {
            Some(mut dek) => {
                let disk = VirtioDisk { dev_index: virtio_blk::DEV_ROOT, blocks };
                let r = Rfs2::mount(disk, GcmTransform::new(&dek), clock_ns);
                // The DEK now lives only inside the cipher's key schedule.
                dek.iter_mut().for_each(|b| *b = 0);
                match r {
                    Ok(fs) => {
                        log_root_mounted(&fs);
                        return Some(Box::new(Rfs2Backend { fs }));
                    }
                    Err(e) => {
                        crate::kprintln!(
                            "[vfs] encrypted root mount failed after unlock: {:?} \
                             (header/superblock inconsistent)",
                            e
                        );
                        return None;
                    }
                }
            }
            None => {
                crate::kprintln!("[vfs] wrong passphrase or tampered header ({attempt}/3)");
            }
        }
    }
    None
}

// ── Encrypted store (doc 08 §6, single passphrase) ───────────────────────────

/// Create-or-overwrite a small file in the root volume and commit. Persists the
/// store key into the (already-mounted) encrypted root. `false` on any error.
fn write_root_file(path: &str, data: &[u8]) -> bool {
    let Some(v) = state() else { return false };
    let target = match resolve_parent(v, ROOT_NS, path) {
        Ok(t) => t,
        Err(_) => return false,
    };
    let (mount, rel) = match v.namespaces.resolve(ROOT_NS, &target) {
        Ok(t) => t,
        Err(_) => return false,
    };
    let Some(be) = v.backends.get(mount) else { return false };
    let ino = match be.create(&rel) {
        Ok(i) => i,
        Err(FsError::Exists) => match be.lookup(&rel) {
            Ok(i) => i,
            Err(_) => return false,
        },
        Err(_) => return false,
    };
    if be.write_at(ino, 0, data).is_err() {
        return false;
    }
    be.commit().is_ok()
}

/// The store DEK for an encrypted root. Reads the persisted key from the root;
/// if absent AND the store disk is genuinely blank, mints a fresh key (RDRAND)
/// and persists it. Refuses (loud) if the store is already formatted but its
/// key is missing — a lost key is never a silent reformat; sealed content stays
/// sealed.
fn obtain_store_dek(blocks: u64) -> Result<[u8; 32], i64> {
    if let Some(k) = load_file(STORE_KEY_PATH) {
        if k.len() != 32 {
            crate::kprintln!("[vfs] {STORE_KEY_PATH} malformed ({} bytes)", k.len());
            return Err(EIO);
        }
        let mut dek = [0u8; 32];
        dek.copy_from_slice(&k);
        return Ok(dek);
    }
    // No key in root. Safe to mint one only if the store is genuinely blank.
    let mut disk = VirtioDisk { dev_index: virtio_blk::DEV_STORE, blocks };
    let mut hbuf = alloc::vec![0u8; BLOCK_SIZE];
    let formatted =
        disk.read_block(0, &mut hbuf).is_ok() && rfs2::StaticHeader::decode(&hbuf).is_ok();
    if formatted {
        crate::kprintln!(
            "[vfs] store is formatted but its key is missing from the encrypted root — \
             refusing (no silent reformat, sealed content preserved)"
        );
        return Err(EIO);
    }
    let dek = crate::kdf::rand_key().ok_or_else(|| {
        crate::kprintln!("[vfs] no hardware RNG (RDRAND) available to mint a store key");
        EIO
    })?;
    if !write_root_file(STORE_KEY_PATH, &dek) {
        crate::kprintln!("[vfs] failed to persist {STORE_KEY_PATH} to the encrypted root");
        return Err(EIO);
    }
    crate::kprintln!("[vfs] minted a fresh store key, persisted to the encrypted root");
    Ok(dek)
}

/// Mount the persistent store, formatting it once if blank. Generic over the
/// transform so a plaintext store (`IdentityTransform`) and an encrypted one
/// (`GcmTransform`) share this exact code (doc 08 "one implementation, two
/// mounts"). `make_xform` yields a fresh transform per (re)mount/format.
fn mount_or_format_store<T, F>(blocks: u64, make_xform: F) -> Result<Box<dyn FsBackend>, i64>
where
    T: rfs2::BlockTransform + 'static,
    F: Fn() -> T,
{
    let make_disk = || VirtioDisk { dev_index: virtio_blk::DEV_STORE, blocks };
    match Rfs2::mount(make_disk(), make_xform(), clock_ns) {
        Ok(fs) => {
            crate::kprintln!(
                "[vfs] rfs2 persistent store mounted (gen={} slot={} blocks={})",
                fs.generation(),
                if fs.current_slot() == 1 { "A" } else { "B" },
                blocks,
            );
            Ok(Box::new(Rfs2Backend { fs }))
        }
        Err(Error::NoSuperblock) | Err(Error::BadHeader) => {
            crate::kprintln!("[vfs] rfs2 persistent store: blank disk — formatting once");
            let mut dev = make_disk();
            // feature_incompat is taken from the transform: GcmTransform stamps
            // INCOMPAT_ENCRYPTION, IdentityTransform stamps 0. The store DEK is
            // sourced from the root, so no header wrap fields are needed.
            let opts = rfs2::MkfsOptions {
                uuid: [0u8; 16],
                label: "shade-store",
                now: clock_ns(),
                crypto: None,
            };
            if rfs2::mkfs(&mut dev, &make_xform(), &opts).is_err() {
                return Err(EIO);
            }
            match Rfs2::mount(make_disk(), make_xform(), clock_ns) {
                Ok(fs) => Ok(Box::new(Rfs2Backend { fs })),
                Err(e) => {
                    crate::kprintln!("[vfs] rfs2 store mount-after-format failed: {:?}", e);
                    Err(EIO)
                }
            }
        }
        Err(e) => {
            crate::kprintln!("[vfs] rfs2 persistent store mount failed: {:?}", e);
            Err(EIO)
        }
    }
}

/// SYS_MOUNT backend selector values (ABI: abi/lythos-abi/src/syscall.rs).
/// Volatile fresh RAM-backed volume, formatted at mount time.
pub const MOUNT_SRC_RFS2_RAM: u64 = 0;
/// Persistent RFS V2 on the secondary virtio-blk device (`store.img`):
/// mounted if already formatted, formatted-then-mounted on first ever boot.
/// Content survives a power cycle.
pub const MOUNT_SRC_RFS2_BLK: u64 = 1;
/// SYS_MOUNT flags (ABI): attach the realize-guard (read-only-after-realize
/// store semantics) to this mount.
pub const MOUNT_STORE: u64 = 1 << 0;

/// Mount a new backend instance at `at`. Capability enforcement happens in
/// the SYS_MOUNT handler (syscall boundary), not here.
///
/// `source` is `MOUNT_SRC_RFS2_RAM` (volatile) or `MOUNT_SRC_RFS2_BLK`
/// (persistent, on the secondary virtio-blk device); `flags` may set
/// `MOUNT_STORE` to attach a [`RealizeGuard`] (read-only-after-realize).
pub fn mount(at: &str, source: u64, flags: u64) -> i64 {
    let v = match state() {
        Some(v) => v,
        None => return ENOMNT,
    };
    if flags & !MOUNT_STORE != 0 {
        return EINVAL;
    }
    let store = flags & MOUNT_STORE != 0;
    let ns = current_ns();

    // Reject early (cheaply) before building the backend.
    if v.namespaces.is_mounted_at(ns, at) {
        return EMOUNTED;
    }

    // Build the RFS2 backend for the requested source.
    let backend: Box<dyn FsBackend> = match source {
        MOUNT_SRC_RFS2_RAM => {
            let Some(mut dev) = RamDisk::new(RAM_DISK_BLOCKS) else {
                return ENOSPC;
            };
            let opts =
                rfs2::MkfsOptions { uuid: [0u8; 16], label: "ram", now: clock_ns(), crypto: None };
            if rfs2::mkfs(&mut dev, &IdentityTransform, &opts).is_err() {
                return EIO;
            }
            match Rfs2::mount(dev, IdentityTransform, clock_ns) {
                Ok(fs) => Box::new(Rfs2Backend { fs }),
                Err(e) => {
                    crate::kprintln!("[vfs] ram rfs2 mount failed: {:?}", e);
                    return EIO;
                }
            }
        }
        MOUNT_SRC_RFS2_BLK => {
            // Persistent backing: the secondary virtio-blk device. Must be
            // present (probed at boot as instance 1).
            if !virtio_blk::is_present_dev(virtio_blk::DEV_STORE) {
                crate::kprintln!("[vfs] rfs2 blk mount: no persistent store device (instance 1)");
                return EIO;
            }
            let sectors = virtio_blk::capacity_sectors_dev(virtio_blk::DEV_STORE);
            let blocks = sectors / SECTORS_PER_BLOCK;
            if blocks == 0 {
                return EIO;
            }
            // Single-passphrase FDE: when the root is encrypted the store is too,
            // keyed by a DEK persisted inside the encrypted root (one prompt
            // unlocks both). A plaintext root ⇒ a plaintext store. VirtioDisk is
            // cheap and stateless; all volume state lives on the disk.
            if ROOT_ENCRYPTED.load(core::sync::atomic::Ordering::Relaxed) {
                let mut dek = match obtain_store_dek(blocks) {
                    Ok(d) => d,
                    Err(e) => return e,
                };
                crate::kprintln!("[vfs] store volume is encrypted (AES-256-GCM, key from root)");
                let r = mount_or_format_store(blocks, || GcmTransform::new(&dek));
                dek.iter_mut().for_each(|b| *b = 0); // DEK now only in cipher schedules
                match r {
                    Ok(b) => b,
                    Err(e) => return e,
                }
            } else {
                match mount_or_format_store(blocks, || IdentityTransform) {
                    Ok(b) => b,
                    Err(e) => return e,
                }
            }
        }
        _ => return EINVAL,
    };

    match v.namespaces.mount(ns, at, backend, &mut v.backends) {
        Ok(id) => {
            if store {
                // Reconstruct the realize-seal set from the persisted store
                // contents. CRITICAL for MOUNT_SRC_RFS2_BLK: the seal set lives
                // only in kernel RAM, so a cold boot starts empty while the disk
                // still holds sealed content — without this, that content would
                // be writable again, breaking the "seal is absolute" invariant
                // across a power cycle. Granularity matches `seal()` exactly:
                // one top-level store name per sealed entry, so post-reboot the
                // NoOp/EROFS rename decisions match the pre-reboot ones. Temp
                // dirs (`.tmp-*`) and `.`/`..` are never sealed — skipped by the
                // dot prefix. Harmless on a fresh RAM store (empty readdir).
                let mut guard = RealizeGuard::new();
                if let Some(be) = v.backends.get(id) {
                    if let Ok(entries) = be.readdir("/") {
                        for e in entries {
                            if e.file_type == FT_DIR && !e.name.starts_with('.') {
                                guard.seal(&e.name);
                            }
                        }
                    }
                }
                let reconstructed = guard.sealed_count();
                v.guards.insert(id, guard);
                crate::kprintln!(
                    "[vfs] store mount at {} — {} sealed entr{} reconstructed",
                    at,
                    reconstructed,
                    if reconstructed == 1 { "y" } else { "ies" },
                );
            } else {
                crate::kprintln!("[vfs] rfs2 volume mounted at {}", at);
            }
            0
        }
        Err(e) => errno_mount(e),
    }
}

/// Unmount the filesystem at exactly `at` and free its backend. Teardown in the
/// order the namespace lifetime work (docs/plans/per-task-mount-namespace.md §4)
/// will reuse: drop the route first, then free the backend from the global
/// [`BackendStore`] **only once no route still reaches it** — so removing one
/// route to a shared backend (future namespaces) never frees a backend another
/// route still names, and, conversely, a dropped route never leaves a dangling
/// [`BackendId`] behind. The realize guard, keyed by the same id, is dropped
/// with the backend. Open fds addressing the freed backend fail gracefully
/// afterwards (their `BackendStore::get` returns `None` → `EBADF`/no-op), the
/// same "backend unmounted underneath the fd" path `close`/`read` already take.
///
/// Wired to `SYS_UNMOUNT` (cap-gated on the syscall boundary exactly like
/// `SYS_MOUNT`); it is also the teardown primitive the namespace reaper will
/// call once per-task namespaces tear down on exit (§4).
pub fn unmount(at: &str) -> i64 {
    let v = match state() {
        Some(v) => v,
        None => return ENOMNT,
    };
    let ns = current_ns();
    // Root is pinned — the whole system routes through it (§4 root-namespace
    // pinning); refuse to strand every path with no `/`. The root backend is
    // pinned regardless of the namespace the unmount runs in.
    if v.namespaces
        .resolve(ns, at)
        .map(|(id, rel)| id == v.root_id && rel == "/")
        .unwrap_or(false)
    {
        return EINVAL;
    }
    let freed = match v.namespaces.unmount(ns, at) {
        Ok(id) => id,
        Err(e) => return errno_mount(e),
    };
    // Free the backend + its guard only when the last route to it is gone
    // across ALL namespaces — a backend shared into another namespace's routing
    // table must outlive the removal of this one route (§5.1 teardown invariant).
    if !v.namespaces.any_routes_to(freed) {
        v.guards.remove(&freed);
        v.backends.remove(freed);
    }
    0
}

// ── Path resolution (symlink following, mount-aware) ──────────────────────────

/// Canonicalize `path`, following symlinks in every component (hop-capped).
/// Every stat/readlink routes through the mount table, so each hop lands on
/// the covering backend — including a symlink target that crosses a mount
/// boundary. Returns a full (mount-side) path whose components are fully
/// resolved.
fn resolve(v: &mut Vfs, ns: NsId, path: &str, hops: usize) -> Result<String, i64> {
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
        // Route in the caller's namespace → Copy BackendId (routing borrow
        // ends), then borrow that one backend for this hop's stat. `meta`/
        // `target` are owned, so the backend borrow is released before the
        // tail-recursive `resolve` below re-borrows the namespace — the
        // reentrancy safety is structural, not a matter of a borrow ending
        // "just in time".
        let (bid, rel) = v.namespaces.resolve(ns, &cand).map_err(errno_mount)?;
        let be = v.backends.get(bid).ok_or(ENOMNT)?;
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
            return resolve(v, ns, &next, hops + 1);
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
fn resolve_parent(v: &mut Vfs, ns: NsId, path: &str) -> Result<String, i64> {
    if !path.starts_with('/') || path.ends_with('/') {
        return Err(EINVAL);
    }
    let idx = path.rfind('/').unwrap();
    let (parent, name) = (&path[..idx], &path[idx + 1..]);
    if name.is_empty() {
        return Err(EINVAL);
    }
    let mut base = resolve(v, ns, if parent.is_empty() { "/" } else { parent }, 0)?;
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
    let ns = current_ns();
    let resolved = match resolve(v, ns, p, 0) {
        Ok(r) => r,
        Err(e) => return e,
    };
    let (mount, rel) = match v.namespaces.resolve(ns, &resolved) {
        Ok(t) => t,
        Err(e) => return errno_mount(e),
    };
    let Some(be) = v.backends.get(mount) else { return ENOMNT };
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
        if let Some(be) = v.backends.get(mount) {
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
    let Some(be) = v.backends.get(of.mount) else {
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
    let Some(be) = v.backends.get(of.mount) else {
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
    let Some(be) = v.backends.get(of.mount) else {
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
    let target = match resolve_parent(v, current_ns(), p) {
        Ok(t) => t,
        Err(e) => return e,
    };
    let (mount, rel) = match v.namespaces.resolve(current_ns(), &target) {
        Ok(t) => t,
        Err(e) => return errno_mount(e),
    };
    if let Some(g) = v.guards.get(&mount) {
        if let Err(e) = g.check_mutate(&rel) {
            return errno_fs(e);
        }
    }
    let Some(be) = v.backends.get(mount) else {
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
        if let Some(be) = v.backends.get(mount) {
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
    let target = match resolve_parent(v, current_ns(), p) {
        Ok(t) => t,
        Err(e) => return e,
    };
    let (mount, rel) = match v.namespaces.resolve(current_ns(), &target) {
        Ok(t) => t,
        Err(e) => return errno_mount(e),
    };
    // A path that resolves to a mount root (backend-relative "/") is the mount
    // point itself — it always exists. The backend's mkdir would reject "/"
    // with Invalid (no parent to split), so report the true condition: EEXIST.
    // `mkdir -p` over a store root (`create_dir_all` in shade-store) relies on
    // this — it tolerates Exists, not Invalid.
    if rel == "/" {
        return EEXIST;
    }
    if let Some(g) = v.guards.get(&mount) {
        if let Err(e) = g.check_mutate(&rel) {
            return errno_fs(e);
        }
    }
    let Some(be) = v.backends.get(mount) else {
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
    let target = match resolve_parent(v, current_ns(), p) {
        Ok(t) => t,
        Err(e) => return e,
    };
    let (mount, rel) = match v.namespaces.resolve(current_ns(), &target) {
        Ok(t) => t,
        Err(e) => return errno_mount(e),
    };
    if let Some(g) = v.guards.get(&mount) {
        if let Err(e) = g.check_mutate(&rel) {
            return errno_fs(e);
        }
    }
    let Some(be) = v.backends.get(mount) else {
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

/// Recursively remove the subtree at backend-relative `path` on `be`: unlink
/// every file/symlink, rmdir every directory, depth-first. Operates directly
/// on the backend — it never routes through the RealizeGuard, so it removes a
/// sealed store tree BELOW the seal (it never opens a sealed file for write).
/// `is_dir` says whether `path` itself is a directory (from the caller's stat /
/// the parent's readdir file_type — never re-stat'd here, to avoid following a
/// symlink). `NotFound` at any node is treated as already-gone (idempotent).
fn remove_tree_rec(be: &mut dyn FsBackend, path: &str, is_dir: bool) -> Result<(), FsError> {
    if is_dir {
        let entries = match be.readdir(path) {
            Ok(e) => e,
            Err(FsError::NotFound) => return Ok(()),
            Err(e) => return Err(e),
        };
        for e in entries {
            if e.name == "." || e.name == ".." {
                continue;
            }
            let child = if path.ends_with('/') {
                alloc::format!("{path}{}", e.name)
            } else {
                alloc::format!("{path}/{}", e.name)
            };
            // file_type: 2 = dir (rfs2 FT_DIR); everything else (file, symlink)
            // is unlinked, so a symlink is removed as a link, never followed.
            remove_tree_rec(be, &child, e.file_type == FT_DIR)?;
        }
        match be.rmdir(path) {
            Ok(()) | Err(FsError::NotFound) => Ok(()),
            Err(e) => Err(e),
        }
    } else {
        match be.unlink(path) {
            Ok(()) | Err(FsError::NotFound) => Ok(()),
            Err(e) => Err(e),
        }
    }
}

/// Remove the whole unreferenced store entry that `path` names (SYS_STORE_REMOVE
/// — the sole store-reclamation primitive after SYS_UNSEAL's removal). `path` is
/// a full path into a realize-guarded (store) mount; its single backend-relative
/// component is the top-level store name. The entire tree is deleted (files
/// unlinked, dirs rmdir'd) and its blocks freed via RFS2 free-space — operating
/// BELOW the seal (the seal is never lifted in place; sealed content is never
/// made writable). The name's in-kernel seal is then dropped as the last step of
/// the lifecycle removal. Content-addressing keeps this safe: a later realize of
/// the same digest reproduces byte-identical content.
///
/// Returns 0 on success (idempotent — already-gone is success), `EINVAL` if the
/// covering mount is not a store mount or `path` is not a single top-level entry
/// (whole-path removal only — never a nested delete inside a sealed tree), or the
/// usual mount / fs errnos. Capability enforcement (Filesystem+WRITE) is on the
/// syscall boundary, not here.
///
/// Atomicity vs realize (first-writer-wins, not timing): the kernel is
/// single-threaded and in-kernel FS paths are not preempted, so this whole
/// removal (delete tree + commit + forget seal) and a concurrent realize of the
/// same digest (rename + seal) are each an indivisible syscall — they never
/// interleave. A realize that runs after this removal simply recreates the
/// entry and re-seals it (content-addressed, byte-identical); one that ran
/// before it sealed the name this removal then deletes. There is no window in
/// which a half-removed tree is observable or in which a realize sees a
/// partially-forgotten seal.
pub fn store_remove_tree(path: &[u8]) -> i64 {
    let v = match state() {
        Some(v) => v,
        None => return ENOMNT,
    };
    let p = match path_str(path) {
        Some(s) => s,
        None => return EINVAL,
    };
    let (mount, rel) = match v.namespaces.resolve(current_ns(), p) {
        Ok(t) => t,
        Err(e) => return errno_mount(e),
    };
    // Must be a store (realize-guarded) mount — nothing to reclaim elsewhere.
    if !v.guards.contains_key(&mount) {
        return EINVAL;
    }
    let Some(name) = vfs_core::realize::top_component(&rel) else {
        return EINVAL; // the mount root itself is never a store entry
    };
    // Whole-path removal only: `rel` must be exactly one component. A nested
    // path would be a partial delete inside a sealed tree — forbidden.
    if rel.split('/').filter(|c| !c.is_empty()).count() != 1 {
        return EINVAL;
    }
    let name = name.to_string();

    let Some(be) = v.backends.get(mount) else {
        return ENOMNT;
    };
    // Determine whether the top-level entry is a directory. Store tops are a
    // directory (an output tree) or a regular file (a `.drv`) — never a symlink,
    // so this stat cannot follow one. NotFound ⇒ already reclaimed: success.
    let is_dir = match be.stat(&rel) {
        Ok(m) => m.is_dir,
        Err(FsError::NotFound) => {
            // Nothing on disk; still drop any stale seal and report success.
            if let Some(g) = v.guards.get_mut(&mount) {
                g.forget(&name);
            }
            return 0;
        }
        Err(e) => return errno_fs(e),
    };
    if let Err(e) = remove_tree_rec(be, &rel, is_dir) {
        return errno_fs(e);
    }
    if let Err(e) = be.commit() {
        return errno_fs(e);
    }
    // Lifecycle-forget the seal: the path no longer exists. This is not an
    // in-place unseal — sealed content was never made writable.
    if let Some(g) = v.guards.get_mut(&mount) {
        g.forget(&name);
    }
    0
}

/// Create a symlink at `link_path` storing `target` verbatim (never resolved
/// — dangling links are legal; `target` crossing a mount boundary resolves at
/// *follow* time through the mount table, not here). The final component of
/// `link_path` is not followed; on a realize-guarded (store) mount, creating
/// a link inside a sealed entry is EROFS.
pub fn symlink(target: &[u8], link_path: &[u8]) -> i64 {
    let v = match state() {
        Some(v) => v,
        None => return ENOMNT,
    };
    let (t, p) = match (path_str(target), path_str(link_path)) {
        (Some(t), Some(p)) => (t, p),
        _ => return EINVAL,
    };
    if t.is_empty() {
        return EINVAL;
    }
    let link_r = match resolve_parent(v, current_ns(), p) {
        Ok(r) => r,
        Err(e) => return e,
    };
    let (mount, rel) = match v.namespaces.resolve(current_ns(), &link_r) {
        Ok(t) => t,
        Err(e) => return errno_mount(e),
    };
    if let Some(g) = v.guards.get(&mount) {
        if let Err(e) = g.check_mutate(&rel) {
            return errno_fs(e);
        }
    }
    let Some(be) = v.backends.get(mount) else {
        return ENOMNT;
    };
    if let Err(e) = be.symlink(t, &rel) {
        return errno_fs(e);
    }
    match be.commit() {
        Ok(()) => 0,
        Err(e) => errno_fs(e),
    }
}

/// Read a symlink's target into `out`. The final component is not followed
/// (it must BE the symlink; EINVAL otherwise). Returns the target length —
/// truncated to `out.len()` bytes copied if the buffer is short (caller
/// detects truncation by `ret > out.len()`).
pub fn readlink(path: &[u8], out: &mut [u8]) -> i64 {
    let v = match state() {
        Some(v) => v,
        None => return ENOMNT,
    };
    let p = match path_str(path) {
        Some(s) => s,
        None => return EINVAL,
    };
    let target = match resolve_parent(v, current_ns(), p) {
        Ok(t) => t,
        Err(e) => return e,
    };
    let (mount, rel) = match v.namespaces.resolve(current_ns(), &target) {
        Ok(t) => t,
        Err(e) => return errno_mount(e),
    };
    let Some(be) = v.backends.get(mount) else { return ENOMNT };
    match be.readlink(&rel) {
        Ok(t) => {
            let n = t.len().min(out.len());
            out[..n].copy_from_slice(&t.as_bytes()[..n]);
            t.len() as i64
        }
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
    let ns = current_ns();
    let old_r = match resolve_parent(v, ns, op) {
        Ok(t) => t,
        Err(e) => return e,
    };
    let new_r = match resolve_parent(v, ns, np) {
        Ok(t) => t,
        Err(e) => return e,
    };
    let (old_mount, old_rel) = match v.namespaces.resolve(ns, &old_r) {
        Ok(t) => t,
        Err(e) => return errno_mount(e),
    };
    let (new_mount, new_rel) = match v.namespaces.resolve(ns, &new_r) {
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

    let Some(be) = v.backends.get(new_mount) else {
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
    let ns = current_ns();
    let Ok(resolved) = resolve(v, ns, p, 0) else { return false };
    let Ok((mount, rel)) = v.namespaces.resolve(ns, &resolved) else { return false };
    let Some(be) = v.backends.get(mount) else { return false };
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
    let ns = current_ns();
    let resolved = resolve(v, ns, p, 0).ok()?;
    let (mount, rel) = v.namespaces.resolve(ns, &resolved).ok()?;
    let be = v.backends.get(mount)?;
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
    let ns = current_ns();
    let resolved = resolve(v, ns, path, 0).ok()?;
    let (mount, rel) = v.namespaces.resolve(ns, &resolved).ok()?;
    let be = v.backends.get(mount)?;
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
    Some(v.backends.get(v.root_id)?.generation())
}

/// Committed generation of the mount covering `path` (boot probes: proves a
/// mounted volume is a distinct backend instance from root).
pub fn generation_at(path: &str) -> Option<u64> {
    let v = state()?;
    let (mount, _) = v.namespaces.resolve(ROOT_NS, path).ok()?;
    Some(v.backends.get(mount)?.generation())
}

/// Whether a mount is installed at exactly `at` (boot probes).
pub fn is_mounted_at(at: &str) -> bool {
    match state() {
        Some(v) => v.namespaces.is_mounted_at(ROOT_NS, at),
        None => false,
    }
}
