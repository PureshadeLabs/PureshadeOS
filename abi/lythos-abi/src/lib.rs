//! lythos-abi — kernel/userspace ABI contract for Lythos.
//!
//! This crate is the single source of truth for:
//! - Syscall numbers and SYSCALL_MAX
//! - Error code sentinels (errno)
//! - Capability rights bits and kinds
//! - Boundary struct layouts (TaskInfo, PsEntry, Stat, DirEntry, BootInfo)
//! - IPC ring constants
//!
//! **No logic.** Pure numbers, types, and repr declarations.
//! Derived from `docs/spec/{syscalls,capabilities,ipc}.md`.

#![no_std]

pub mod syscall;
pub mod errno;
pub mod cap;
pub mod structs;
pub mod ipc;
