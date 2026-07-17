//! RFS V2 — copy-on-write filesystem for PureshadeOS, per `docs/rfs-v2/` (01–10).
//!
//! Implemented here: on-disk layout (02), dual superblock + generation flip (03),
//! COW write path + commit (04), mount-time mark-and-sweep space management (05),
//! 128-byte inodes + COW radix inode map + per-file block map (06), ext2-style
//! directories (07), crash consistency (09), format/feature flags/mkfs (10).
//!
//! NOT implemented: the encryption layer (08). All block sealing/opening is routed
//! through the [`transform::BlockTransform`] seam; the shipped impl is the no-op
//! [`transform::IdentityTransform`] (zero tags, no confidentiality, no tamper
//! detection). TODO(crypto): AES-256-GCM + Argon2id per doc 08 land behind that
//! trait once the 128-bit-nonce (`block ‖ gen`) KAT against SP 800-38D clears.
//!
//! The crate is `no_std + alloc` so the kernel can consume it unchanged; tests run
//! on the host against an in-memory block device.

#![no_std]

extern crate alloc;

pub mod crypto;
pub mod device;
pub mod dirent;
pub mod fs;
pub mod inode;
pub mod layout;
pub mod space;
pub mod superblock;
pub mod transform;

pub use device::BlockDevice;
pub use fs::{mkfs, DirEntryOut, MkfsOptions, Rfs2};
pub use inode::Inode;
pub use layout::{BlockPtr, StaticHeader};
pub use superblock::Superblock;
pub use crypto::CryptoParams;
#[cfg(feature = "crypto")]
pub use crypto::{open_dek, seal_dek, GcmTransform, WrapGeometry};
pub use transform::{BlockTransform, IdentityTransform};

/// Filesystem error type. Mapping to Lythos errno sentinels happens at the
/// kernel integration boundary, not here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// Device-level I/O failure.
    Io,
    /// A block failed transform authentication (doc 02 §4: torn, tampered,
    /// replayed, or relocated). Never tolerated; the subtree is unreadable.
    Auth { block: u64 },
    /// Static header invalid (bad magic / version / geometry).
    BadHeader,
    /// Volume uses a feature this reader cannot honor (doc 10 §2).
    Unsupported,
    /// Mounted read-only (unknown ro_compat feature bit set).
    ReadOnly,
    /// Neither superblock slot holds a valid superblock (doc 09 §6).
    NoSuperblock,
    /// No free block satisfies SPACE-1.
    NoSpace,
    NotFound,
    Exists,
    NotDir,
    IsDir,
    NotEmpty,
    /// Operation not permitted (hard link to a directory).
    Perm,
    /// Generation counter reached `GEN_LIMIT`; the volume is permanently
    /// frozen read-only (doc 03 §3). Remedy: offline copy to a fresh volume.
    GenExhausted,
    /// Malformed argument (bad path, name too long, ...).
    Invalid,
    /// On-disk structural invariant violated (e.g. DIR-1 dirent chain).
    Corrupt,
}

pub type Result<T> = core::result::Result<T, Error>;
