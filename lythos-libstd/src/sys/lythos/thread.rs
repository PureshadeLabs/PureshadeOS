// PAL — threading / cooperative scheduling.
//
// Lythos tasks are heavyweight (each is a separate ELF process).  True
// thread-level parallelism requires spawning via SYS_EXEC.  Within a single
// task, cooperative yielding is the only primitive.

use lythos_std::syscall::{SYS_YIELD, syscall0};

/// Yield the CPU to another runnable task.
pub fn yield_now() {
    unsafe { syscall0(SYS_YIELD); }
}

/// Spin-yield for approximately `nanos` nanoseconds.
///
/// The Lythos kernel provides no sleep syscall; we busy-wait by repeatedly
/// checking the time and yielding.  This is only suitable for short waits.
pub fn spin_sleep_nanos(nanos: u64) {
    let start = super::time::read_nanos();
    loop {
        let now = super::time::read_nanos();
        if now.saturating_sub(start) >= nanos {
            break;
        }
        yield_now();
    }
}
