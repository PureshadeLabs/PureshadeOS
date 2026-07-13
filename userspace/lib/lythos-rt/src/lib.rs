//! **lythos-std** — the standard library for Lythos userspace.
//!
//! Mirrors the structure of Rust's `std` for programs targeting the Lythos
//! microkernel ABI.  Link this crate to get:
//!
//! - A heap (`Vec`, `String`, `Box`, `Arc`, …): 64 KiB static bootstrap arena,
//!   grown on demand via SYS_BRK when the task holds a Memory capability.
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

mod allocator;         // GlobalAlloc — 64 KiB static arena + brk growth

pub mod args;          // argv from the SYS_EXEC initial stack frame + entry!
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

// ── Syscall numbers ───────────────────────────────────────────────────────────

pub use lythos_abi::syscall::*;

// ── Capability rights constants ───────────────────────────────────────────────

pub mod cap_rights {
    pub use lythos_abi::cap::{
        RIGHT_READ   as READ,
        RIGHT_WRITE  as WRITE,
        RIGHT_GRANT  as GRANT,
        RIGHT_REVOKE as REVOKE,
        RIGHT_ALL    as ALL,
    };
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
    /// Path component is not a directory (`ENOTDIR`).
    NotDir,
    /// Filesystem not mounted (`ENOMNT`).
    NoMnt,
    /// Too many open file descriptors (`EMFILE`).
    MFile,
    /// File or directory already exists (`EEXIST`).
    Exist,
    /// No space left on device (`ENOSPC`).
    NoSpc,
    /// A mount already exists at the mount point (`EMOUNTED`).
    Mounted,
    /// Write to a read-only / sealed path (`EROFS`).
    RoFs,
    /// Path is a directory where a regular file is required (`EISDIR`).
    IsDir,
    /// Directory not empty (`ENOTEMPTY`).
    NotEmpty,
    /// I/O or integrity fault — device error, failed auth, corruption (`EIO`).
    Io,
    Unknown(u64),
}

impl SysError {
    pub fn from_raw(v: u64) -> Self {
        use lythos_abi::errno as e;
        match v {
            e::ENOSYS    => SysError::NoSys,
            e::ENOCAP    => SysError::NoCap,
            e::ENOPERM   => SysError::NoPerm,
            e::EINVAL    => SysError::Inval,
            e::ENOENT    => SysError::NoEnt,
            e::EBADF     => SysError::BadFd,
            e::EAGAIN    => SysError::Again,
            e::ENOTDIR   => SysError::NotDir,
            e::ENOMNT    => SysError::NoMnt,
            e::EMFILE    => SysError::MFile,
            e::EEXIST    => SysError::Exist,
            e::ENOSPC    => SysError::NoSpc,
            e::EMOUNTED  => SysError::Mounted,
            e::EROFS     => SysError::RoFs,
            e::EISDIR    => SysError::IsDir,
            e::ENOTEMPTY => SysError::NotEmpty,
            e::EIO       => SysError::Io,
            other        => SysError::Unknown(other),
        }
    }

    /// Whether a raw syscall return is an error sentinel (`errno::is_err`).
    #[inline]
    pub fn is_err_raw(v: u64) -> bool { lythos_abi::errno::is_err(v) }
}

impl core::fmt::Display for SysError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        core::fmt::Debug::fmt(self, f)
    }
}

// Backward-compat alias so existing code using `lythos_rt::Error` still works.
pub use SysError as Error;

// ── Raw syscall helpers ───────────────────────────────────────────────────────

use lythos_syscall::{syscall0, syscall1, syscall2, syscall3, syscall4, syscall6};

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

/// Set or query the program break (heap top). `new_brk = 0` queries.
/// Returns the resulting break. Requires a Memory capability with WRITE
/// right; tasks without one get `ENOPERM` and are limited to the static
/// bootstrap heap in `allocator`.
pub fn sys_brk(new_brk: u64) -> Result<u64, SysError> {
    let r = unsafe { syscall1(SYS_BRK, new_brk) };
    if SysError::is_err_raw(r) { Err(SysError::from_raw(r)) } else { Ok(r) }
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
/// The task starts with an empty argv; see [`sys_exec_argv`].
pub fn sys_exec(elf: &[u8], caps: &[u64]) -> Result<u64, SysError> {
    sys_exec_argv(elf, caps, &[])
}

/// Load and execute a static ELF64 binary, passing `argv` to the new task.
/// The task reads it back via [`args`](crate::args) (or lythos-libstd's
/// `env::args()`); `argv[0]` is by convention the program name. The flat
/// argv buffer must fit the kernel's 4000-byte cap
/// (docs/spec/syscalls.md SYS_EXEC) or the call returns `Inval`.
pub fn sys_exec_argv(elf: &[u8], caps: &[u64], argv: &[&str]) -> Result<u64, SysError> {
    let (argv_ptr, argv_len, _buf);
    if argv.is_empty() {
        (argv_ptr, argv_len, _buf) = (0u64, 0u64, alloc::vec::Vec::new());
    } else {
        // Flat "arg0\0arg1\0…" buffer, the SYS_EXEC a5/a6 wire format.
        let mut buf =
            alloc::vec::Vec::with_capacity(argv.iter().map(|s| s.len() + 1).sum());
        for a in argv {
            buf.extend_from_slice(a.as_bytes());
            buf.push(0);
        }
        (argv_ptr, argv_len, _buf) = (buf.as_ptr() as u64, buf.len() as u64, buf);
    }
    let r = unsafe {
        syscall6(SYS_EXEC, elf.as_ptr() as u64, elf.len() as u64,
                 caps.as_ptr() as u64, caps.len() as u64,
                 argv_ptr, argv_len)
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
// The buffers are heap-backed and allocated on first use: a fixed
// [u8; PIPE_BUF_SIZE] static here would land in every linking binary's BSS,
// and the ELF loader eagerly frames all of p_memsz — 64 KiB per task, idle
// or not.  Only the shell ever pipes, so only the shell pays.  Note this
// means the pipe API effectively requires a Memory capability: the buffer
// exceeds the 64 KiB bootstrap arena, so allocation needs SYS_BRK.
//
// 64 KiB covers virtually all command outputs; the buffer silently truncates
// anything larger (only a problem for `cat bigfile | …` which is unusual).

const PIPE_BUF_SIZE: usize = 64 * 1024;

struct PipeBuf {
    buf: core::cell::UnsafeCell<alloc::vec::Vec<u8>>,
    pos: core::cell::UnsafeCell<usize>,
}
// SAFETY: single-threaded userspace process; no concurrent access.
unsafe impl Sync for PipeBuf {}
impl PipeBuf {
    const fn new() -> Self {
        Self {
            buf: core::cell::UnsafeCell::new(alloc::vec::Vec::new()),
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
    // Reserve the full buffer up front so the sys_log capture path never
    // allocates (an alloc panic mid-log would re-enter capture).
    unsafe { *CAPTURE.buf.get() = alloc::vec::Vec::with_capacity(PIPE_BUF_SIZE); }
    CAPTURE_ACTIVE.store(true, core::sync::atomic::Ordering::Relaxed);
}

/// Stop capturing and return the bytes written since [`pipe_capture_start`].
pub fn pipe_capture_end() -> alloc::vec::Vec<u8> {
    CAPTURE_ACTIVE.store(false, core::sync::atomic::Ordering::Relaxed);
    unsafe { core::mem::take(&mut *CAPTURE.buf.get()) }
}

/// Feed `data` as the stdin source for the next pipeline stage.
///
/// While stdin is active, [`pipe_stdin_active`] returns `true` and
/// [`pipe_stdin_read_all`] drains the buffer.  Call [`pipe_stdin_clear`]
/// after the stage finishes to reset state.
pub fn pipe_stdin_set(data: &[u8]) {
    let n = data.len().min(PIPE_BUF_SIZE);
    unsafe {
        *STDIN.buf.get() = alloc::vec::Vec::from(&data[..n]);
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
    let buf = unsafe { core::mem::take(&mut *STDIN.buf.get()) };
    let pos = unsafe { core::mem::replace(&mut *STDIN.pos.get(), 0) };
    let bytes = buf.get(pos..).unwrap_or(&[]);
    alloc::string::String::from(core::str::from_utf8(bytes).unwrap_or(""))
}

/// Reset piped stdin state and release the buffer.
pub fn pipe_stdin_clear() {
    STDIN_ACTIVE.store(false, core::sync::atomic::Ordering::Relaxed);
    unsafe {
        *STDIN.buf.get() = alloc::vec::Vec::new();
        *STDIN.pos.get() = 0;
    }
}

/// Write a UTF-8 string to the kernel serial console.
///
/// When pipe capture is active (between [`pipe_capture_start`] and
/// [`pipe_capture_end`]), output is diverted into the capture buffer
/// instead of the serial port.  Automatically chunks strings longer
/// than 4096 bytes.  Prefer `print!` / `println!` unless you need raw access.
pub fn sys_log(s: &str) {
    if CAPTURE_ACTIVE.load(core::sync::atomic::Ordering::Relaxed) {
        let b = s.as_bytes();
        unsafe {
            let v = &mut *CAPTURE.buf.get();
            // Alloc-free append into the capacity reserved by
            // pipe_capture_start — extend_from_slice would drag the
            // allocator (and its 64 KiB arena) into every binary that
            // merely prints.
            let space  = PIPE_BUF_SIZE.min(v.capacity()).saturating_sub(v.len());
            let copy_n = b.len().min(space);
            if copy_n > 0 {
                let old = v.len();
                core::ptr::copy_nonoverlapping(b.as_ptr(), v.as_mut_ptr().add(old), copy_n);
                v.set_len(old + copy_n);
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

/// Sleep for at least `ns` nanoseconds (rounded up to the next APIC ms tick).
#[inline]
pub fn sys_nanosleep(ns: u64) {
    unsafe { syscall1(SYS_NANOSLEEP, ns); }
}

/// Return Unix epoch milliseconds (ms since 1970-01-01 00:00:00 UTC).
///
/// Anchored from the CMOS RTC at boot and advanced by the APIC tick counter.
/// Resolution ~1 ms; accuracy within boot-overhead of the RTC reading.
#[inline]
pub fn sys_time_epoch() -> u64 {
    unsafe { syscall0(SYS_TIME_EPOCH) }
}

/// Return the UID of the calling task. Returns 0 (root) until SYS_SETUID is implemented.
#[inline]
pub fn sys_getuid() -> u32 {
    unsafe { syscall0(SYS_GETUID) as u32 }
}

/// Return the GID of the calling task.
#[inline]
pub fn sys_getgid() -> u32 {
    unsafe { syscall0(SYS_GETGID) as u32 }
}

/// Set the UID of the calling task. Returns true on success, false if not permitted.
#[inline]
pub fn sys_setuid(uid: u32) -> bool {
    unsafe { syscall1(SYS_SETUID, uid as u64) == 0 }
}

/// Set the GID of the calling task. Returns true on success, false if not permitted.
#[inline]
pub fn sys_setgid(gid: u32) -> bool {
    unsafe { syscall1(SYS_SETGID, gid as u64) == 0 }
}

/// Return the liveness status of a task by `task_id`.
///
/// Returns the raw kernel value: 0 = dead/missing, 1 = running/ready, 2 = blocked.
/// Use `task::task_status()` for the typed wrapper.
#[inline]
pub fn sys_task_status(task_id: u64) -> u64 {
    unsafe { syscall1(SYS_TASK_STATUS, task_id) }
}

pub use lythos_abi::structs::TaskInfo;

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
/// Wire layout (48 bytes, all LE, naturally aligned):
/// `[0..8]=size [8..16]=mtime [16..24]=ctime [24..28]=flags [28..32]=uid
///  [32..36]=gid [36..40]=nlink [40..42]=mode [42..48]=_pad`
#[derive(Clone, Copy, Default, Debug)]
pub struct FileStat {
    pub size:  u64,
    pub mtime: u64,
    pub ctime: u64,
    pub flags: u32,
    pub uid:   u32,
    pub gid:   u32,
    pub nlink: u32,
    /// Unix permission bits (low 12 bits of st_mode).
    pub mode:  u16,
}

impl FileStat {
    fn from_bytes(b: &[u8; 48]) -> Self {
        FileStat {
            size:  u64::from_le_bytes(b[ 0.. 8].try_into().unwrap()),
            mtime: u64::from_le_bytes(b[ 8..16].try_into().unwrap()),
            ctime: u64::from_le_bytes(b[16..24].try_into().unwrap()),
            flags: u32::from_le_bytes(b[24..28].try_into().unwrap()),
            uid:   u32::from_le_bytes(b[28..32].try_into().unwrap()),
            gid:   u32::from_le_bytes(b[32..36].try_into().unwrap()),
            nlink: u32::from_le_bytes(b[36..40].try_into().unwrap()),
            mode:  u16::from_le_bytes(b[40..42].try_into().unwrap()),
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

/// Mount a filesystem backend at `at` (SYS_MOUNT). Requires a Filesystem
/// capability with WRITE right in the caller's table. `source` selects the
/// backend (`MOUNT_SRC_RFS2_RAM`); `flags` may set `MOUNT_STORE` for
/// read-only-after-realize store semantics. Returns the raw errno on failure.
pub fn sys_mount(at: &str, source: u64, flags: u64) -> Result<(), i64> {
    let r = unsafe { syscall4(SYS_MOUNT, at.as_ptr() as u64, at.len() as u64, source, flags) };
    if (r as i64) < 0 { Err(r as i64) } else { Ok(()) }
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
/// Uses `\r\n` so the cursor returns to column 0 in QEMU raw-terminal mode.
#[macro_export]
macro_rules! println {
    ()            => { $crate::print!("\r\n") };
    ($($arg:tt)*) => { $crate::print!("{}\r\n", ::core::format_args!($($arg)*)) };
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
    ()            => { $crate::eprint!("\r\n") };
    ($($arg:tt)*) => { $crate::eprint!("{}\r\n", ::core::format_args!($($arg)*)) };
}

// ── BootInfo ──────────────────────────────────────────────────────────────────

pub use lythos_abi::ipc::{BootInfo, BOOT_SIGNATURE as BOOT_INFO_SIGNATURE};
