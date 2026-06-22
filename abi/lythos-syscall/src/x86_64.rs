//! x86_64-specific `syscall` instruction wrappers.
//!
//! ## Register convention (lythos kernel ABI)
//!
//! | Register | Role on entry           | Role on return |
//! |----------|-------------------------|----------------|
//! | RAX      | syscall number          | return value   |
//! | RDI      | argument 1              |                |
//! | RSI      | argument 2              |                |
//! | RDX      | argument 3              |                |
//! | R10      | argument 4 (RCX used by `syscall`) | |
//! | R8       | argument 5              |                |
//! | R9       | argument 6              |                |
//! | RCX      | clobbered (CPU saves user RIP here) | |
//! | R11      | clobbered (CPU saves user RFLAGS)   | |

use lythos_abi::syscall as sys;

// ── Raw asm stubs ─────────────────────────────────────────────────────────────

/// `syscall(nr)` → RAX.
#[inline(always)]
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

/// `syscall(nr, a1)` → RAX.
#[inline(always)]
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

/// `syscall(nr, a1, a2)` → RAX.
#[inline(always)]
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

/// `syscall(nr, a1, a2, a3)` → RAX.
#[inline(always)]
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

/// `syscall(nr, a1, a2, a3, a4)` → RAX.
#[inline(always)]
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

/// `syscall(nr, a1, a2, a3, a4, a5)` → RAX.
#[inline(always)]
pub unsafe fn syscall5(nr: u64, a1: u64, a2: u64, a3: u64, a4: u64, a5: u64) -> u64 {
    let ret: u64;
    unsafe { core::arch::asm!(
        "syscall",
        inlateout("rax") nr => ret,
        in("rdi") a1,
        in("rsi") a2,
        in("rdx") a3,
        in("r10") a4,
        in("r8")  a5,
        out("rcx") _,
        out("r11") _,
        options(nostack, preserves_flags),
    ); }
    ret
}

/// `syscall(nr, a1, a2, a3, a4, a5, a6)` → RAX.
#[inline(always)]
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

// ── Typed wrappers — one per syscall ─────────────────────────────────────────

/// Voluntarily yield the CPU.
#[inline(always)]
pub unsafe fn sys_yield() {
    unsafe { syscall0(sys::SYS_YIELD) };
}

/// Terminate the calling task (does not return).
#[inline(always)]
pub unsafe fn sys_task_exit() -> ! {
    unsafe { syscall0(sys::SYS_TASK_EXIT) };
    unreachable!()
}

/// Map anonymous frames. Returns 0 or error.
#[inline(always)]
pub unsafe fn sys_mmap(virt: u64, phys: u64, flags: u64) -> u64 {
    unsafe { syscall3(sys::SYS_MMAP, virt, phys, flags) }
}

/// Unmap a virtual page. Returns 0 or error.
#[inline(always)]
pub unsafe fn sys_munmap(virt: u64) -> u64 {
    unsafe { syscall1(sys::SYS_MUNMAP, virt) }
}

/// Grant a derived capability to another task.
/// Returns new handle index in recipient's table, or error.
#[inline(always)]
pub unsafe fn sys_cap_grant(handle: u64, target_task: u64, rights: u64) -> u64 {
    unsafe { syscall3(sys::SYS_CAP_GRANT, handle, target_task, rights) }
}

/// Cascade-revoke a capability. Returns 0 or error.
#[inline(always)]
pub unsafe fn sys_cap_revoke(handle: u64) -> u64 {
    unsafe { syscall1(sys::SYS_CAP_REVOKE, handle) }
}

/// Send a message to an IPC endpoint (blocking). Returns 0 or error.
#[inline(always)]
pub unsafe fn sys_ipc_send(cap: u64, msg_ptr: u64, msg_len: u64) -> u64 {
    unsafe { syscall3(sys::SYS_IPC_SEND, cap, msg_ptr, msg_len) }
}

/// Receive a message from an IPC endpoint (blocking). Returns bytes or error.
#[inline(always)]
pub unsafe fn sys_ipc_recv(cap: u64, buf_ptr: u64, buf_len: u64) -> u64 {
    unsafe { syscall3(sys::SYS_IPC_RECV, cap, buf_ptr, buf_len) }
}

/// Create a new IPC endpoint. Returns new cap handle or error.
#[inline(always)]
pub unsafe fn sys_ipc_create() -> u64 {
    unsafe { syscall0(sys::SYS_IPC_CREATE) }
}

/// Trigger kernel rollback (does not return on success). Requires Rollback cap.
#[inline(always)]
pub unsafe fn sys_rollback() -> u64 {
    unsafe { syscall0(sys::SYS_ROLLBACK) }
}

/// Exec a new task from an ELF blob. Returns 0 or error.
#[inline(always)]
pub unsafe fn sys_exec(elf_ptr: u64, elf_len: u64, cap_handle: u64) -> u64 {
    unsafe { syscall3(sys::SYS_EXEC, elf_ptr, elf_len, cap_handle) }
}

/// Write a UTF-8 string to the kernel serial console. Returns 0 or error.
#[inline(always)]
pub unsafe fn sys_log(str_ptr: u64, str_len: u64) -> u64 {
    unsafe { syscall2(sys::SYS_LOG, str_ptr, str_len) }
}

/// Send a message and transfer a capability atomically. Returns 0 or error.
#[inline(always)]
pub unsafe fn sys_ipc_send_cap(cap: u64, msg_ptr: u64, msg_len: u64, cap_to_send: u64) -> u64 {
    unsafe { syscall4(sys::SYS_IPC_SEND_CAP, cap, msg_ptr, msg_len, cap_to_send) }
}

/// Receive a message and accept a capability. Writes cap handle to `*out_handle`.
/// Returns bytes received or error.
#[inline(always)]
pub unsafe fn sys_ipc_recv_cap(cap: u64, buf_ptr: u64, buf_len: u64, out_handle_ptr: u64) -> u64 {
    unsafe { syscall4(sys::SYS_IPC_RECV_CAP, cap, buf_ptr, buf_len, out_handle_ptr) }
}

/// Read bytes from COM1 serial port (blocking). Returns bytes read or error.
#[inline(always)]
pub unsafe fn sys_serial_read(buf_ptr: u64, buf_len: u64) -> u64 {
    unsafe { syscall2(sys::SYS_SERIAL_READ, buf_ptr, buf_len) }
}

/// Check COM1 data availability. Returns 1 if data ready, 0 if not.
#[inline(always)]
pub unsafe fn sys_serial_avail() -> u64 {
    unsafe { syscall0(sys::SYS_SERIAL_AVAIL) }
}

/// Milliseconds since kernel boot. Returns u64.
#[inline(always)]
pub unsafe fn sys_time() -> u64 {
    unsafe { syscall0(sys::SYS_TIME) }
}

/// Unix epoch milliseconds (UTC). Returns u64.
#[inline(always)]
pub unsafe fn sys_time_epoch() -> u64 {
    unsafe { syscall0(sys::SYS_TIME_EPOCH) }
}

/// Sleep for at least `ns` nanoseconds (rounded up to next ms tick). Returns 0.
#[inline(always)]
pub unsafe fn sys_nanosleep(ns: u64) -> u64 {
    unsafe { syscall1(sys::SYS_NANOSLEEP, ns) }
}

/// Query task liveness. Returns 0=dead, 1=running, 2=ready, 3=blocked.
#[inline(always)]
pub unsafe fn sys_task_status(task_id: u64) -> u64 {
    unsafe { syscall1(sys::SYS_TASK_STATUS, task_id) }
}

/// Fill TaskInfo buffer with live tasks. Returns entry count.
#[inline(always)]
pub unsafe fn sys_task_list(buf_ptr: u64, capacity: u64) -> u64 {
    unsafe { syscall2(sys::SYS_TASK_LIST, buf_ptr, capacity) }
}

/// Return free 4 KiB frame count.
#[inline(always)]
pub unsafe fn sys_mem_stat() -> u64 {
    unsafe { syscall0(sys::SYS_MEM_STAT) }
}

/// Terminate a task by ID. Returns 0 or error.
#[inline(always)]
pub unsafe fn sys_task_kill(task_id: u64) -> u64 {
    unsafe { syscall1(sys::SYS_TASK_KILL, task_id) }
}

/// Block until task exits. Returns 0.
#[inline(always)]
pub unsafe fn sys_task_wait(task_id: u64) -> u64 {
    unsafe { syscall1(sys::SYS_TASK_WAIT, task_id) }
}

/// Fill PsEntry buffer with extended task info. Returns entry count.
#[inline(always)]
pub unsafe fn sys_ps(buf_ptr: u64, capacity: u64) -> u64 {
    unsafe { syscall2(sys::SYS_PS, buf_ptr, capacity) }
}

/// Set task scheduling priority. Returns 0 or error.
#[inline(always)]
pub unsafe fn sys_task_setpriority(task_id: u64, priority: u64) -> u64 {
    unsafe { syscall2(sys::SYS_TASK_SETPRIORITY, task_id, priority) }
}

/// Set/query heap break. Returns new break address.
#[inline(always)]
pub unsafe fn sys_brk(new_brk: u64) -> u64 {
    unsafe { syscall1(sys::SYS_BRK, new_brk) }
}

/// Read a 512-byte sector from the block device. Returns 0 or error.
#[inline(always)]
pub unsafe fn sys_blk_read(sector: u64, buf_ptr: u64) -> u64 {
    unsafe { syscall2(sys::SYS_BLK_READ, sector, buf_ptr) }
}

/// Write a 512-byte sector to the block device. Returns 0 or error.
#[inline(always)]
pub unsafe fn sys_blk_write(sector: u64, buf_ptr: u64) -> u64 {
    unsafe { syscall2(sys::SYS_BLK_WRITE, sector, buf_ptr) }
}

/// Open a file. Returns fd (≥0) or error.
#[inline(always)]
pub unsafe fn sys_open(path_ptr: u64, path_len: u64) -> u64 {
    unsafe { syscall2(sys::SYS_OPEN, path_ptr, path_len) }
}

/// Read from fd. Returns bytes read or error.
#[inline(always)]
pub unsafe fn sys_read(fd: u64, buf_ptr: u64, len: u64) -> u64 {
    unsafe { syscall3(sys::SYS_READ, fd, buf_ptr, len) }
}

/// Write to a writable fd. Returns bytes written or error.
#[inline(always)]
pub unsafe fn sys_write(fd: u64, buf_ptr: u64, len: u64) -> u64 {
    unsafe { syscall3(sys::SYS_WRITE, fd, buf_ptr, len) }
}

/// Close fd. Returns 0.
#[inline(always)]
pub unsafe fn sys_close(fd: u64) -> u64 {
    unsafe { syscall1(sys::SYS_CLOSE, fd) }
}

/// Stat a path into a 48-byte buffer. Returns 0 or error.
#[inline(always)]
pub unsafe fn sys_stat(path_ptr: u64, path_len: u64, stat_buf: u64) -> u64 {
    unsafe { syscall3(sys::SYS_STAT, path_ptr, path_len, stat_buf) }
}

/// Read directory entries. Returns entry count or error.
#[inline(always)]
pub unsafe fn sys_readdir(path_ptr: u64, path_len: u64, buf_ptr: u64, buf_len: u64) -> u64 {
    unsafe { syscall4(sys::SYS_READDIR, path_ptr, path_len, buf_ptr, buf_len) }
}

/// Create a new empty file. Returns writable fd or error.
#[inline(always)]
pub unsafe fn sys_create(path_ptr: u64, path_len: u64) -> u64 {
    unsafe { syscall2(sys::SYS_CREATE, path_ptr, path_len) }
}

/// Delete a file. Returns 0 or error.
#[inline(always)]
pub unsafe fn sys_unlink(path_ptr: u64, path_len: u64) -> u64 {
    unsafe { syscall2(sys::SYS_UNLINK, path_ptr, path_len) }
}

/// Create a directory. Returns 0 or error.
#[inline(always)]
pub unsafe fn sys_mkdir(path_ptr: u64, path_len: u64) -> u64 {
    unsafe { syscall2(sys::SYS_MKDIR, path_ptr, path_len) }
}

/// Rename/move a file. Returns 0 or error.
#[inline(always)]
pub unsafe fn sys_rename(old_ptr: u64, old_len: u64, new_ptr: u64, new_len: u64) -> u64 {
    unsafe { syscall4(sys::SYS_RENAME, old_ptr, old_len, new_ptr, new_len) }
}

/// Seek within an open fd. Returns new offset or error.
#[inline(always)]
pub unsafe fn sys_seek(fd: u64, offset: u64, whence: u64) -> u64 {
    unsafe { syscall3(sys::SYS_SEEK, fd, offset, whence) }
}

/// Non-blocking IPC recv. Returns bytes or EAGAIN.
#[inline(always)]
pub unsafe fn sys_ipc_poll(cap: u64, buf_ptr: u64, buf_len: u64) -> u64 {
    unsafe { syscall3(sys::SYS_IPC_POLL, cap, buf_ptr, buf_len) }
}

/// Bind a name to an IPC endpoint. Returns 0 or error.
#[inline(always)]
pub unsafe fn sys_ipc_bind(cap: u64, name_ptr: u64, name_len: u64) -> u64 {
    unsafe { syscall3(sys::SYS_IPC_BIND, cap, name_ptr, name_len) }
}

/// Look up a named IPC endpoint. Returns new handle or error.
#[inline(always)]
pub unsafe fn sys_ipc_lookup(name_ptr: u64, name_len: u64, rights: u64) -> u64 {
    unsafe { syscall3(sys::SYS_IPC_LOOKUP, name_ptr, name_len, rights) }
}

/// Blocking IPC recv with timeout. Returns bytes or EAGAIN.
#[inline(always)]
pub unsafe fn sys_ipc_recv_timeout(cap: u64, buf_ptr: u64, buf_len: u64, timeout_ms: u64) -> u64 {
    unsafe { syscall4(sys::SYS_IPC_RECV_TIMEOUT, cap, buf_ptr, buf_len, timeout_ms) }
}

/// Blocking IPC send with timeout. Returns 0 or EAGAIN.
#[inline(always)]
pub unsafe fn sys_ipc_send_timeout(cap: u64, msg_ptr: u64, msg_len: u64, timeout_ms: u64) -> u64 {
    unsafe { syscall4(sys::SYS_IPC_SEND_TIMEOUT, cap, msg_ptr, msg_len, timeout_ms) }
}

/// Create a UDP socket. Returns fd or ENOSYS.
#[inline(always)]
pub unsafe fn sys_socket() -> u64 {
    unsafe { syscall0(sys::SYS_SOCKET) }
}

/// Bind a socket to a local UDP port. Returns 0 or error.
#[inline(always)]
pub unsafe fn sys_bind(fd: u64, port: u64) -> u64 {
    unsafe { syscall2(sys::SYS_BIND, fd, port) }
}

/// Send a UDP datagram. Returns 0 or error.
#[inline(always)]
pub unsafe fn sys_sendto(fd: u64, buf_ptr: u64, len: u64, dst_ip: u64, dst_port: u64) -> u64 {
    unsafe { syscall5(sys::SYS_SENDTO, fd, buf_ptr, len, dst_ip, dst_port) }
}

/// Receive a UDP datagram (blocking). Returns bytes received.
#[inline(always)]
pub unsafe fn sys_recvfrom(
    fd: u64, buf_ptr: u64, len: u64, src_ip_out: u64, src_port_out: u64,
) -> u64 {
    unsafe { syscall5(sys::SYS_RECVFROM, fd, buf_ptr, len, src_ip_out, src_port_out) }
}

/// Close a socket or fd. Returns 0 or error.
#[inline(always)]
pub unsafe fn sys_net_close(fd: u64) -> u64 {
    unsafe { syscall1(sys::SYS_NET_CLOSE, fd) }
}

/// Power off the machine (does not return).
#[inline(always)]
pub unsafe fn sys_poweroff() -> ! {
    unsafe { syscall0(sys::SYS_POWEROFF) };
    unreachable!()
}
