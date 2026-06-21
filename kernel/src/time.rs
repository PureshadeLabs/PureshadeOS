//! Wall-clock time — CMOS RTC anchored to Unix epoch milliseconds.
//!
//! ## Design
//!
//! The MC146818 CMOS RTC is read once during `init()` (called after
//! `apic::init()` so the monotonic `ticks()` counter is already live).
//! The reading is converted to Unix epoch milliseconds using Howard Hinnant's
//! days-from-civil algorithm, then anchored against `apic::ticks()`.
//! Subsequent `epoch_ms()` calls advance the time using the APIC counter.
//!
//! ## Century detection
//!
//! This kernel does not parse ACPI FADT, so the FADT century-register field
//! is unavailable.  CMOS register 0x32 is read instead.  If it yields a
//! plausible century (19, 20, or 21), it is used; otherwise 20 (year 20xx)
//! is assumed with a log message.  Known limitation: revisit if a real ACPI
//! parser is added.
//!
//! ## UTC assumption
//!
//! The RTC is assumed to hold UTC.  No timezone conversion is performed.
//! QEMU defaults to UTC; on real hardware the RTC must be configured for UTC
//! (standard on Linux dual-boot machines).

use core::sync::atomic::{AtomicU64, Ordering};

// ── Anchors ───────────────────────────────────────────────────────────────────

/// Unix epoch milliseconds at the moment `init()` ran.
static EPOCH_MS_ANCHOR: AtomicU64 = AtomicU64::new(0);

/// `apic::ticks()` value recorded at the same moment.
static MONO_ANCHOR: AtomicU64 = AtomicU64::new(0);

// ── CMOS I/O ─────────────────────────────────────────────────────────────────

/// Select CMOS register `reg` (with NMI disable via bit 7) and read its byte.
#[inline]
unsafe fn cmos_read(reg: u8) -> u8 {
    let val: u8;
    unsafe {
        core::arch::asm!(
            "out dx, al",
            in("dx") 0x70u16,
            in("al") reg | 0x80u8,       // bit 7 disables NMI during access
            options(nomem, nostack, preserves_flags),
        );
        core::arch::asm!(
            "in al, dx",
            out("al") val,
            in("dx") 0x71u16,
            options(nomem, nostack, preserves_flags),
        );
    }
    val
}

/// Spin until the Update-In-Progress flag (Status Reg A bit 7) clears.
/// The UIP window is ≤ 248 µs per the MC146818 datasheet.
#[inline]
unsafe fn wait_not_uip() {
    loop {
        if unsafe { cmos_read(0x0A) } & 0x80 == 0 { break; }
        core::hint::spin_loop();
    }
}

// ── Snapshot ─────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq)]
struct RtcSnapshot {
    sec:     u8,
    min:     u8,
    hour:    u8,  // raw (may contain PM bit in 12h mode)
    day:     u8,
    mon:     u8,
    year:    u8,  // two-digit year from CMOS 0x09
    century: u8,  // CMOS 0x32 (may be 0 or implausible on some boards)
}

unsafe fn take_snapshot() -> RtcSnapshot {
    unsafe {
        RtcSnapshot {
            sec:     cmos_read(0x00),
            min:     cmos_read(0x02),
            hour:    cmos_read(0x04),
            day:     cmos_read(0x07),
            mon:     cmos_read(0x08),
            year:    cmos_read(0x09),
            century: cmos_read(0x32),
        }
    }
}

// ── BCD helper ───────────────────────────────────────────────────────────────

#[inline]
fn bcd_to_bin(b: u8) -> u8 {
    (b >> 4) * 10 + (b & 0x0F)
}

// ── Calendar math ─────────────────────────────────────────────────────────────
//
// Howard Hinnant's days_from_civil — returns days since Unix epoch 1970-01-01
// for the proleptic Gregorian date (y, m, d).
// Reference: https://howardhinnant.github.io/date_algorithms.html
//
// Works correctly for any year where i64 arithmetic doesn't overflow.
// For the years [1970, 2199] the result fits comfortably in i64.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era  = y.div_euclid(400);
    let yoe  = (y - era * 400) as u64;                             // [0, 399]
    let madj = if m > 2 { m - 3 } else { m + 9 };                 // March=0 … Feb=11
    let doy  = ((153 * madj + 2) / 5 + d - 1) as u64;             // [0, 365]
    let doe  = yoe * 365 + yoe / 4 - yoe / 100 + doy;             // [0, 146096]
    era * 146_097 + doe as i64 - 719_468
}

// ── Initialisation ────────────────────────────────────────────────────────────

/// Read the CMOS RTC once and establish the epoch-millisecond anchor.
///
/// **Must be called after `apic::init()`** so that `apic::ticks()` is live.
/// Called once from `kmain` during early boot.
pub fn init() {
    let (sec, min, hour, day, mon, year_full) = unsafe {
        // UIP-safe double-read: wait until UIP=0, read all fields, wait again,
        // read again; accept only when the two readings are identical.
        // Retry up to 8 times; on persistent instability, use the last read.
        let snap = {
            let mut accepted: Option<RtcSnapshot> = None;
            for _ in 0..8u32 {
                wait_not_uip();
                let a = take_snapshot();
                wait_not_uip();
                let b = take_snapshot();
                if a == b { accepted = Some(a); break; }
            }
            match accepted {
                Some(s) => s,
                None => {
                    crate::kprintln!("[time] RTC: reads unstable after 8 retries; using last");
                    wait_not_uip();
                    take_snapshot()
                }
            }
        };

        // Status Register B controls encoding and hour format.
        let reg_b     = cmos_read(0x0B);
        let is_binary = reg_b & 0x04 != 0;  // bit 2: 0=BCD (default), 1=binary
        let is_24h    = reg_b & 0x02 != 0;  // bit 1: 0=12h, 1=24h (QEMU default)

        // Re-enable NMI now that all reads are done.
        core::arch::asm!(
            "out dx, al",
            in("dx") 0x70u16,
            in("al") 0u8,
            options(nomem, nostack, preserves_flags),
        );

        let cvt = |b: u8| -> u8 { if is_binary { b } else { bcd_to_bin(b) } };

        let sec = cvt(snap.sec) as u32;
        let min = cvt(snap.min) as u32;

        // Hours: in 12h mode the PM flag lives in bit 7 of the raw field and
        // must be extracted BEFORE BCD conversion.
        let hour = if is_24h {
            cvt(snap.hour) as u32
        } else {
            let is_pm = snap.hour & 0x80 != 0;
            let h12   = cvt(snap.hour & 0x7F) as u32;
            // 12 AM → 0, 12 PM → 12, others ±12 as expected.
            match (is_pm, h12) {
                (false, 12) =>  0,
                (false, h)  =>  h,
                (true,  12) => 12,
                (true,  h)  =>  h + 12,
            }
        };

        let day   = cvt(snap.day) as u32;
        let mon   = cvt(snap.mon) as u32;
        let year2 = cvt(snap.year) as u32;

        // Century: CMOS register 0x32.  Accept 19/20/21; log and assume 20 otherwise.
        let century_raw = cvt(snap.century);
        let century = if matches!(century_raw, 19 | 20 | 21) {
            century_raw as u32
        } else {
            crate::kprintln!(
                "[time] CMOS 0x32 = 0x{:02x} → {}; implausible century, assuming 20xx",
                snap.century, century_raw
            );
            20
        };

        (sec, min, hour, day, mon, century * 100 + year2)
    };

    // Epoch milliseconds = full days × 86 400 000 + intra-day milliseconds.
    let days     = days_from_civil(year_full as i64, mon as i64, day as i64);
    let intraday = (hour as u64 * 3_600 + min as u64 * 60 + sec as u64) * 1_000;
    let epoch_ms = (days as u64) * 86_400_000 + intraday;

    // Anchor against the current monotonic tick.
    let mono_now = crate::apic::ticks();
    EPOCH_MS_ANCHOR.store(epoch_ms, Ordering::Relaxed);
    MONO_ANCHOR.store(mono_now, Ordering::Relaxed);

    crate::kprintln!(
        "[time] RTC {}-{:02}-{:02} {:02}:{:02}:{:02} UTC  epoch_ms={}",
        year_full, mon, day, hour, min, sec, epoch_ms,
    );
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Current Unix time in milliseconds since 1970-01-01 00:00:00 UTC.
///
/// Derived from the RTC reading taken at `init()`; advances monotonically via
/// `apic::ticks()`.  Returns 0 if called before `init()`.
#[inline]
pub fn epoch_ms() -> u64 {
    let anchor = EPOCH_MS_ANCHOR.load(Ordering::Relaxed);
    let mono0  = MONO_ANCHOR.load(Ordering::Relaxed);
    anchor + crate::apic::ticks().saturating_sub(mono0)
}
