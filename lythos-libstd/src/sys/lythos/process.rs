// PAL — process / task control.

use lythos_std::SysError;

/// Terminate the current task immediately.  Never returns.
pub fn exit_task() -> ! {
    lythos_std::sys_task_exit()
}

/// Load and start a new task from a static ELF64 image.
pub fn exec(elf: &[u8], caps: &[u64]) -> Result<u64, SysError> {
    lythos_std::sys_exec(elf, caps)
}
