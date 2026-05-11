//! Lythos-specific extensions to `std`.
//!
//! Re-exports the Lythos capability system and IPC primitives from `lythos-std`
//! at a `std::os::lythos` path for ergonomic use by OS-aware code.

// Re-export key types so callers don't need to depend on lythos-std directly.
pub use lythos_std::cap::{CapHandle, Rights};
pub use lythos_std::ipc::{Endpoint, Channel, Message, MSG_SIZE};
pub use lythos_std::SysError;

// Boot info — BootInfo is at the lythos_std crate root (no boot submodule).
pub use lythos_std::BootInfo;
pub use crate::env::parse_boot_info;

/// Raw page-flag constants for `SYS_MMAP`.
pub mod page_flags {
    pub const PRESENT:  u64 = 1 << 0;
    pub const WRITABLE: u64 = 1 << 1;
    pub const USER:     u64 = 1 << 2;
    pub const NX:       u64 = 1 << 63;

    pub const USER_RX:  u64 = PRESENT | USER;
    pub const USER_RW:  u64 = PRESENT | WRITABLE | USER | NX;
}

/// Well-known capability handles assigned to `lythd` at boot.
pub mod boot_caps {
    pub const MEM_CAP:       u64 = 0;
    pub const ROLLBACK_CAP:  u64 = 1;
    pub const BOOT_INFO_CAP: u64 = 2;
}

/// CapRights bit values.
pub mod rights {
    pub const READ:   u8 = 0x01;
    pub const WRITE:  u8 = 0x02;
    pub const GRANT:  u8 = 0x04;
    pub const REVOKE: u8 = 0x08;
    pub const ALL:    u8 = 0x0F;
}
