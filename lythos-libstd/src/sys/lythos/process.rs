// PAL — process / task control.

use lythos_std::syscall::{SYS_TASK_EXIT, SYS_EXEC, syscall0, syscall4};
use lythos_std::error::SysError;

/// Terminate the current task immediately.  Never returns.
pub fn exit_task() -> ! {
    unsafe { syscall0(SYS_TASK_EXIT) };
    unreachable!()
}

/// Load and start a new task from a static ELF64 image.
///
/// `caps` is a slice of capability handle values to copy into the new task's
/// cap table (handles 0, 1, 2, … in order).
///
/// Returns the new `TaskId` (a `u64`) on success, or a `SysError`.
pub fn exec(elf: &[u8], caps: &[u64]) -> Result<u64, SysError> {
    let ret = unsafe {
        syscall4(
            SYS_EXEC,
            elf.as_ptr()  as u64,
            elf.len()     as u64,
            caps.as_ptr() as u64,
            caps.len()    as u64,
        )
    };
    SysError::from_raw(ret)
}
