//! **lythos-std** — the standard library for lythos userspace.
//!
//! Mirrors the structure of Rust's `std` for programs targeting the lythos
//! microkernel ABI.  Link this crate to get:
//!
//! - A 4 MiB heap (`Vec`, `String`, `Box`, `Arc`, …).
//! - `print!` / `println!` / `eprint!` / `eprintln!` macros.
//! - `io::{Read, Write, BufWriter, Cursor, …}`
//! - `sync::{Mutex, RwLock, OnceLock, Arc}`
//! - `time::Duration`
//! - `task::{spawn, yield_now, exit}`
//! - `ipc::{Endpoint, Channel<T>}`
//! - `cap::{CapHandle, Rights}`
//! - Raw `sys_*` wrappers for every lythos syscall.
//!
//! # Quick start
//!
//! ```rust,ignore
//! #![no_std]
//! #![no_main]
//! extern crate alloc;
//!
//! use lythos_std::prelude::*;
//!
//! #[unsafe(no_mangle)]
//! pub extern "C" fn _start() -> ! {
//!     println!("hello from lythos!");
//!     lythos_std::task::exit()
//! }
//!
//! #[panic_handler]
//! fn panic(_: &core::panic::PanicInfo) -> ! { loop {} }
//! ```

#![no_std]

extern crate alloc;

// ── Modules ───────────────────────────────────────────────────────────────────

mod allocator;         // GlobalAlloc — 4 MiB static free-list heap

pub mod io;            // Read, Write, Stdout, BufWriter, Cursor, …
pub mod sync;          // Mutex, RwLock, OnceLock, Arc
pub mod time;          // Duration
pub mod task;          // spawn, yield_now, exit
pub mod ipc;           // Endpoint, Channel<T>, Message
pub mod cap;           // CapHandle, Rights
pub mod prelude;       // common re-exports (use lythos_std::prelude::*)

// ── Collections re-exports ────────────────────────────────────────────────────
//
// All collection types live in `alloc`.  Re-export the most common ones at
// module paths that match `std::collections`.

pub mod collections {
    pub use alloc::collections::{BTreeMap, BTreeSet, LinkedList, VecDeque};
    // HashMap / HashSet require a hasher — re-export from alloc's hashbrown.
    // alloc does not bundle hashbrown on no_std; users can add it as a dep.
    // For now the ordered variants are always available.
}

pub mod string {
    pub use alloc::string::{String, ToString};
}

pub mod vec {
    pub use alloc::vec::Vec;
}

pub mod boxed {
    pub use alloc::boxed::Box;
}

// ── Syscall numbers (pub so crates can inspect them) ─────────────────────────

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
/// Write a UTF-8 string to the kernel serial console (debug aid).
pub const SYS_LOG:        u64 = 11;

// ── Capability rights constants ───────────────────────────────────────────────

pub mod cap_rights {
    pub const READ:   u8 = 1;
    pub const WRITE:  u8 = 2;
    pub const GRANT:  u8 = 4;
    pub const REVOKE: u8 = 8;
    pub const ALL:    u8 = 15;
}

// ── Kernel error codes ────────────────────────────────────────────────────────

/// Error codes returned by the lythos kernel (raw syscall layer).
///
/// For idiomatic error handling use `io::Error` or module-specific `Result`
/// types.  These raw variants are returned by the `sys_*` functions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SysError {
    /// Unknown syscall number (`ENOSYS`).
    NoSys,
    /// Invalid or stale capability handle (`ENOCAP`).
    NoCap,
    /// Insufficient capability rights (`ENOPERM`).
    NoPerm,
    /// Invalid argument — bad task ID, self-grant, etc. (`EINVAL`).
    Inval,
    /// An error code that isn't one of the above.
    Unknown(u64),
}

impl SysError {
    pub fn from_raw(v: u64) -> Self {
        match v {
            0xFFFF_FFFF_FFFF_FFFF => SysError::NoSys,
            0xFFFF_FFFF_FFFF_FFFE => SysError::NoCap,
            0xFFFF_FFFF_FFFF_FFFD => SysError::NoPerm,
            0xFFFF_FFFF_FFFF_FFFC => SysError::Inval,
            other                  => SysError::Unknown(other),
        }
    }

    #[inline]
    pub fn is_err_raw(v: u64) -> bool { v >= 0xFFFF_FFFF_FFFF_FFFC }
}

impl core::fmt::Display for SysError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        core::fmt::Debug::fmt(self, f)
    }
}

// Backward-compat alias so existing code using `lythos_std::Error` still works.
pub use SysError as Error;

// ── Raw syscall helpers (private) ─────────────────────────────────────────────

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
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
        );
    }
    ret
}

#[inline] unsafe fn syscall3(nr: u64, a1: u64, a2: u64, a3: u64) -> u64 {
    unsafe { syscall4(nr, a1, a2, a3, 0) }
}
#[inline] unsafe fn syscall2(nr: u64, a1: u64, a2: u64) -> u64 {
    unsafe { syscall4(nr, a1, a2, 0, 0) }
}
#[inline] unsafe fn syscall1(nr: u64, a1: u64) -> u64 {
    unsafe { syscall4(nr, a1, 0, 0, 0) }
}
#[inline] unsafe fn syscall0(nr: u64) -> u64 {
    unsafe { syscall4(nr, 0, 0, 0, 0) }
}

// ── Public raw syscall API ────────────────────────────────────────────────────

/// Yield the current task's CPU slice. See also `task::yield_now`.
#[inline]
pub fn sys_yield() { unsafe { syscall0(SYS_YIELD) }; }

/// Exit the current task. Never returns. See also `task::exit`.
#[inline]
pub fn sys_task_exit() -> ! {
    unsafe { syscall0(SYS_TASK_EXIT) };
    unreachable!()
}

/// Map a physical frame into the address space.
///
/// `flags`: raw x86-64 PTE bits.
/// - User RX (code):  `0x0000_0000_0000_0005` (PRESENT | USER)
/// - User RW (data):  `0x8000_0000_0000_0007` (PRESENT | WRITABLE | USER | NX)
pub fn sys_mmap(virt: u64, phys: u64, flags: u64) -> Result<(), SysError> {
    let r = unsafe { syscall3(SYS_MMAP, virt, phys, flags) };
    if SysError::is_err_raw(r) { Err(SysError::from_raw(r)) } else { Ok(()) }
}

/// Unmap a virtual address.
pub fn sys_munmap(virt: u64) -> Result<(), SysError> {
    let r = unsafe { syscall1(SYS_MUNMAP, virt) };
    if SysError::is_err_raw(r) { Err(SysError::from_raw(r)) } else { Ok(()) }
}

/// Grant a derived capability to another task.
///
/// `rights` — bitmask from `cap_rights`: READ=1, WRITE=2, GRANT=4, REVOKE=8.
/// Returns the handle index in the recipient's table.
pub fn sys_cap_grant(handle: u64, target: u64, rights: u8) -> Result<u64, SysError> {
    let r = unsafe { syscall3(SYS_CAP_GRANT, handle, target, rights as u64) };
    if SysError::is_err_raw(r) { Err(SysError::from_raw(r)) } else { Ok(r) }
}

/// Revoke a capability (cascade).
pub fn sys_cap_revoke(handle: u64) -> Result<(), SysError> {
    let r = unsafe { syscall1(SYS_CAP_REVOKE, handle) };
    if SysError::is_err_raw(r) { Err(SysError::from_raw(r)) } else { Ok(()) }
}

/// Send a message to an IPC endpoint (blocks if full).
pub fn sys_ipc_send(cap: u64, msg: &[u8]) -> Result<(), SysError> {
    let r = unsafe { syscall3(SYS_IPC_SEND, cap, msg.as_ptr() as u64, msg.len() as u64) };
    if SysError::is_err_raw(r) { Err(SysError::from_raw(r)) } else { Ok(()) }
}

/// Receive a message from an IPC endpoint (blocks if empty).
pub fn sys_ipc_recv(cap: u64, buf: &mut [u8]) -> Result<usize, SysError> {
    let r = unsafe {
        syscall3(SYS_IPC_RECV, cap, buf.as_mut_ptr() as u64, buf.len() as u64)
    };
    if SysError::is_err_raw(r) { Err(SysError::from_raw(r)) } else { Ok(r as usize) }
}

/// Create a new IPC endpoint. Returns a capability handle.
pub fn sys_ipc_create() -> Result<u64, SysError> {
    let r = unsafe { syscall0(SYS_IPC_CREATE) };
    if SysError::is_err_raw(r) { Err(SysError::from_raw(r)) } else { Ok(r) }
}

/// Trigger a system rollback (requires Rollback capability).
/// Returns an error if the cap check fails; never returns on success.
pub fn sys_rollback() -> SysError {
    SysError::from_raw(unsafe { syscall0(SYS_ROLLBACK) })
}

/// Load and execute a static ELF64 binary. Returns the new `TaskId`.
pub fn sys_exec(elf: &[u8], caps: &[u64]) -> Result<u64, SysError> {
    let r = unsafe {
        syscall4(SYS_EXEC, elf.as_ptr() as u64, elf.len() as u64,
                 caps.as_ptr() as u64, caps.len() as u64)
    };
    if SysError::is_err_raw(r) { Err(SysError::from_raw(r)) } else { Ok(r) }
}

/// Write a UTF-8 string to the kernel serial console.
///
/// Automatically chunks strings longer than 4096 bytes.
/// Prefer `print!` / `println!` unless you need raw access.
pub fn sys_log(s: &str) {
    let b = s.as_bytes();
    let mut off = 0;
    while off < b.len() {
        let end = (off + 4096).min(b.len());
        unsafe { syscall2(SYS_LOG, b[off..end].as_ptr() as u64, (end - off) as u64) };
        off = end;
    }
}

// ── print! / println! / eprint! / eprintln! ───────────────────────────────────

/// Internal: format `args` into a stack buffer, then emit via `sys_log`.
#[doc(hidden)]
pub fn _print(args: core::fmt::Arguments<'_>) {
    use core::fmt::Write;

    struct StackBuf { data: [u8; 4096], pos: usize }
    impl Write for StackBuf {
        fn write_str(&mut self, s: &str) -> core::fmt::Result {
            let b = s.as_bytes();
            let n = b.len().min(self.data.len() - self.pos);
            self.data[self.pos..self.pos + n].copy_from_slice(&b[..n]);
            self.pos += n;
            Ok(())
        }
    }

    let mut buf = StackBuf { data: [0u8; 4096], pos: 0 };
    let _ = core::fmt::write(&mut buf, args);
    if let Ok(s) = core::str::from_utf8(&buf.data[..buf.pos]) {
        sys_log(s);
    }
}

/// Print to the kernel serial console without a trailing newline.
#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => { $crate::_print(::core::format_args!($($arg)*)) };
}

/// Print to the kernel serial console with a trailing newline.
#[macro_export]
macro_rules! println {
    ()            => { $crate::print!("\n") };
    ($($arg:tt)*) => { $crate::print!("{}\n", ::core::format_args!($($arg)*)) };
}

/// Print to the kernel serial console (stderr) without a trailing newline.
/// Currently routes to the same sink as `print!`.
#[macro_export]
macro_rules! eprint {
    ($($arg:tt)*) => { $crate::_print(::core::format_args!($($arg)*)) };
}

/// Print to the kernel serial console (stderr) with a trailing newline.
#[macro_export]
macro_rules! eprintln {
    ()            => { $crate::eprint!("\n") };
    ($($arg:tt)*) => { $crate::eprint!("{}\n", ::core::format_args!($($arg)*)) };
}

// ── BootInfo ──────────────────────────────────────────────────────────────────

/// Signature constant for the `BootInfo` message.
pub const BOOT_INFO_SIGNATURE: u64 = 0xB007_1000_B007_1000;

/// The 64-byte boot-info message pre-queued by the kernel on capability handle 2.
///
/// lythd reads this on startup via `sys_ipc_recv(2, &mut buf)`.
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
    /// Parse from a raw 64-byte IPC buffer. Returns `None` on signature mismatch.
    pub fn from_bytes(buf: &[u8; 64]) -> Option<Self> {
        let info: Self = unsafe { core::ptr::read_unaligned(buf.as_ptr() as *const Self) };
        if { info.signature } == BOOT_INFO_SIGNATURE { Some(info) } else { None }
    }

    /// CPU vendor string as `&str` (best-effort UTF-8).
    pub fn vendor_str(&self) -> &str {
        core::str::from_utf8(&self.vendor).unwrap_or("unknown")
    }
}
