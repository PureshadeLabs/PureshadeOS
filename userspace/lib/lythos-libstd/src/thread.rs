//! Thread-like task management for Lythos.
//!
//! Lythos tasks are heavyweight ELF processes launched via `SYS_EXEC`.
//! True closure-based thread spawning (as in `std::thread::spawn`) is not
//! possible without a shared-memory runtime or dynamic linking; this module
//! provides the pieces that *are* available:
//!
//! - `yield_now()` — cooperative yield
//! - `sleep()` — spin-sleep via the time PAL
//! - `spawn_task()` — launch a static ELF binary as a new task
//! - `ThreadId` — opaque task identifier
//!
//! A full `Builder::spawn` with closures would require either a dynamic
//! linker or a custom async executor — that belongs in a future
//! `lythos-async` crate.

use crate::time::Duration;
use crate::sys::{thread_impl, process_impl};
use lythos_rt::SysError;

/// Opaque identifier for a Lythos task, equivalent to `std::thread::ThreadId`.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct ThreadId(pub u64);

impl core::fmt::Display for ThreadId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "ThreadId({})", self.0)
    }
}

/// Yield the calling task's CPU quantum to the scheduler.
///
/// Equivalent to `std::thread::yield_now`.
pub fn yield_now() {
    thread_impl::yield_now();
}

/// Sleep for at least `dur` by spin-yielding.
///
/// Lythos has no sleep syscall; this busy-waits with `SYS_YIELD` between
/// time checks.  Only suitable for short durations or low-priority tasks.
pub fn sleep(dur: Duration) {
    thread_impl::spin_sleep_nanos(dur.as_nanos() as u64);
}

/// Spawn a new Lythos task from a static ELF image.
///
/// `caps` lists capability handles (from the caller's cap table) to copy into
/// the new task's table as handles 0, 1, 2, … in order.
///
/// Returns the `ThreadId` (= kernel `TaskId`) of the new task.
pub fn spawn_task(elf: &[u8], caps: &[u64]) -> Result<ThreadId, SysError> {
    process_impl::exec(elf, caps).map(ThreadId)
}

/// Builder for configuring task spawns (mirrors `std::thread::Builder`).
pub struct Builder {
    name:     Option<_alloc::string::String>,
    stack_sz: Option<usize>,
}

impl Builder {
    pub fn new() -> Self { Builder { name: None, stack_sz: None } }

    pub fn name(mut self, name: _alloc::string::String) -> Self {
        self.name = Some(name);
        self
    }

    pub fn stack_size(mut self, sz: usize) -> Self {
        self.stack_sz = Some(sz);
        self
    }

    /// Spawn a static ELF image as a task.
    ///
    /// Closure-based spawn requires a future dynamic-linking layer;
    /// call `spawn_task` directly until then.
    pub fn spawn_elf(self, elf: &[u8], caps: &[u64]) -> Result<ThreadId, SysError> {
        spawn_task(elf, caps)
    }
}

impl Default for Builder {
    fn default() -> Self { Self::new() }
}
