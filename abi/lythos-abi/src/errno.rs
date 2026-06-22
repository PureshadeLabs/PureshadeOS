//! Error code sentinels — transcribed from `docs/spec/syscalls.md` error table.
//!
//! All values are returned in RAX as two's-complement u64 (negative i64 reinterpreted).
//! The spec documents exactly seven codes; they are listed in the table at the top
//! of syscalls.md.
//!
//! ## Note on rfs-internal codes (FINDING F3)
//!
//! Five additional error codes exist in `kernel/src/rfs.rs` as `i64` constants:
//!   ENOTDIR = -8, ENOMNT = -9, EMFILE = -10, EEXIST = -11, ENOSPC = -12
//!
//! These are returned by VFS syscalls (SYS_OPEN, SYS_CREATE, SYS_MKDIR, etc.)
//! but are absent from the spec error table and from all current userspace code.
//! They are NOT included here pending spec adjudication.

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

/// The lowest (most negative as i64) error sentinel value.
/// Any return value ≥ ERR_MIN is an error.
pub const ERR_MIN: u64 = EAGAIN;

/// Return `true` if `v` is an error sentinel.
#[inline(always)]
pub const fn is_err(v: u64) -> bool {
    v >= ERR_MIN
}
