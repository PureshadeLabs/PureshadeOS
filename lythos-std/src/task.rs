/// Task management — yield, exit, spawn.

use crate::error::SysError;
use crate::syscall::*;

/// Cooperatively yield the CPU to the next ready task.
#[inline]
pub fn yield_now() {
    unsafe { syscall0(SYS_YIELD); }
}

/// Terminate the calling task immediately.  Never returns.
pub fn sys_task_exit() -> ! {
    unsafe { syscall0(SYS_TASK_EXIT); }
    // Unreachable, but the compiler needs this to type-check `-> !`.
    loop { unsafe { core::arch::asm!("hlt", options(nostack, nomem)); } }
}

/// Trigger a system rollback (requires `CapKind::Rollback` at handle 1).
///
/// If the capability check passes the kernel halts and this never returns.
/// If the caller lacks the Rollback capability, returns `Err(ENOPERM)`.
pub fn sys_rollback() -> Result<(), SysError> {
    let r = unsafe { syscall0(SYS_ROLLBACK) };
    SysError::from_raw(r).map(|_| ())
}

/// Spawn a new process from a static ELF64 blob.
///
/// `caps` is a slice of capability handles from the caller's cap table; they
/// are inherited by the new process as handles 0, 1, 2, … in order.
///
/// Returns the new task's `TaskId` on success.
pub fn spawn(elf: &[u8], caps: &[u64]) -> Result<u64, SysError> {
    let r = unsafe {
        syscall4(
            SYS_EXEC,
            elf.as_ptr() as u64,
            elf.len() as u64,
            caps.as_ptr() as u64,
            caps.len() as u64,
        )
    };
    SysError::from_raw(r)
}
