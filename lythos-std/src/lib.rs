//! lythos-std — native runtime for RaptorOS userspace.
//!
//! Provides thin safe wrappers around the lythos syscall ABI, a global
//! bump allocator backed by `SYS_MMAP`, and the `BootInfo` / `Endpoint`
//! types used by the boot protocol.

#![no_std]
#![feature(alloc_error_handler)]

extern crate alloc;

pub mod boot;
pub mod error;
pub mod ipc;
pub mod io;
pub mod syscall;
pub mod task;
mod allocator; // registers #[global_allocator]; not part of public API

// ── Crate-root re-exports ─────────────────────────────────────────────────────

pub use boot::BootInfo;
pub use error::SysError;
pub use io::sys_log;
pub use task::{sys_rollback, sys_task_exit};

// ── Alloc error handler ───────────────────────────────────────────────────────

#[alloc_error_handler]
fn on_oom(_layout: core::alloc::Layout) -> ! {
    io::sys_log("[lythos-std] out of memory\n");
    task::sys_task_exit()
}
