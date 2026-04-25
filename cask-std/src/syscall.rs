/// Raw cask syscall wrappers.
///
/// Each `syscallN` function places arguments in the correct registers and
/// issues the `syscall` instruction.  The return value (RAX) is returned
/// as a raw `u64`; callers convert to `Result` via `SysError::from_raw`.
///
/// Register convention (matches cask kernel ABI):
///   RAX = syscall number / return value
///   RDI = a1,  RSI = a2,  RDX = a3,  R10 = a4,  R8 = a5,  R9 = a6
///
/// The `syscall` instruction clobbers RCX (saves RIP) and R11 (saves RFLAGS).
/// The kernel does not restore RDI/RSI/RDX/R10/R8/R9 on return; those are
/// marked as inputs only.

// ── Syscall numbers ───────────────────────────────────────────────────────────

pub const SYS_YIELD:         u64 = 0;
pub const SYS_TASK_EXIT:     u64 = 1;
pub const SYS_MMAP:          u64 = 2;
pub const SYS_MUNMAP:        u64 = 3;
pub const SYS_CAP_GRANT:     u64 = 4;
pub const SYS_CAP_REVOKE:    u64 = 5;
pub const SYS_IPC_SEND:      u64 = 6;
pub const SYS_IPC_RECV:      u64 = 7;
pub const SYS_IPC_CREATE:    u64 = 8;
pub const SYS_ROLLBACK:      u64 = 9;
pub const SYS_EXEC:          u64 = 10;
pub const SYS_LOG:           u64 = 11;
pub const SYS_IPC_SEND_CAP:  u64 = 12;
pub const SYS_IPC_RECV_CAP:  u64 = 13;

// ── Error sentinels ───────────────────────────────────────────────────────────

pub const ENOSYS:  u64 = (-1i64) as u64;
pub const ENOCAP:  u64 = (-2i64) as u64;
pub const ENOPERM: u64 = (-3i64) as u64;
pub const EINVAL:  u64 = (-4i64) as u64;

/// The lowest error sentinel value.  Any return value >= this is an error.
pub const ERR_MIN: u64 = EINVAL;

// ── Inline syscall stubs ──────────────────────────────────────────────────────

#[inline]
pub unsafe fn syscall0(nr: u64) -> u64 {
    let ret: u64;
    unsafe { core::arch::asm!(
        "syscall",
        inlateout("rax") nr => ret,
        out("rcx") _,
        out("r11") _,
        options(nostack, preserves_flags),
    ); }
    ret
}

#[inline]
pub unsafe fn syscall1(nr: u64, a1: u64) -> u64 {
    let ret: u64;
    unsafe { core::arch::asm!(
        "syscall",
        inlateout("rax") nr => ret,
        in("rdi") a1,
        out("rcx") _,
        out("r11") _,
        options(nostack, preserves_flags),
    ); }
    ret
}

#[inline]
pub unsafe fn syscall2(nr: u64, a1: u64, a2: u64) -> u64 {
    let ret: u64;
    unsafe { core::arch::asm!(
        "syscall",
        inlateout("rax") nr => ret,
        in("rdi") a1,
        in("rsi") a2,
        out("rcx") _,
        out("r11") _,
        options(nostack, preserves_flags),
    ); }
    ret
}

#[inline]
pub unsafe fn syscall3(nr: u64, a1: u64, a2: u64, a3: u64) -> u64 {
    let ret: u64;
    unsafe { core::arch::asm!(
        "syscall",
        inlateout("rax") nr => ret,
        in("rdi") a1,
        in("rsi") a2,
        in("rdx") a3,
        out("rcx") _,
        out("r11") _,
        options(nostack, preserves_flags),
    ); }
    ret
}

#[inline]
pub unsafe fn syscall4(nr: u64, a1: u64, a2: u64, a3: u64, a4: u64) -> u64 {
    let ret: u64;
    unsafe { core::arch::asm!(
        "syscall",
        inlateout("rax") nr => ret,
        in("rdi") a1,
        in("rsi") a2,
        in("rdx") a3,
        in("r10") a4,
        out("rcx") _,
        out("r11") _,
        options(nostack, preserves_flags),
    ); }
    ret
}

#[inline]
pub unsafe fn syscall6(nr: u64, a1: u64, a2: u64, a3: u64, a4: u64, a5: u64, a6: u64) -> u64 {
    let ret: u64;
    unsafe { core::arch::asm!(
        "syscall",
        inlateout("rax") nr => ret,
        in("rdi") a1,
        in("rsi") a2,
        in("rdx") a3,
        in("r10") a4,
        in("r8")  a5,
        in("r9")  a6,
        out("rcx") _,
        out("r11") _,
        options(nostack, preserves_flags),
    ); }
    ret
}
