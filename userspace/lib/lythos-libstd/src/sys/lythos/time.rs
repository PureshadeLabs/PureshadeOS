// PAL — timekeeping via SYS_TIME.
//
// SYS_TIME returns milliseconds elapsed since kernel boot (not nanoseconds).

/// Read the current kernel timestamp in milliseconds (monotonic).
#[inline]
pub fn read_millis() -> u64 {
    lythos_rt::sys_time()
}

/// Read the current kernel timestamp in nanoseconds (monotonic).
///
/// Converts from the kernel's millisecond resolution by multiplying by 1_000_000.
/// Sub-millisecond precision is not available.
#[inline]
pub fn read_nanos() -> u64 {
    lythos_rt::sys_time().saturating_mul(1_000_000)
}
