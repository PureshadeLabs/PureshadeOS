//! **lythos-std** — the standard library for Lythos userspace.
//!
//! Mirrors the structure of Rust's `std` for programs targeting the Lythos
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
//! use lythos_rt::prelude::*;
//!
//! #[unsafe(no_mangle)]
//! pub extern "C" fn _start() -> ! {
//!     println!("hello from lythos!");
//!     lythos_rt::task::exit()
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
pub mod prelude;       // common re-exports (use lythos_rt::prelude::*)
pub mod orox;          // OROX binary manifest format

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
pub const SYS_LOG:           u64 = 11;
/// Send a 64-byte message **and** transfer a capability over an IPC endpoint.
/// a1=ipc_cap, a2=msg_ptr, a3=msg_len, a4=cap_handle_to_send (moved from caller).
pub const SYS_IPC_SEND_CAP:  u64 = 12;
/// Receive a 64-byte message **and** accept any in-flight capability.
/// a1=ipc_cap, a2=buf_ptr, a3=buf_len, a4=out_handle_ptr (*mut u64; 0=ignore).
/// Returns bytes received; writes new handle (or u64::MAX if none) to *out_handle_ptr.
pub const SYS_IPC_RECV_CAP:  u64 = 13;
/// Read bytes from the COM1 serial port into a user buffer.
/// a1=buf_ptr (user VA), a2=buf_len.
/// Blocks until at least one byte is available.  Returns bytes read.
pub const SYS_SERIAL_READ:   u64 = 14;
/// Non-blocking serial poll.  No arguments.
/// Returns 1 if at least one byte is waiting in the COM1 FIFO, 0 otherwise.
/// Does NOT consume any bytes (reads LSR bit 0 only, never RBR).
pub const SYS_SERIAL_AVAIL:  u64 = 30;
/// Block the calling task until task `a1` (TaskId) exits.
/// Returns 0 immediately if the target is not found or already Dead.
pub const SYS_TASK_WAIT:     u64 = 31;
/// Return milliseconds elapsed since kernel boot.  No arguments.
/// Return value is always a valid u64 millisecond count (never an error sentinel).
pub const SYS_TIME:          u64 = 15;
/// Return liveness of a task by ID.
/// a1=TaskId.  Returns: 0=dead/missing, 1=running/ready, 2=blocked.
pub const SYS_TASK_STATUS:   u64 = 16;
/// Fill a user buffer with TaskInfo structs for all live tasks.
/// a1=buf_ptr (*mut TaskInfo), a2=buf_capacity (max entries).
/// Returns number of entries written.
pub const SYS_TASK_LIST:     u64 = 17;
/// Return physical memory statistics.  No arguments.
/// Returns free 4 KiB frame count as a u64.
pub const SYS_MEM_STAT:      u64 = 18;
/// Terminate a task by ID.
/// a1=TaskId.  Returns 0 on success, EINVAL if not found/dead/protected.
pub const SYS_TASK_KILL:     u64 = 19;
/// Open a file by path. a1=path_ptr, a2=path_len. Returns fd (≥ 0) or error.
pub const SYS_OPEN:          u64 = 22;
/// Read from an open fd. a1=fd, a2=buf_ptr, a3=len. Returns bytes read or error.
pub const SYS_READ:          u64 = 23;
/// Write to a writable fd. a1=fd, a2=buf_ptr, a3=len. Returns bytes written or error.
pub const SYS_WRITE:         u64 = 24;
/// Close an fd. a1=fd. Returns 0 or error.
pub const SYS_CLOSE:         u64 = 25;
/// Stat a path. a1=path_ptr, a2=path_len, a3=stat_ptr (48 bytes). Returns 0 or error.
pub const SYS_STAT:          u64 = 26;
/// Readdir. a1=path_ptr, a2=path_len, a3=buf_ptr, a4=buf_len. Returns entry count or error.
pub const SYS_READDIR:       u64 = 27;
/// Create a new empty file. a1=path_ptr, a2=path_len. Returns writable fd or error.
pub const SYS_CREATE:        u64 = 28;
/// Delete a file. a1=path_ptr, a2=path_len. Returns 0 or error.
pub const SYS_UNLINK:        u64 = 29;
/// Create a directory. a1=path_ptr, a2=path_len. Returns 0 or error.
pub const SYS_MKDIR:              u64 = 32;
/// Receive from IPC endpoint with a millisecond timeout.
/// a1=cap, a2=buf_ptr, a3=buf_len, a4=timeout_ms. Returns bytes or EAGAIN.
pub const SYS_IPC_RECV_TIMEOUT:  u64 = 42;
/// Send to IPC endpoint with a millisecond timeout.
/// a1=cap, a2=msg_ptr, a3=msg_len, a4=timeout_ms. Returns 0 or EAGAIN.
pub const SYS_IPC_SEND_TIMEOUT:  u64 = 43;
/// Non-blocking IPC recv. a1=cap, a2=buf_ptr, a3=buf_len. Returns bytes or EAGAIN.
pub const SYS_IPC_POLL:          u64 = 39;
/// Bind an IPC endpoint to a name. a1=cap, a2=name_ptr, a3=name_len (≤128). Returns 0 or ENOSYS.
pub const SYS_IPC_BIND:          u64 = 40;
/// Look up a named IPC endpoint. a1=name_ptr, a2=name_len, a3=rights_mask. Returns handle or ENOENT.
pub const SYS_IPC_LOOKUP:        u64 = 41;
/// Create a UDP socket. Returns socket fd (≥ 0) or error (ENOSYS if no net device).
pub const SYS_SOCKET:            u64 = 50;
/// Bind a UDP socket to a local port. a1=fd, a2=port. Returns 0 or error.
pub const SYS_BIND:              u64 = 51;
/// Send a UDP datagram. a1=fd, a2=buf_ptr, a3=len, a4=dst_ip (u32), a5=dst_port (u16).
/// Returns 0 on success, EAGAIN if ARP not yet resolved.
pub const SYS_SENDTO:            u64 = 52;
/// Receive a UDP datagram (blocking). a1=fd, a2=buf_ptr, a3=len,
/// a4=src_ip_out (*mut u32, 0=ignore), a5=src_port_out (*mut u16, 0=ignore).
/// Returns bytes received.
pub const SYS_RECVFROM:          u64 = 53;
/// Close a socket. a1=fd. Returns 0.
pub const SYS_NET_CLOSE:         u64 = 54;
/// Power off the machine (ACPI S5). No arguments. Does not return.
pub const SYS_POWEROFF:          u64 = 55;

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
    NoEnt,
    BadFd,
    Again,
    Unknown(u64),
}

impl SysError {
    pub fn from_raw(v: u64) -> Self {
        match v {
            0xFFFF_FFFF_FFFF_FFFF => SysError::NoSys,
            0xFFFF_FFFF_FFFF_FFFE => SysError::NoCap,
            0xFFFF_FFFF_FFFF_FFFD => SysError::NoPerm,
            0xFFFF_FFFF_FFFF_FFFC => SysError::Inval,
            0xFFFF_FFFF_FFFF_FFFB => SysError::NoEnt,
            0xFFFF_FFFF_FFFF_FFFA => SysError::BadFd,
            0xFFFF_FFFF_FFFF_FFF9 => SysError::Again,
            other                  => SysError::Unknown(other),
        }
    }

    #[inline]
    pub fn is_err_raw(v: u64) -> bool { v >= 0xFFFF_FFFF_FFFF_FFF9 }
}

impl core::fmt::Display for SysError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        core::fmt::Debug::fmt(self, f)
    }
}

// Backward-compat alias so existing code using `lythos_rt::Error` still works.
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

#[inline]
unsafe fn syscall6(nr: u64, a1: u64, a2: u64, a3: u64, a4: u64, a5: u64, a6: u64) -> u64 {
    let ret: u64;
    unsafe {
        core::arch::asm!(
            "syscall",
            inlateout("rax") nr => ret,
            in("rdi") a1,
            in("rsi") a2,
            in("rdx") a3,
            in("r10") a4,
            in("r8")  a5,
            in("r9")  a6,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
        );
    }
    ret
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

/// Send a message **and** transfer a capability over an IPC endpoint.
///
/// `cap_to_send` is moved out of the caller's capability table.
/// Blocks if the endpoint ring buffer is full.
pub fn sys_ipc_send_cap(ipc_cap: u64, msg: &[u8], cap_to_send: u64) -> Result<(), SysError> {
    let r = unsafe {
        syscall4(SYS_IPC_SEND_CAP,
                 ipc_cap,
                 msg.as_ptr() as u64,
                 msg.len() as u64,
                 cap_to_send)
    };
    if SysError::is_err_raw(r) { Err(SysError::from_raw(r)) } else { Ok(()) }
}

/// Receive a message and accept any in-flight capability from an IPC endpoint.
///
/// Returns `(bytes_received, Some(handle))` if a capability was attached to the
/// message, or `(bytes_received, None)` if no capability was in flight.
/// Blocks if the endpoint ring buffer is empty.
pub fn sys_ipc_recv_cap(ipc_cap: u64, buf: &mut [u8]) -> Result<(usize, Option<u64>), SysError> {
    let mut out_handle: u64 = u64::MAX;
    let r = unsafe {
        syscall4(SYS_IPC_RECV_CAP,
                 ipc_cap,
                 buf.as_mut_ptr() as u64,
                 buf.len() as u64,
                 &mut out_handle as *mut u64 as u64)
    };
    if SysError::is_err_raw(r) {
        return Err(SysError::from_raw(r));
    }
    let cap = if out_handle == u64::MAX { None } else { Some(out_handle) };
    Ok((r as usize, cap))
}

/// Trigger a system rollback (requires Rollback capability).
/// Returns an error if the cap check fails; never returns on success.
pub fn sys_rollback() -> SysError {
    SysError::from_raw(unsafe { syscall0(SYS_ROLLBACK) })
}

/// Load and execute a static ELF64 binary. Returns the new `TaskId`.
pub fn sys_exec(elf: &[u8], caps: &[u64]) -> Result<u64, SysError> {
    let r = unsafe {
        syscall6(SYS_EXEC, elf.as_ptr() as u64, elf.len() as u64,
                 caps.as_ptr() as u64, caps.len() as u64,
                 0, 0)
    };
    if SysError::is_err_raw(r) { Err(SysError::from_raw(r)) } else { Ok(r) }
}

// ── Pipe capture ─────────────────────────────────────────────────────────────
//
// lysh implements shell pipes by capturing one stage's output (via sys_log
// interception) then feeding it as stdin to the next stage.  No IPC or
// separate process needed — the scheduler is cooperative and commands are
// library calls inside the shell process.
//
// PIPE_BUF_SIZE must be a power-of-two multiple of 4096 and fit in BSS.
// 64 KiB covers virtually all command outputs; the buffer silently truncates
// anything larger (only a problem for `cat bigfile | …` which is unusual).

const PIPE_BUF_SIZE: usize = 64 * 1024;

struct PipeBuf {
    buf: core::cell::UnsafeCell<[u8; PIPE_BUF_SIZE]>,
    len: core::cell::UnsafeCell<usize>,
    pos: core::cell::UnsafeCell<usize>,
}
// SAFETY: single-threaded userspace process; no concurrent access.
unsafe impl Sync for PipeBuf {}
impl PipeBuf {
    const fn new() -> Self {
        Self {
            buf: core::cell::UnsafeCell::new([0; PIPE_BUF_SIZE]),
            len: core::cell::UnsafeCell::new(0),
            pos: core::cell::UnsafeCell::new(0),
        }
    }
}

static CAPTURE_ACTIVE: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);
static CAPTURE: PipeBuf = PipeBuf::new();

static STDIN_ACTIVE: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);
static STDIN: PipeBuf = PipeBuf::new();

/// Begin capturing all `print!` / `sys_log` output into an internal buffer.
///
/// Output is silently truncated at `PIPE_BUF_SIZE` (64 KiB).
/// Call [`pipe_capture_end`] to stop capturing and retrieve the buffer.
pub fn pipe_capture_start() {
    unsafe { *CAPTURE.len.get() = 0; }
    CAPTURE_ACTIVE.store(true, core::sync::atomic::Ordering::Relaxed);
}

/// Stop capturing and return the bytes written since [`pipe_capture_start`].
pub fn pipe_capture_end() -> alloc::vec::Vec<u8> {
    CAPTURE_ACTIVE.store(false, core::sync::atomic::Ordering::Relaxed);
    let len = unsafe { *CAPTURE.len.get() };
    let buf = unsafe { &(&*CAPTURE.buf.get())[..len] };
    alloc::vec::Vec::from(buf)
}

/// Feed `data` as the stdin source for the next pipeline stage.
///
/// While stdin is active, [`pipe_stdin_active`] returns `true` and
/// [`pipe_stdin_read_all`] drains the buffer.  Call [`pipe_stdin_clear`]
/// after the stage finishes to reset state.
pub fn pipe_stdin_set(data: &[u8]) {
    let n = data.len().min(PIPE_BUF_SIZE);
    unsafe {
        (&mut *STDIN.buf.get())[..n].copy_from_slice(&data[..n]);
        *STDIN.len.get() = n;
        *STDIN.pos.get() = 0;
    }
    STDIN_ACTIVE.store(true, core::sync::atomic::Ordering::Relaxed);
}

/// Return `true` if piped stdin data is available.
#[inline]
pub fn pipe_stdin_active() -> bool {
    STDIN_ACTIVE.load(core::sync::atomic::Ordering::Relaxed)
}

/// Consume and return all remaining piped stdin as a UTF-8 string.
///
/// Resets the stdin state; subsequent calls return an empty string.
pub fn pipe_stdin_read_all() -> alloc::string::String {
    STDIN_ACTIVE.store(false, core::sync::atomic::Ordering::Relaxed);
    let len = unsafe { *STDIN.len.get() };
    let pos = unsafe { *STDIN.pos.get() };
    unsafe { *STDIN.pos.get() = len; }
    let buf = unsafe { &(&*STDIN.buf.get())[pos..len] };
    alloc::string::String::from(core::str::from_utf8(buf).unwrap_or(""))
}

/// Reset piped stdin state without consuming the data.
pub fn pipe_stdin_clear() {
    STDIN_ACTIVE.store(false, core::sync::atomic::Ordering::Relaxed);
}

/// Write a UTF-8 string to the kernel serial console.
///
/// When pipe capture is active (between [`pipe_capture_start`] and
/// [`pipe_capture_end`]), output is diverted into the capture buffer
/// instead of the serial port.  Automatically chunks strings longer
/// than 4096 bytes.  Prefer `print!` / `println!` unless you need raw access.
pub fn sys_log(s: &str) {
    if CAPTURE_ACTIVE.load(core::sync::atomic::Ordering::Relaxed) {
        let b       = s.as_bytes();
        let cur_len = unsafe { *CAPTURE.len.get() };
        let space   = PIPE_BUF_SIZE - cur_len;
        let copy_n  = b.len().min(space);
        if copy_n > 0 {
            unsafe {
                (&mut *CAPTURE.buf.get())[cur_len..cur_len + copy_n]
                    .copy_from_slice(&b[..copy_n]);
                *CAPTURE.len.get() = cur_len + copy_n;
            }
        }
        return;
    }
    let b = s.as_bytes();
    let mut off = 0;
    while off < b.len() {
        let end = (off + 4096).min(b.len());
        unsafe { syscall2(SYS_LOG, b[off..end].as_ptr() as u64, (end - off) as u64) };
        off = end;
    }
}

/// Return `true` if at least one byte is waiting in the COM1 FIFO.
///
/// Non-blocking and non-destructive — only reads LSR bit 0, never RBR.
/// Use immediately after receiving `0x1B` (ESC) to distinguish a plain ESC
/// keypress from the start of a VT100 escape sequence: if `sys_serial_avail()`
/// returns `false` the escape sequence bytes haven't arrived yet, so it was a
/// bare ESC; if `true`, read the remaining sequence bytes with `sys_serial_read`.
#[inline]
pub fn sys_serial_avail() -> bool {
    unsafe { syscall0(SYS_SERIAL_AVAIL) != 0 }
}

/// Block the calling task until task `tid` has exited.
///
/// Returns immediately if the task is already dead. Returns `Err` only if
/// the kernel rejects the call (e.g. invalid tid format).
pub fn sys_task_wait(tid: u64) -> Result<(), SysError> {
    let r = unsafe { syscall1(SYS_TASK_WAIT, tid) };
    if SysError::is_err_raw(r) { Err(SysError::from_raw(r)) } else { Ok(()) }
}

/// Read bytes from the COM1 serial port into `buf`.
///
/// Blocks (yielding the CPU) until at least one byte is available, then
/// reads as many bytes as are ready (up to `buf.len()`).  Returns the number
/// of bytes written into `buf`.
pub fn sys_serial_read(buf: &mut [u8]) -> Result<usize, SysError> {
    if buf.is_empty() { return Ok(0); }
    let r = unsafe { syscall2(SYS_SERIAL_READ, buf.as_mut_ptr() as u64, buf.len() as u64) };
    if SysError::is_err_raw(r) { Err(SysError::from_raw(r)) } else { Ok(r as usize) }
}

/// Return milliseconds elapsed since kernel boot.
///
/// Backed by the calibrated APIC tick counter (~1 ms resolution).
/// Use `time::Instant::now()` for ergonomic access.
#[inline]
pub fn sys_time() -> u64 {
    unsafe { syscall0(SYS_TIME) }
}

/// Return the liveness status of a task by `task_id`.
///
/// Returns the raw kernel value: 0 = dead/missing, 1 = running/ready, 2 = blocked.
/// Use `task::task_status()` for the typed wrapper.
#[inline]
pub fn sys_task_status(task_id: u64) -> u64 {
    unsafe { syscall1(SYS_TASK_STATUS, task_id) }
}

/// Filled by `sys_task_list` for each live task.
#[repr(C)]
pub struct TaskInfo {
    pub id:    u64,
    /// 1 = running/ready, 2 = blocked
    pub state: u64,
    /// 0 = kernel task, 1 = userspace task
    pub kind:  u8,
    pub _pad:  [u8; 7],
}

/// Fill `buf` with info on every live task.  Returns the number of entries written.
pub fn sys_task_list(buf: &mut [TaskInfo]) -> usize {
    if buf.is_empty() { return 0; }
    let n = unsafe {
        syscall2(SYS_TASK_LIST, buf.as_mut_ptr() as u64, buf.len() as u64)
    };
    n as usize
}

/// Return the number of free 4 KiB physical frames.
#[inline]
pub fn sys_mem_stat() -> u64 {
    unsafe { syscall0(SYS_MEM_STAT) }
}

/// Terminate a task by ID.  Returns `true` on success.
#[inline]
pub fn sys_task_kill(task_id: u64) -> bool {
    let r = unsafe { syscall1(SYS_TASK_KILL, task_id) };
    r == 0
}

// ── VFS types ─────────────────────────────────────────────────────────────────

/// File stat info returned by [`sys_stat`].
///
/// Wire layout (48 bytes, all LE):
/// `[0..8]=size [8..12]=flags [12..14]=mode [14..18]=uid [18..22]=gid
///  [22..26]=nlink [26..34]=mtime [34..42]=ctime [42..48]=_pad`
#[derive(Clone, Copy, Default, Debug)]
pub struct FileStat {
    pub size:  u64,
    pub flags: u32,
    /// Unix permission bits (low 12 bits of st_mode).
    pub mode:  u16,
    pub uid:   u32,
    pub gid:   u32,
    pub nlink: u32,
    pub mtime: u64,
    pub ctime: u64,
}

impl FileStat {
    fn from_bytes(b: &[u8; 48]) -> Self {
        FileStat {
            size:  u64::from_le_bytes(b[ 0.. 8].try_into().unwrap()),
            flags: u32::from_le_bytes(b[ 8..12].try_into().unwrap()),
            mode:  u16::from_le_bytes(b[12..14].try_into().unwrap()),
            uid:   u32::from_le_bytes(b[14..18].try_into().unwrap()),
            gid:   u32::from_le_bytes(b[18..22].try_into().unwrap()),
            nlink: u32::from_le_bytes(b[22..26].try_into().unwrap()),
            mtime: u64::from_le_bytes(b[26..34].try_into().unwrap()),
            ctime: u64::from_le_bytes(b[34..42].try_into().unwrap()),
        }
    }

    /// True if this inode is a directory.
    #[inline] pub fn is_dir(&self)     -> bool { self.flags & 0x2 != 0 }
    /// True if this inode is a symlink.
    #[inline] pub fn is_symlink(&self) -> bool { self.flags & 0x4 != 0 }
}

/// RFS inode flag constants (matches kernel `rfs::INODE_*`).
pub mod inode_flags {
    pub const USED:     u32 = 1 << 0;
    pub const DIR:      u32 = 1 << 1;
    pub const SYMLINK:  u32 = 1 << 2;
    pub const FAST_SYM: u32 = 1 << 3;
}

/// RFS directory-entry file_type constants.
pub mod file_type {
    pub const REG:     u8 = 1;
    pub const DIR:     u8 = 2;
    pub const SYMLINK: u8 = 3;
}

/// Wire size of one readdir entry buffer slot (264 bytes).
pub const DIR_ENTRY_SIZE: usize = 264;

/// One directory entry returned by [`sys_readdir`].
#[derive(Clone, Debug)]
pub struct DirEntry {
    pub ino:       u32,
    pub file_type: u8,
    name_len:      u8,
    name_buf:      [u8; 256],
}

impl DirEntry {
    fn from_wire(b: &[u8; DIR_ENTRY_SIZE]) -> Self {
        let mut name_buf = [0u8; 256];
        name_buf.copy_from_slice(&b[8..264]);
        DirEntry {
            ino:       u32::from_le_bytes(b[0..4].try_into().unwrap()),
            file_type: b[4],
            name_len:  b[5],
            name_buf,
        }
    }

    /// Entry name as `&str` (UTF-8, empty string on decode failure).
    pub fn name(&self) -> &str {
        let len = self.name_len as usize;
        core::str::from_utf8(&self.name_buf[..len]).unwrap_or("")
    }
}

// ── VFS syscall wrappers ──────────────────────────────────────────────────────

/// Open a file by path. Returns fd (≥ 0) on success.
pub fn sys_open(path: &str) -> Result<u64, ()> {
    let r = unsafe { syscall2(SYS_OPEN, path.as_ptr() as u64, path.len() as u64) };
    if (r as i64) < 0 { Err(()) } else { Ok(r) }
}

/// Read up to `buf.len()` bytes from `fd`. Returns bytes read.
pub fn sys_read_fd(fd: u64, buf: &mut [u8]) -> Result<usize, ()> {
    let r = unsafe { syscall3(SYS_READ, fd, buf.as_mut_ptr() as u64, buf.len() as u64) };
    if (r as i64) < 0 { Err(()) } else { Ok(r as usize) }
}

/// Close `fd`.
pub fn sys_close(fd: u64) {
    unsafe { syscall1(SYS_CLOSE, fd) };
}

/// Stat `path`. Returns `None` if not found.
pub fn sys_stat(path: &str) -> Option<FileStat> {
    let mut buf = [0u8; 48];
    let r = unsafe {
        syscall3(SYS_STAT, path.as_ptr() as u64, path.len() as u64, buf.as_mut_ptr() as u64)
    };
    if (r as i64) < 0 { None } else { Some(FileStat::from_bytes(&buf)) }
}

/// Write `buf` to an open writable `fd`. Returns bytes written.
pub fn sys_write_fd(fd: u64, buf: &[u8]) -> Result<usize, ()> {
    let r = unsafe { syscall3(SYS_WRITE, fd, buf.as_ptr() as u64, buf.len() as u64) };
    if (r as i64) < 0 { Err(()) } else { Ok(r as usize) }
}

/// Create a new empty regular file. Returns a writable fd on success.
pub fn sys_create(path: &str) -> Result<u64, ()> {
    let r = unsafe { syscall2(SYS_CREATE, path.as_ptr() as u64, path.len() as u64) };
    if (r as i64) < 0 { Err(()) } else { Ok(r) }
}

/// Delete a regular file.
pub fn sys_unlink(path: &str) -> Result<(), ()> {
    let r = unsafe { syscall2(SYS_UNLINK, path.as_ptr() as u64, path.len() as u64) };
    if (r as i64) < 0 { Err(()) } else { Ok(()) }
}

/// Receive from an IPC endpoint with a timeout.
///
/// Returns `Ok(bytes)` on success, or `Err(EAGAIN)` if no message arrived
/// within `timeout_ms` milliseconds.
pub fn sys_ipc_recv_timeout(cap: u64, buf: &mut [u8], timeout_ms: u64) -> Result<usize, SysError> {
    let r = unsafe {
        syscall4(SYS_IPC_RECV_TIMEOUT, cap, buf.as_mut_ptr() as u64, buf.len() as u64, timeout_ms)
    };
    if SysError::is_err_raw(r) { Err(SysError::from_raw(r)) } else { Ok(r as usize) }
}

/// Send to an IPC endpoint with a timeout.
///
/// Returns `Ok(())` on success, or `Err(EAGAIN)` if the ring stayed full for
/// `timeout_ms` milliseconds.
pub fn sys_ipc_send_timeout(cap: u64, msg: &[u8], timeout_ms: u64) -> Result<(), SysError> {
    let r = unsafe {
        syscall4(SYS_IPC_SEND_TIMEOUT, cap, msg.as_ptr() as u64, msg.len() as u64, timeout_ms)
    };
    if SysError::is_err_raw(r) { Err(SysError::from_raw(r)) } else { Ok(()) }
}

/// Create a UDP socket. Returns the socket fd on success.
pub fn sys_socket() -> Result<u64, SysError> {
    let r = unsafe { syscall0(SYS_SOCKET) };
    if SysError::is_err_raw(r) { Err(SysError::from_raw(r)) } else { Ok(r) }
}

/// Bind a UDP socket to a local port.
pub fn sys_bind(fd: u64, port: u16) -> Result<(), SysError> {
    let r = unsafe { syscall2(SYS_BIND, fd, port as u64) };
    if SysError::is_err_raw(r) { Err(SysError::from_raw(r)) } else { Ok(()) }
}

/// Send a UDP datagram.
///
/// `dst_ip` is an IPv4 address in host byte order (e.g. `0x0A00_0202` = 10.0.2.2).
/// Returns `Err(EAGAIN)` if the ARP entry for `dst_ip` is not yet resolved.
pub fn sys_sendto(fd: u64, buf: &[u8], dst_ip: u32, dst_port: u16) -> Result<(), SysError> {
    let r = unsafe {
        syscall6(SYS_SENDTO, fd, buf.as_ptr() as u64, buf.len() as u64,
                 dst_ip as u64, dst_port as u64, 0)
    };
    if SysError::is_err_raw(r) { Err(SysError::from_raw(r)) } else { Ok(()) }
}

/// Receive a UDP datagram (blocking).
///
/// Returns `(bytes_received, src_ip, src_port)`.
pub fn sys_recvfrom(fd: u64, buf: &mut [u8]) -> Result<(usize, u32, u16), SysError> {
    let mut src_ip:   u32 = 0;
    let mut src_port: u16 = 0;
    let r = unsafe {
        syscall6(SYS_RECVFROM, fd, buf.as_mut_ptr() as u64, buf.len() as u64,
                 &mut src_ip   as *mut u32 as u64,
                 &mut src_port as *mut u16 as u64,
                 0)
    };
    if SysError::is_err_raw(r) {
        Err(SysError::from_raw(r))
    } else {
        Ok((r as usize, src_ip, src_port))
    }
}

/// Close a socket.
pub fn sys_net_close(fd: u64) {
    unsafe { syscall1(SYS_NET_CLOSE, fd) };
}

/// Power off the machine via ACPI S5. Does not return.
pub fn sys_poweroff() -> ! {
    unsafe { syscall0(SYS_POWEROFF) };
    loop {}
}

/// Create a directory at `path`. Parent directory must already exist.
pub fn sys_mkdir(path: &str) -> Result<(), ()> {
    let r = unsafe { syscall2(SYS_MKDIR, path.as_ptr() as u64, path.len() as u64) };
    if (r as i64) < 0 { Err(()) } else { Ok(()) }
}

/// Non-blocking receive from an IPC endpoint.
///
/// Returns `Ok(bytes)` if a message was ready, or `Err(Again)` if the ring is empty.
pub fn sys_ipc_poll(cap: u64, buf: &mut [u8]) -> Result<usize, SysError> {
    let r = unsafe { syscall3(SYS_IPC_POLL, cap, buf.as_mut_ptr() as u64, buf.len() as u64) };
    if SysError::is_err_raw(r) { Err(SysError::from_raw(r)) } else { Ok(r as usize) }
}

/// Bind an IPC endpoint to a well-known name in the kernel registry.
///
/// `name` must be ≤128 bytes. Returns `Err(NoSys)` if the name is already taken.
/// Requires GRANT right on the cap.
pub fn sys_ipc_bind(cap: u64, name: &str) -> Result<(), SysError> {
    let r = unsafe { syscall3(SYS_IPC_BIND, cap, name.as_ptr() as u64, name.len() as u64) };
    if SysError::is_err_raw(r) { Err(SysError::from_raw(r)) } else { Ok(()) }
}

/// Look up a named IPC endpoint and obtain a new cap handle.
///
/// `rights` is a bitmask from `cap_rights` (GRANT/REVOKE are stripped by the kernel).
/// Returns `Err(NoEnt)` if the name is not registered.
pub fn sys_ipc_lookup(name: &str, rights: u8) -> Result<u64, SysError> {
    let r = unsafe {
        syscall3(SYS_IPC_LOOKUP, name.as_ptr() as u64, name.len() as u64, rights as u64)
    };
    if SysError::is_err_raw(r) { Err(SysError::from_raw(r)) } else { Ok(r) }
}

/// Read directory entries for `path`. Returns `None` if not a directory or not found.
pub fn sys_readdir(path: &str) -> Option<alloc::vec::Vec<DirEntry>> {
    const MAX_ENTRIES: usize = 512;
    let buf_len = MAX_ENTRIES * DIR_ENTRY_SIZE;
    let mut buf = alloc::vec![0u8; buf_len];
    let count = unsafe {
        syscall4(SYS_READDIR,
            path.as_ptr() as u64,
            path.len() as u64,
            buf.as_mut_ptr() as u64,
            buf_len as u64)
    };
    if (count as i64) < 0 { return None; }
    let count = count as usize;
    let mut entries = alloc::vec::Vec::with_capacity(count);
    for i in 0..count {
        let off = i * DIR_ENTRY_SIZE;
        let chunk: &[u8; DIR_ENTRY_SIZE] = buf[off..off + DIR_ENTRY_SIZE].try_into().unwrap();
        entries.push(DirEntry::from_wire(chunk));
    }
    Some(entries)
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
