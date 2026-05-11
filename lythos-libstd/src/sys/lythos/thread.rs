// PAL — threading / cooperative scheduling.

/// Yield the CPU to another runnable task.
pub fn yield_now() {
    lythos_std::sys_yield();
}

/// Spin-yield for approximately `nanos` nanoseconds.
pub fn spin_sleep_nanos(nanos: u64) {
    // sys_time() returns milliseconds; convert threshold to ms.
    let threshold_ms = (nanos / 1_000_000).max(1);
    let start_ms = super::time::read_millis();
    loop {
        let now_ms = super::time::read_millis();
        if now_ms.saturating_sub(start_ms) >= threshold_ms {
            break;
        }
        yield_now();
    }
}
