//! Time types for cask userspace.
//!
//! `Duration` is fully implemented with arithmetic and conversions.
//! `Instant` wraps `SYS_TIME`, which returns milliseconds since kernel boot.

use core::{
    fmt,
    ops::{Add, AddAssign, Div, Mul, Sub, SubAssign},
};

// ── Constants ─────────────────────────────────────────────────────────────────

const NANOS_PER_SEC:   u32 = 1_000_000_000;
const NANOS_PER_MILLI: u32 = 1_000_000;
const NANOS_PER_MICRO: u32 = 1_000;
const MILLIS_PER_SEC:  u64 = 1_000;
const MICROS_PER_SEC:  u64 = 1_000_000;

// ── Duration ──────────────────────────────────────────────────────────────────

/// A span of time with nanosecond precision.
///
/// Mirrors `std::time::Duration` exactly.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Duration {
    secs:  u64,
    nanos: u32,   // always < NANOS_PER_SEC
}

impl Duration {
    // ── Constructors ──────────────────────────────────────────────────────────

    pub const ZERO:        Duration = Duration { secs: 0, nanos: 0 };
    pub const MAX:         Duration = Duration { secs: u64::MAX, nanos: NANOS_PER_SEC - 1 };
    pub const SECOND:      Duration = Duration::from_secs(1);
    pub const MILLISECOND: Duration = Duration::from_millis(1);
    pub const MICROSECOND: Duration = Duration::from_micros(1);
    pub const NANOSECOND:  Duration = Duration::from_nanos(1);

    pub const fn new(secs: u64, nanos: u32) -> Self {
        let extra = nanos / NANOS_PER_SEC;
        Duration {
            secs:  secs + extra as u64,
            nanos: nanos % NANOS_PER_SEC,
        }
    }

    pub const fn from_secs(secs: u64) -> Self {
        Duration { secs, nanos: 0 }
    }

    pub const fn from_millis(ms: u64) -> Self {
        Duration {
            secs:  ms / MILLIS_PER_SEC,
            nanos: ((ms % MILLIS_PER_SEC) as u32) * NANOS_PER_MILLI,
        }
    }

    pub const fn from_micros(us: u64) -> Self {
        Duration {
            secs:  us / MICROS_PER_SEC,
            nanos: ((us % MICROS_PER_SEC) as u32) * NANOS_PER_MICRO,
        }
    }

    pub const fn from_nanos(ns: u64) -> Self {
        Duration {
            secs:  ns / NANOS_PER_SEC as u64,
            nanos: (ns % NANOS_PER_SEC as u64) as u32,
        }
    }

    pub fn from_secs_f64(secs: f64) -> Self {
        let s = secs as u64;
        let n = ((secs - s as f64) * NANOS_PER_SEC as f64) as u32;
        Duration { secs: s, nanos: n.min(NANOS_PER_SEC - 1) }
    }

    pub fn from_secs_f32(secs: f32) -> Self {
        Self::from_secs_f64(secs as f64)
    }

    // ── Accessors ─────────────────────────────────────────────────────────────

    pub const fn as_secs(&self)         -> u64  { self.secs }
    pub const fn subsec_millis(&self)   -> u32  { self.nanos / NANOS_PER_MILLI }
    pub const fn subsec_micros(&self)   -> u32  { self.nanos / NANOS_PER_MICRO }
    pub const fn subsec_nanos(&self)    -> u32  { self.nanos }
    pub const fn is_zero(&self)         -> bool { self.secs == 0 && self.nanos == 0 }

    pub const fn as_millis(&self) -> u128 {
        self.secs as u128 * MILLIS_PER_SEC as u128
            + self.nanos as u128 / NANOS_PER_MILLI as u128
    }

    pub const fn as_micros(&self) -> u128 {
        self.secs as u128 * MICROS_PER_SEC as u128
            + self.nanos as u128 / NANOS_PER_MICRO as u128
    }

    pub const fn as_nanos(&self) -> u128 {
        self.secs as u128 * NANOS_PER_SEC as u128 + self.nanos as u128
    }

    pub fn as_secs_f64(&self) -> f64 {
        self.secs as f64 + self.nanos as f64 / NANOS_PER_SEC as f64
    }

    pub fn as_secs_f32(&self) -> f32 {
        self.secs as f32 + self.nanos as f32 / NANOS_PER_SEC as f32
    }

    // ── Arithmetic ────────────────────────────────────────────────────────────

    pub fn checked_add(self, rhs: Duration) -> Option<Duration> {
        let nanos = self.nanos + rhs.nanos;
        let carry = (nanos / NANOS_PER_SEC) as u64;
        let nanos = nanos % NANOS_PER_SEC;
        let secs  = self.secs.checked_add(rhs.secs)?.checked_add(carry)?;
        Some(Duration { secs, nanos })
    }

    pub fn checked_sub(self, rhs: Duration) -> Option<Duration> {
        let secs = self.secs.checked_sub(rhs.secs)?;
        if self.nanos >= rhs.nanos {
            Some(Duration { secs, nanos: self.nanos - rhs.nanos })
        } else {
            let secs  = secs.checked_sub(1)?;
            let nanos = NANOS_PER_SEC - rhs.nanos + self.nanos;
            Some(Duration { secs, nanos })
        }
    }

    pub fn checked_mul(self, rhs: u32) -> Option<Duration> {
        let total_nanos = self.nanos as u64 * rhs as u64;
        let carry = total_nanos / NANOS_PER_SEC as u64;
        let nanos = (total_nanos % NANOS_PER_SEC as u64) as u32;
        let secs  = self.secs.checked_mul(rhs as u64)?.checked_add(carry)?;
        Some(Duration { secs, nanos })
    }

    pub fn checked_div(self, rhs: u32) -> Option<Duration> {
        if rhs == 0 { return None; }
        let total = self.secs as u128 * NANOS_PER_SEC as u128 + self.nanos as u128;
        let result = total / rhs as u128;
        Some(Duration::from_nanos(result as u64))
    }

    pub fn saturating_add(self, rhs: Duration) -> Duration {
        self.checked_add(rhs).unwrap_or(Duration::MAX)
    }

    pub fn saturating_sub(self, rhs: Duration) -> Duration {
        self.checked_sub(rhs).unwrap_or(Duration::ZERO)
    }

    pub fn saturating_mul(self, rhs: u32) -> Duration {
        self.checked_mul(rhs).unwrap_or(Duration::MAX)
    }

    pub fn mul_f64(self, f: f64) -> Duration {
        Duration::from_secs_f64(self.as_secs_f64() * f)
    }

    pub fn mul_f32(self, f: f32) -> Duration {
        Duration::from_secs_f32(self.as_secs_f32() * f)
    }

    pub fn div_f64(self, f: f64) -> Duration {
        Duration::from_secs_f64(self.as_secs_f64() / f)
    }

    pub fn div_f32(self, f: f32) -> Duration {
        Duration::from_secs_f32(self.as_secs_f32() / f)
    }

    pub fn div_duration_f64(self, rhs: Duration) -> f64 {
        self.as_secs_f64() / rhs.as_secs_f64()
    }
}

// ── Operator impls ────────────────────────────────────────────────────────────

impl Add for Duration {
    type Output = Duration;
    fn add(self, rhs: Duration) -> Duration {
        self.checked_add(rhs).expect("overflow when adding durations")
    }
}

impl AddAssign for Duration {
    fn add_assign(&mut self, rhs: Duration) { *self = *self + rhs; }
}

impl Sub for Duration {
    type Output = Duration;
    fn sub(self, rhs: Duration) -> Duration {
        self.checked_sub(rhs).expect("overflow when subtracting durations")
    }
}

impl SubAssign for Duration {
    fn sub_assign(&mut self, rhs: Duration) { *self = *self - rhs; }
}

impl Mul<u32> for Duration {
    type Output = Duration;
    fn mul(self, rhs: u32) -> Duration {
        self.checked_mul(rhs).expect("overflow when multiplying duration")
    }
}

impl Mul<Duration> for u32 {
    type Output = Duration;
    fn mul(self, rhs: Duration) -> Duration { rhs * self }
}

impl Div<u32> for Duration {
    type Output = Duration;
    fn div(self, rhs: u32) -> Duration {
        self.checked_div(rhs).expect("divide by zero")
    }
}

// ── Formatting ────────────────────────────────────────────────────────────────

impl fmt::Debug for Duration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{:09}s", self.secs, self.nanos)
    }
}

impl fmt::Display for Duration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.secs > 0 {
            write!(f, "{}.{:03}s", self.secs, self.nanos / NANOS_PER_MILLI)
        } else if self.nanos >= NANOS_PER_MILLI {
            write!(f, "{}.{:03}ms", self.nanos / NANOS_PER_MILLI,
                   (self.nanos % NANOS_PER_MILLI) / NANOS_PER_MICRO)
        } else if self.nanos >= NANOS_PER_MICRO {
            write!(f, "{}µs", self.nanos / NANOS_PER_MICRO)
        } else {
            write!(f, "{}ns", self.nanos)
        }
    }
}

// ── Instant ───────────────────────────────────────────────────────────────────

/// A measurement of a monotonically non-decreasing clock.
///
/// Backed by `SYS_TIME`, which returns the number of milliseconds elapsed
/// since kernel boot (APIC tick counter, ~1 ms resolution).
///
/// Instants are always greater than or equal to any prior `Instant::now()` on
/// the same boot.  They are not wall-clock time; they reset on reboot.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct Instant {
    /// Milliseconds since kernel boot at the moment this `Instant` was created.
    millis: u64,
}

impl Instant {
    /// Capture the current time.
    #[inline]
    pub fn now() -> Self {
        Instant { millis: crate::sys_time() }
    }

    /// Return the `Duration` elapsed since `earlier`.
    ///
    /// Saturates to `Duration::ZERO` if `self` is earlier than `earlier`
    /// (e.g. due to clock skew or incorrect argument order).
    #[inline]
    pub fn duration_since(&self, earlier: Instant) -> Duration {
        Duration::from_millis(self.millis.saturating_sub(earlier.millis))
    }

    /// Return `Some(duration)` if `self >= earlier`, else `None`.
    #[inline]
    pub fn checked_duration_since(&self, earlier: Instant) -> Option<Duration> {
        self.millis.checked_sub(earlier.millis).map(Duration::from_millis)
    }

    /// Return the `Duration` elapsed since `earlier`, saturating to zero.
    #[inline]
    pub fn saturating_duration_since(&self, earlier: Instant) -> Duration {
        self.duration_since(earlier)
    }

    /// Return the `Duration` elapsed since this `Instant` was created.
    #[inline]
    pub fn elapsed(&self) -> Duration {
        Instant::now().duration_since(*self)
    }

    /// Raw millisecond count since boot.  Prefer `duration_since` / `elapsed`.
    #[inline]
    pub fn as_millis_since_boot(&self) -> u64 {
        self.millis
    }
}
