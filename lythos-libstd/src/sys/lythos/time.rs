// PAL — timekeeping via SYS_TIME.
//
// SYS_TIME returns the number of nanoseconds elapsed since kernel boot.

use lythos_std::syscall::{SYS_TIME, syscall0};

/// Read the current kernel timestamp in nanoseconds (monotonic).
#[inline]
pub fn read_nanos() -> u64 {
    unsafe { syscall0(SYS_TIME) }
}
