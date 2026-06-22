//! Error code sentinels — transcribed from `docs/spec/syscalls.md` error table.
//!
//! All values are returned in RAX as two's-complement u64 (negative i64 reinterpreted).
//! Twelve codes are defined: seven generic capability/syscall codes (-1..-7) plus
//! five VFS-specific codes (-8..-12) returned by filesystem syscalls.

/// Unknown or unassigned syscall number.
pub const ENOSYS:  u64 = (-1i64) as u64;   // 0xFFFF_FFFF_FFFF_FFFF

/// Invalid or stale capability handle.
pub const ENOCAP:  u64 = (-2i64) as u64;   // 0xFFFF_FFFF_FFFF_FFFE

/// Capability rights insufficient for the requested operation.
pub const ENOPERM: u64 = (-3i64) as u64;   // 0xFFFF_FFFF_FFFF_FFFD

/// Invalid argument (bad task ID, self-grant, misaligned pointer, etc.).
pub const EINVAL:  u64 = (-4i64) as u64;   // 0xFFFF_FFFF_FFFF_FFFC

/// No such file or directory.
pub const ENOENT:  u64 = (-5i64) as u64;   // 0xFFFF_FFFF_FFFF_FFFB

/// Bad file descriptor.
pub const EBADF:   u64 = (-6i64) as u64;   // 0xFFFF_FFFF_FFFF_FFFA

/// Resource temporarily unavailable (non-blocking op on empty/full resource,
/// or IPC timeout expired).
pub const EAGAIN:  u64 = (-7i64) as u64;   // 0xFFFF_FFFF_FFFF_FFF9

/// Path component is not a directory (VFS: SYS_CREATE, SYS_MKDIR, SYS_RENAME).
pub const ENOTDIR: u64 = (-8i64) as u64;   // 0xFFFF_FFFF_FFFF_FFF8

/// Filesystem not mounted; VirtIO block device absent at VFS call time.
/// Returned by all VFS syscalls (SYS_OPEN, SYS_READ, SYS_WRITE, SYS_CREATE,
/// SYS_MKDIR, SYS_UNLINK, SYS_RENAME, SYS_SEEK) when the RFS is not mounted.
pub const ENOMNT:  u64 = (-9i64) as u64;   // 0xFFFF_FFFF_FFFF_FFF7

/// Too many open file descriptors (VFS: SYS_OPEN, SYS_CREATE).
pub const EMFILE:  u64 = (-10i64) as u64;  // 0xFFFF_FFFF_FFFF_FFF6

/// File or directory already exists (VFS: SYS_CREATE, SYS_MKDIR, SYS_RENAME).
pub const EEXIST:  u64 = (-11i64) as u64;  // 0xFFFF_FFFF_FFFF_FFF5

/// No space left on device (VFS: SYS_CREATE, SYS_MKDIR).
pub const ENOSPC:  u64 = (-12i64) as u64;  // 0xFFFF_FFFF_FFFF_FFF4

/// The lowest (most negative as i64) error sentinel value.
/// Any return value ≥ ERR_MIN is an error.
pub const ERR_MIN: u64 = ENOSPC;

/// Return `true` if `v` is an error sentinel.
#[inline(always)]
pub const fn is_err(v: u64) -> bool {
    v >= ERR_MIN
}
