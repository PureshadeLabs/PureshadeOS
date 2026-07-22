//! Task exit-status encoding — the contract between `SYS_TASK_EXIT` (the code a
//! task leaves behind) and `SYS_TASK_WAIT` (the status a waiter reads back).
//!
//! Source of truth: `docs/spec/syscalls.md` §SYS_TASK_EXIT / §SYS_TASK_WAIT.
//!
//! A task's exit status is a `u32` status word:
//!
//! * **Normal exit** — the task called `SYS_TASK_EXIT`. Bit 8 (`EXIT_ABNORMAL`)
//!   is clear and the low 8 bits ([`EXIT_CODE_MASK`]) hold the exit code the
//!   task passed (0..=255). `0` conventionally means success.
//! * **Abnormal termination** — the kernel ended the task, not the task itself.
//!   Bit 8 (`EXIT_ABNORMAL`) is set; the low 8 bits carry a reason:
//!   [`EXIT_REASON_KILLED`] (via `SYS_TASK_KILL`) or [`EXIT_REASON_FAULT`] (an
//!   unrecoverable CPU exception — page fault, stack overflow, …).
//!
//! Every status word is `< 0x1_0000`, far below the error-sentinel floor
//! (`errno::ERR_MIN`), so `SYS_TASK_WAIT` returns a status directly in RAX and a
//! caller distinguishes it from "no such task" (which returns `errno::ENOENT`,
//! an error sentinel) with `errno::is_err`. In particular a clean exit with code
//! `0` returns `0` — no longer overloaded with "not found".

/// Mask selecting the exit-code / reason byte of a status word.
pub const EXIT_CODE_MASK: u32 = 0x0000_00FF;

/// Set when the kernel terminated the task (kill or fault) rather than the task
/// exiting via `SYS_TASK_EXIT`.
pub const EXIT_ABNORMAL: u32 = 0x0000_0100;

/// Reason byte (with [`EXIT_ABNORMAL`] set): task terminated by `SYS_TASK_KILL`.
pub const EXIT_REASON_KILLED: u32 = 0;

/// Reason byte (with [`EXIT_ABNORMAL`] set): task terminated by an unrecoverable
/// CPU exception (page fault, stack-guard hit, divide error, …).
pub const EXIT_REASON_FAULT: u32 = 1;

/// Build the status word for a normal `SYS_TASK_EXIT` with `code` (truncated to
/// the low 8 bits — the convention is 0..=255, `0` = success).
#[inline(always)]
pub const fn normal(code: u32) -> u32 {
    code & EXIT_CODE_MASK
}

/// Build the status word for a kernel-forced termination with `reason`
/// ([`EXIT_REASON_KILLED`] / [`EXIT_REASON_FAULT`]).
#[inline(always)]
pub const fn abnormal(reason: u32) -> u32 {
    EXIT_ABNORMAL | (reason & EXIT_CODE_MASK)
}

/// `true` if `status` denotes a normal exit (task called `SYS_TASK_EXIT`).
#[inline(always)]
pub const fn is_normal(status: u32) -> bool {
    status & EXIT_ABNORMAL == 0
}

/// The exit code / reason byte of a status word.
#[inline(always)]
pub const fn code(status: u32) -> u32 {
    status & EXIT_CODE_MASK
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::errno;

    #[test]
    fn normal_zero_is_zero_and_not_an_error() {
        // A clean exit with code 0 encodes as literally 0 — so SYS_TASK_WAIT
        // returning 0 unambiguously means "exited with 0", never "not found".
        let s = normal(0);
        assert_eq!(s, 0);
        assert!(is_normal(s));
        assert_eq!(code(s), 0);
        assert!(!errno::is_err(s as u64));
    }

    #[test]
    fn normal_nonzero_round_trips_and_truncates() {
        assert_eq!(code(normal(42)), 42);
        assert!(is_normal(normal(42)));
        // Only the low 8 bits survive (0..=255 convention).
        assert_eq!(normal(0x1_2A), 0x2A);
    }

    #[test]
    fn abnormal_sets_the_flag_and_carries_reason() {
        let killed = abnormal(EXIT_REASON_KILLED);
        let fault = abnormal(EXIT_REASON_FAULT);
        assert!(!is_normal(killed));
        assert!(!is_normal(fault));
        assert_eq!(code(killed), EXIT_REASON_KILLED);
        assert_eq!(code(fault), EXIT_REASON_FAULT);
        assert_ne!(killed, fault);
    }

    #[test]
    fn every_status_word_is_below_the_error_floor() {
        // Status words must never collide with an errno sentinel, so a waiter
        // can tell a status from "not found" (ENOENT) with errno::is_err.
        for code in 0u32..=255 {
            assert!(!errno::is_err(normal(code) as u64));
            assert!(!errno::is_err(abnormal(code) as u64));
        }
        assert!(errno::is_err(errno::ENOENT));
    }
}
