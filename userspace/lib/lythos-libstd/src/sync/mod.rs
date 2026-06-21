//! Synchronization primitives for Lythos.
//!
//! Lythos has no futex-style blocking primitive, so `Mutex` and `RwLock` are
//! implemented as spinlocks using `core::sync::atomic`.  `Condvar` is
//! implemented as a spin-wait (cooperative yields between checks).
//!
//! For production workloads where contention is expected, prefer
//! `lythos_rt::ipc::Endpoint` for coarse-grained producer-consumer patterns.

mod mutex;
mod rwlock;
mod once;
mod condvar;

pub use mutex::{Mutex, MutexGuard};
pub use rwlock::{RwLock, RwLockReadGuard, RwLockWriteGuard};
pub use once::{OnceLock, Once, OnceState};
pub use condvar::Condvar;

// Re-export Arc / Weak from alloc.
pub use _alloc::sync::{Arc, Weak};
pub use core::sync::atomic;
