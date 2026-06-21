//! Task management — spawn, yield, and exit for lythos userspace processes.

/// Opaque identifier for a lythos task (each task = one process).
pub type TaskId = u64;

/// Liveness state of a task as reported by `SYS_TASK_STATUS`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskStatus {
    /// Task is running or ready to run.
    Running,
    /// Task is blocked waiting on an IPC endpoint.
    Blocked,
    /// Task has exited or was never created.
    Dead,
}

/// Query the kernel for the liveness of `task_id`.
///
/// Returns `TaskStatus::Dead` for any unknown task ID.
pub fn task_status(task_id: TaskId) -> TaskStatus {
    match crate::sys_task_status(task_id) {
        1 => TaskStatus::Running,
        2 => TaskStatus::Blocked,
        _ => TaskStatus::Dead,
    }
}

/// Spawn a new userspace process from a static ELF64 binary blob.
///
/// `caps` is the ordered list of capability handles (from the calling task's
/// table) to inherit into the new task as handles 0, 1, 2, ...
///
/// Returns the `TaskId` of the new task.
pub fn spawn(elf: &[u8], caps: &[u64]) -> crate::io::Result<TaskId> {
    crate::sys_exec(elf, caps).map_err(crate::io::Error::from_kernel)
}

/// Cooperatively yield the current task's CPU time slice to the scheduler.
///
/// The current task remains `Ready` and will be rescheduled after other tasks
/// have had a chance to run.
#[inline]
pub fn yield_now() {
    crate::sys_yield();
}

/// Terminate the current task immediately. Never returns.
pub fn exit() -> ! {
    crate::sys_task_exit()
}
