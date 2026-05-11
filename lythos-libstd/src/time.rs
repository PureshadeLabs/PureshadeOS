//! Time types for Lythos.
//!
//! `Duration` mirrors `core::time::Duration`.
//! `Instant` is backed by `SYS_TIME` (nanoseconds since kernel boot — monotonic).
//! `SystemTime` is an alias for `Instant` (no wall-clock yet).

pub use core::time::Duration;
use crate::sys::time_impl;

/// A monotonic timestamp backed by `SYS_TIME`.
///
/// The epoch is kernel boot.  Absolute values are not meaningful; only
/// differences between two `Instant`s are.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct Instant(u64); // nanos since boot

impl Instant {
    pub fn now() -> Self { Instant(time_impl::read_nanos()) }

    pub fn duration_since(&self, earlier: Instant) -> Duration {
        Duration::from_nanos(self.0.saturating_sub(earlier.0))
    }

    pub fn elapsed(&self) -> Duration {
        Instant::now().duration_since(*self)
    }

    pub fn checked_add(&self, duration: Duration) -> Option<Self> {
        self.0.checked_add(duration.as_nanos() as u64).map(Instant)
    }

    pub fn checked_sub(&self, duration: Duration) -> Option<Self> {
        self.0.checked_sub(duration.as_nanos() as u64).map(Instant)
    }
}

impl core::ops::Add<Duration> for Instant {
    type Output = Instant;
    fn add(self, d: Duration) -> Instant { Instant(self.0 + d.as_nanos() as u64) }
}

impl core::ops::Sub<Duration> for Instant {
    type Output = Instant;
    fn sub(self, d: Duration) -> Instant { Instant(self.0.saturating_sub(d.as_nanos() as u64)) }
}

impl core::ops::Sub<Instant> for Instant {
    type Output = Duration;
    fn sub(self, earlier: Instant) -> Duration { self.duration_since(earlier) }
}

/// On Lythos there is no wall-clock source yet; `SystemTime` is monotonic boot time.
pub type SystemTime = Instant;

/// The UNIX_EPOCH constant is a dummy on Lythos (no RTC support yet).
pub const UNIX_EPOCH: Instant = Instant(0);
