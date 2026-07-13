//! Capability types — transcribed from `docs/spec/capabilities.md`.
//!
//! ## ABI contract vs. kernel internals
//!
//! The rights bits ARE ABI (they are passed as register arguments in e.g.
//! `SYS_CAP_GRANT a3`). The `CapRights` Rust struct in `kernel/src/cap.rs`
//! is a kernel implementation detail and not reproduced here; only the bit
//! values are defined.
//!
//! `CapHandle` is an opaque `u64`. Userspace must not parse or forge it.
//! The internal generation/slot encoding is not part of the ABI.
//!
//! `CapKind` is included for documentation only. Kind values are never passed
//! in register arguments; they are kernel-internal metadata associated with a
//! capability at creation time.
//!
//! Cross-check against `kernel/src/cap.rs`:
//! - CapRights bits: READ=1, WRITE=2, GRANT=4, REVOKE=8, ALL=15 — match ✓
//! - CapKind variants: Memory, Ipc, Device, Rollback, Filesystem — match ✓
//! - CapHandle inner type: u64 — match ✓

// ── Rights bits ───────────────────────────────────────────────────────────────
//
// Passed as the low bits of a u64 register argument (e.g. a3 in SYS_CAP_GRANT).
// Valid range 0–15 (4 bits used).

/// Can read from / receive from the resource.
pub const RIGHT_READ:   u8 = 1 << 0;   // 1
/// Can write to / send to the resource.
pub const RIGHT_WRITE:  u8 = 1 << 1;   // 2
/// Can grant a derived cap with reduced rights to another task.
pub const RIGHT_GRANT:  u8 = 1 << 2;   // 4
/// Can revoke this cap and all derived copies.
pub const RIGHT_REVOKE: u8 = 1 << 3;   // 8
/// All four rights combined.
pub const RIGHT_ALL:    u8 = 0x0F;     // 15
/// No rights; handle is effectively unusable.
pub const RIGHT_NONE:   u8 = 0x00;     // 0

// ── CapKind ───────────────────────────────────────────────────────────────────

/// The kind of resource a capability refers to.
///
/// Values are never passed in register arguments; documented here to make the
/// boot-cap table (handles 0/1/2) self-describing.
///
/// Variants match `kernel/src/cap.rs::CapKind` exactly.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CapKind {
    /// Contiguous physical frame range.  Grants `SYS_MMAP` access.
    Memory,
    /// One IPC endpoint.  Grants `SYS_IPC_SEND`/`SYS_IPC_RECV` and variants.
    Ipc,
    /// Hardware device (IRQ line, port range, MMIO region).
    Device,
    /// Privileged kernel-rollback trigger.  Granted only to `lythd` at boot.
    Rollback,
    /// Filesystem mount authority.  Grants `SYS_MOUNT` (WRITE right required).
    /// Granted only to `lythd` at boot; delegable via `SYS_CAP_GRANT`.
    Filesystem,
}

// ── CapHandle ─────────────────────────────────────────────────────────────────

/// An opaque handle into the current task's capability table.
///
/// Assigned by the kernel on capability creation or derivation.
/// Valid operations:
/// - Pass back to a syscall.
/// - Compare two handles for equality.
/// - Test `handle.0 == u64::MAX` — the sentinel for "no capability" in
///   `SYS_IPC_RECV_CAP`.
///
/// Do NOT parse, forge, or arithmetically manipulate the raw value.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[repr(transparent)]
pub struct CapHandle(pub u64);

impl CapHandle {
    /// Sentinel returned by `SYS_IPC_RECV_CAP` when the message carried no cap.
    pub const NONE: Self = Self(u64::MAX);

    /// The raw u64 value (for passing to syscall register arguments).
    #[inline(always)]
    pub const fn as_raw(self) -> u64 { self.0 }

    /// True if this handle is the no-capability sentinel.
    #[inline(always)]
    pub const fn is_none(self) -> bool { self.0 == u64::MAX }
}

// ── Boot capability handles ───────────────────────────────────────────────────
//
// Per capabilities.md § "Initial capabilities at boot".
// The kernel pre-creates these in lythd's capability table.

/// Boot handle 0: Memory capability with ALL rights over the full PMM.
pub const BOOT_CAP_MEMORY:     CapHandle = CapHandle(0);
/// Boot handle 1: Rollback capability with ALL rights.
pub const BOOT_CAP_ROLLBACK:   CapHandle = CapHandle(1);
/// Boot handle 2: IPC endpoint (BootInfo pre-queued) with ALL rights.
pub const BOOT_CAP_IPC:        CapHandle = CapHandle(2);
/// Boot handle 3: Filesystem (mount authority) capability with ALL rights.
pub const BOOT_CAP_FILESYSTEM: CapHandle = CapHandle(3);
