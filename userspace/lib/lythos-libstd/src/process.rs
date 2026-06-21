//! Process control for Lythos.
//!
//! Provides `exit`, `abort`, and a `Command` builder for spawning ELF tasks.

use crate::sys::process_impl;
use lythos_rt::SysError;

/// Exit the current task with the given status code.
///
/// On Lythos, exit codes are not currently observable by the parent; this
/// function always calls `SYS_TASK_EXIT`.
pub fn exit(code: i32) -> ! {
    let _ = code;
    process_impl::exit_task()
}

/// Abort: exit immediately (equivalent to `exit(1)` on Lythos).
pub fn abort() -> ! {
    process_impl::exit_task()
}

/// Spawn a new Lythos task from a raw ELF image.
///
/// Returns the kernel `TaskId` of the new task, or a `SysError`.
pub fn spawn_elf(elf: &[u8], caps: &[u64]) -> Result<u64, SysError> {
    process_impl::exec(elf, caps)
}

// ── Command ───────────────────────────────────────────────────────────────────

/// A builder for spawning new Lythos tasks (partial `std::process::Command` mirror).
///
/// On Lythos there is no shell, no PATH search, and no argv/envp; a task is
/// identified by the raw bytes of its ELF image.  `Command` holds that image
/// and the set of capabilities to inherit.
pub struct Command {
    elf:  Option<_alloc::vec::Vec<u8>>,
    caps: _alloc::vec::Vec<u64>,
}

impl Command {
    /// Create a new `Command`.  The ELF image must be supplied via
    /// [`Command::elf`] before calling [`Command::spawn`].
    pub fn new() -> Self { Command { elf: None, caps: _alloc::vec::Vec::new() } }

    /// Set the ELF binary to execute.
    pub fn elf(mut self, image: _alloc::vec::Vec<u8>) -> Self {
        self.elf = Some(image);
        self
    }

    /// Add a capability handle to inherit.
    pub fn cap(mut self, handle: u64) -> Self {
        self.caps.push(handle);
        self
    }

    /// Spawn the task.  Returns the new `TaskId` or a `SysError`.
    pub fn spawn(self) -> Result<Child, SpawnError> {
        let elf = self.elf.ok_or(SpawnError::NoElf)?;
        let tid = process_impl::exec(&elf, &self.caps).map_err(SpawnError::Kernel)?;
        Ok(Child { task_id: tid })
    }
}

impl Default for Command {
    fn default() -> Self { Self::new() }
}

/// A running child task.
pub struct Child {
    pub task_id: u64,
}

impl Child {
    /// Poll whether the task has exited.  Not yet implemented — Lythos does
    /// not expose a wait/waitpid equivalent yet.
    pub fn try_wait(&mut self) -> Option<ExitStatus> {
        // TODO: use SYS_TASK_STATUS once kernel exposes it.
        None
    }
}

/// Exit status of a child task.
#[derive(Debug, Clone, Copy)]
pub struct ExitStatus(i32);

impl ExitStatus {
    pub fn success(&self) -> bool { self.0 == 0 }
    pub fn code(&self) -> Option<i32> { Some(self.0) }
}

/// Error returned by `Command::spawn`.
#[derive(Debug)]
pub enum SpawnError {
    NoElf,
    Kernel(SysError),
}

impl core::fmt::Display for SpawnError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            SpawnError::NoElf        => f.write_str("no ELF image provided"),
            SpawnError::Kernel(e)    => write!(f, "kernel error: {:?}", e),
        }
    }
}
