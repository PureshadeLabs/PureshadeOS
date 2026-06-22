//! lythos-syscall — raw `syscall`/`sysretq` stub wrappers for Lythos.
//!
//! Only functional when compiled for `x86_64`. On other architectures
//! (e.g. host-side tooling builds on Apple Silicon) the crate is empty.
//!
//! See `x86_64.rs` for the register convention and full wrapper list.

#![no_std]

/// Re-export syscall numbers from lythos-abi for convenience.
pub use lythos_abi::syscall as nr;

#[cfg(target_arch = "x86_64")]
mod x86_64;
#[cfg(target_arch = "x86_64")]
pub use x86_64::*;
