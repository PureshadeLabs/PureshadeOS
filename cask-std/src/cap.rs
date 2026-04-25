//! Capability handles and rights for cask userspace.
//!
//! Capabilities are opaque handles into the kernel's capability table for the
//! current task.  Pass a `CapHandle` to `ipc::Endpoint` constructors, task
//! spawning, etc.

use crate::io;

// ── Rights ────────────────────────────────────────────────────────────────────

/// Bitmask of rights associated with a capability.
///
/// `Rights::ALL` grants all four operations.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rights(pub u8);

impl Rights {
    pub const READ:   Rights = Rights(crate::cap_rights::READ);
    pub const WRITE:  Rights = Rights(crate::cap_rights::WRITE);
    pub const GRANT:  Rights = Rights(crate::cap_rights::GRANT);
    pub const REVOKE: Rights = Rights(crate::cap_rights::REVOKE);
    pub const ALL:    Rights = Rights(crate::cap_rights::ALL);
    pub const NONE:   Rights = Rights(0);

    /// Return `true` if `self` includes all bits set in `other`.
    pub const fn has(self, other: Rights) -> bool {
        self.0 & other.0 == other.0
    }

    /// Union (bitwise OR) of two rights sets.
    pub const fn union(self, other: Rights) -> Rights {
        Rights(self.0 | other.0)
    }

    /// Intersection (bitwise AND) of two rights sets.
    pub const fn intersect(self, other: Rights) -> Rights {
        Rights(self.0 & other.0)
    }

    /// Return the raw byte value.
    pub const fn as_u8(self) -> u8 { self.0 }
}

impl core::fmt::Debug for Rights {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let mut parts = f.debug_set();
        if self.has(Rights::READ)   { parts.entry(&"READ");   }
        if self.has(Rights::WRITE)  { parts.entry(&"WRITE");  }
        if self.has(Rights::GRANT)  { parts.entry(&"GRANT");  }
        if self.has(Rights::REVOKE) { parts.entry(&"REVOKE"); }
        parts.finish()
    }
}

// ── CapHandle ─────────────────────────────────────────────────────────────────

/// An index into the current task's capability table.
///
/// Handle values are assigned by the kernel on creation or derivation.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct CapHandle(pub u64);

impl CapHandle {
    /// The raw handle index.
    pub const fn as_raw(self) -> u64 { self.0 }

    /// Grant a derived capability with `rights` to task `target`.
    ///
    /// Returns the handle index the recipient received.
    /// Requires `Rights::GRANT` on `self`.
    pub fn grant(self, target: crate::task::TaskId, rights: Rights) -> io::Result<CapHandle> {
        crate::sys_cap_grant(self.0, target, rights.0)
            .map(CapHandle)
            .map_err(io::Error::from_kernel)
    }

    /// Cascade-revoke this capability (removes from the caller's table and all
    /// derived holders).  Requires `Rights::REVOKE`.
    pub fn revoke(self) -> io::Result<()> {
        crate::sys_cap_revoke(self.0).map_err(io::Error::from_kernel)
    }
}

impl core::fmt::Debug for CapHandle {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "CapHandle({})", self.0)
    }
}

impl core::fmt::Display for CapHandle {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}", self.0)
    }
}
