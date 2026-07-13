// PAL — process / task control.

use lythos_rt::SysError;

/// Terminate the current task immediately.  Never returns.
pub fn exit_task() -> ! {
    lythos_rt::sys_task_exit()
}

/// Load and start a new task from a static ELF64 image.
pub fn exec(elf: &[u8], caps: &[u64]) -> Result<u64, SysError> {
    lythos_rt::sys_exec(elf, caps)
}

/// Like [`exec`], passing `argv` to the new task (readable there via
/// `env::args()` when the binary uses `lythos_rt::entry!`).
pub fn exec_argv(elf: &[u8], caps: &[u64], argv: &[&str]) -> Result<u64, SysError> {
    lythos_rt::sys_exec_argv(elf, caps, argv)
}
