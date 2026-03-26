//! lythos-std — native syscall wrappers for the lythos microkernel ABI.
//!
//! This crate provides safe, thin wrappers around the lythos syscall interface.
//! All RaptorOS programs should use this crate rather than raw `syscall`
//! instructions.
//!
//! # Syscall ABI
//!
//! Entry via `syscall` instruction:
//!   - RAX = syscall number on entry; return value on exit
//!   - RDI = a1, RSI = a2, RDX = a3, R10 = a4 (not RCX), R8 = a5, R9 = a6
//!
//! Error values are large u64s (negative i64s). See the `Error` type.

#![no_std]

// ── Syscall numbers ───────────────────────────────────────────────────────────

pub const SYS_YIELD:      u64 = 0;
pub const SYS_TASK_EXIT:  u64 = 1;
pub const SYS_MMAP:       u64 = 2;
pub const SYS_MUNMAP:     u64 = 3;
pub const SYS_CAP_GRANT:  u64 = 4;
pub const SYS_CAP_REVOKE: u64 = 5;
pub const SYS_IPC_SEND:   u64 = 6;
pub const SYS_IPC_RECV:   u64 = 7;
pub const SYS_IPC_CREATE: u64 = 8;
pub const SYS_ROLLBACK:   u64 = 9;
pub const SYS_EXEC:       u64 = 10;

// ── Error codes ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// Unknown syscall number.
    NoSys,
    /// Invalid or stale capability handle.
    NoCap,
    /// Insufficient capability rights.
    NoPerm,
    /// Invalid argument (bad task ID, self-grant, etc.).
    Inval,
    /// Unknown error code returned by the kernel.
    Unknown(u64),
}

impl Error {
    pub fn from_raw(v: u64) -> Self {
        match v {
            0xFFFF_FFFF_FFFF_FFFF => Error::NoSys,
            0xFFFF_FFFF_FFFF_FFFE => Error::NoCap,
            0xFFFF_FFFF_FFFF_FFFD => Error::NoPerm,
            0xFFFF_FFFF_FFFF_FFFC => Error::Inval,
            other                  => Error::Unknown(other),
        }
    }

    /// True if this raw return value represents an error.
    #[inline]
    pub fn is_err(v: u64) -> bool {
        v >= 0xFFFF_FFFF_FFFF_FFFC
    }
}

// ── Raw syscall ───────────────────────────────────────────────────────────────

/// Issue a syscall with up to 4 arguments. Returns the raw RAX value.
#[inline]
unsafe fn syscall4(nr: u64, a1: u64, a2: u64, a3: u64, a4: u64) -> u64 {
    let ret: u64;
    unsafe {
        core::arch::asm!(
            "syscall",
            inlateout("rax") nr => ret,
            in("rdi") a1,
            in("rsi") a2,
            in("rdx") a3,
            in("r10") a4,
            // rCX and r11 are clobbered by the syscall instruction itself.
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
        );
    }
    ret
}

#[inline]
unsafe fn syscall3(nr: u64, a1: u64, a2: u64, a3: u64) -> u64 {
    unsafe { syscall4(nr, a1, a2, a3, 0) }
}

#[inline]
unsafe fn syscall1(nr: u64, a1: u64) -> u64 {
    unsafe { syscall4(nr, a1, 0, 0, 0) }
}

#[inline]
unsafe fn syscall0(nr: u64) -> u64 {
    unsafe { syscall4(nr, 0, 0, 0, 0) }
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Yield the current task's CPU time slice to the scheduler.
#[inline]
pub fn sys_yield() {
    unsafe { syscall0(SYS_YIELD) };
}

/// Exit the current task. Never returns.
#[inline]
pub fn sys_task_exit() -> ! {
    unsafe { syscall0(SYS_TASK_EXIT) };
    unreachable!()
}

/// Map a physical frame into the current address space.
///
/// `flags` is a raw x86-64 PTE flag word. Common values:
/// - User read-execute (code):      `0x0000_0000_0000_0005`  (PRESENT | USER)
/// - User read-write (stack/data):  `0x8000_0000_0000_0007`  (PRESENT | WRITABLE | USER | NX)
pub fn sys_mmap(virt: u64, phys: u64, flags: u64) -> Result<(), Error> {
    let r = unsafe { syscall3(SYS_MMAP, virt, phys, flags) };
    if Error::is_err(r) { Err(Error::from_raw(r)) } else { Ok(()) }
}

/// Unmap a virtual address from the current address space.
pub fn sys_munmap(virt: u64) -> Result<(), Error> {
    let r = unsafe { syscall1(SYS_MUNMAP, virt) };
    if Error::is_err(r) { Err(Error::from_raw(r)) } else { Ok(()) }
}

/// Grant a derived capability to another task.
///
/// `rights` is a bitfield: READ=1, WRITE=2, GRANT=4, REVOKE=8.
/// Returns the handle index in the recipient's capability table.
pub fn sys_cap_grant(handle: u64, target_task: u64, rights: u8) -> Result<u64, Error> {
    let r = unsafe { syscall3(SYS_CAP_GRANT, handle, target_task, rights as u64) };
    if Error::is_err(r) { Err(Error::from_raw(r)) } else { Ok(r) }
}

/// Revoke a capability from your own table.
pub fn sys_cap_revoke(handle: u64) -> Result<(), Error> {
    let r = unsafe { syscall1(SYS_CAP_REVOKE, handle) };
    if Error::is_err(r) { Err(Error::from_raw(r)) } else { Ok(()) }
}

/// Send a message to an IPC endpoint.
///
/// Blocks if the ring is full. `msg` is truncated to 64 bytes (MSG_SIZE).
pub fn sys_ipc_send(cap: u64, msg: &[u8]) -> Result<(), Error> {
    let r = unsafe {
        syscall3(SYS_IPC_SEND, cap, msg.as_ptr() as u64, msg.len() as u64)
    };
    if Error::is_err(r) { Err(Error::from_raw(r)) } else { Ok(()) }
}

/// Receive a message from an IPC endpoint.
///
/// Blocks if the ring is empty. Returns the number of bytes written into `buf`.
/// `buf` should be at least 64 bytes (MSG_SIZE).
pub fn sys_ipc_recv(cap: u64, buf: &mut [u8]) -> Result<usize, Error> {
    let r = unsafe {
        syscall3(SYS_IPC_RECV, cap, buf.as_mut_ptr() as u64, buf.len() as u64)
    };
    if Error::is_err(r) { Err(Error::from_raw(r)) } else { Ok(r as usize) }
}

/// Create a new IPC endpoint. Returns a cap handle with full rights.
pub fn sys_ipc_create() -> Result<u64, Error> {
    let r = unsafe { syscall0(SYS_IPC_CREATE) };
    if Error::is_err(r) { Err(Error::from_raw(r)) } else { Ok(r) }
}

/// Trigger a system rollback. Requires a Rollback capability.
/// Never returns on success.
pub fn sys_rollback() -> Error {
    let r = unsafe { syscall0(SYS_ROLLBACK) };
    Error::from_raw(r)
}

/// Load and execute a static ELF64 binary.
///
/// `caps` is a slice of cap handle values from the calling task's table.
/// They are copied into the new task's table as handles 0, 1, 2, ...
/// Returns the new TaskId.
pub fn sys_exec(elf: &[u8], caps: &[u64]) -> Result<u64, Error> {
    let r = unsafe {
        syscall4(
            SYS_EXEC,
            elf.as_ptr() as u64,
            elf.len() as u64,
            caps.as_ptr() as u64,
            caps.len() as u64,
        )
    };
    if Error::is_err(r) { Err(Error::from_raw(r)) } else { Ok(r) }
}

// ── BootInfo ──────────────────────────────────────────────────────────────────

/// Signature constant for the BootInfo message.
pub const BOOT_INFO_SIGNATURE: u64 = 0xB007_1000_B007_1000;

/// The boot-info message pre-queued by the kernel on handle 2.
/// Exactly 64 bytes (MSG_SIZE).
#[repr(C, packed)]
pub struct BootInfo {
    pub signature:   u64,
    pub mem_bytes:   u64,
    pub free_frames: u64,
    pub vendor:      [u8; 12],
    pub _pad:        [u8; 28],
}

const _: () = assert!(core::mem::size_of::<BootInfo>() == 64);

impl BootInfo {
    /// Parse from a raw 64-byte buffer received via SYS_IPC_RECV.
    ///
    /// Returns None if the signature doesn't match.
    pub fn from_bytes(buf: &[u8; 64]) -> Option<Self> {
        let info: Self = unsafe { core::ptr::read_unaligned(buf.as_ptr() as *const Self) };
        if { info.signature } == BOOT_INFO_SIGNATURE { Some(info) } else { None }
    }

    /// CPU vendor string as a UTF-8 str (best-effort).
    pub fn vendor_str(&self) -> &str {
        core::str::from_utf8(&self.vendor).unwrap_or("unknown")
    }
}
