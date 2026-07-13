//! Environment inspection for Lythos.
//!
//! Lythos has no concept of process environment variables or a working
//! directory.  This module exposes what *is* available: the kernel boot-info
//! record received on handle 2 at task startup.

use lythos_rt::BootInfo;
use _alloc::string::String;
use _alloc::vec::Vec;

/// Parse a 64-byte IPC frame as a `BootInfo`.
///
/// Call this immediately after `sys_ipc_recv(2, ...)` to consume the boot-info
/// message that the kernel pre-queued on handle 2 before `lythd` started.
pub fn parse_boot_info(frame: &[u8; 64]) -> Option<BootInfo> {
    BootInfo::from_bytes(frame)
}

// ── Stub std::env API ─────────────────────────────────────────────────────────

/// Returns an empty iterator — Lythos has no environment variables.
pub fn vars() -> Vars { Vars(core::iter::empty()) }

pub struct Vars(core::iter::Empty<(String, String)>);

impl Iterator for Vars {
    type Item = (String, String);
    fn next(&mut self) -> Option<Self::Item> { None }
}

/// Always returns `None` — Lythos has no environment variables.
pub fn var(_key: &str) -> Option<String> { None }

/// Returns `"lythos"` — the platform identifier.
pub fn consts() -> &'static str { "lythos" }

/// The task's command-line arguments, `argv[0]` first.
///
/// Backed by the `SYS_EXEC` initial stack frame via `lythos_rt::args`.
/// Non-empty only when the binary uses `lythos_rt::entry!` (which captures
/// the frame) and the spawner passed argv (`sys_exec_argv`).
pub fn args() -> Args { Args { inner: lythos_rt::args::args() } }

pub struct Args { inner: lythos_rt::args::Args }

impl Iterator for Args {
    type Item = String;
    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next().map(String::from)
    }
}

impl ExactSizeIterator for Args {
    fn len(&self) -> usize { self.inner.len() }
}
