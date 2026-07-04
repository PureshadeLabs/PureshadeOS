//! Syscall numbers — transcribed from `docs/spec/syscalls.md`.
//!
//! Source of truth: the spec table (Nr column).
//! Cross-checked against `kernel/src/syscall.rs` — all 42 numbers match.
//!
//! Gap: 49 is unassigned and always returns ENOSYS.
//! SYSCALL_MAX = 55 (SYS_POWEROFF).

// ── Process / task management ─────────────────────────────────────────────────

/// Voluntarily yield the CPU to the scheduler.
pub const SYS_YIELD:             u64 = 0;
/// Terminate the calling task (does not return).
pub const SYS_TASK_EXIT:         u64 = 1;
/// Map anonymous physical frames into the calling task's address space.
/// a1=virt, a2=phys, a3=flags. Requires Memory capability.
pub const SYS_MMAP:              u64 = 2;
/// Unmap a virtual page from the calling task's address space.
/// a1=virt.
pub const SYS_MUNMAP:            u64 = 3;
/// Privileged system reset. Requires Rollback capability (granted only to lythd).
pub const SYS_ROLLBACK:          u64 = 9;
/// Load an ELF image from user memory and spawn a new userspace task.
/// a1=elf_ptr, a2=elf_len, a3=cap_handle (cap to transfer to child).
pub const SYS_EXEC:              u64 = 10;
/// Write a UTF-8 string to the kernel serial console (debug).
/// a1=str_ptr, a2=str_len.
pub const SYS_LOG:               u64 = 11;

// ── Capability management ──────────────────────────────────────────────────────

/// Grant a derived capability with reduced rights to another task.
/// a1=handle, a2=target_task_id, a3=rights_mask (u8 in low bits).
/// Returns the new handle index in the recipient's table.
pub const SYS_CAP_GRANT:         u64 = 4;
/// Cascade-revoke a capability from this task and all derived holders.
/// a1=handle.
pub const SYS_CAP_REVOKE:        u64 = 5;

// ── IPC ───────────────────────────────────────────────────────────────────────

/// Send a message to an IPC endpoint (blocking if full).
/// a1=ipc_cap, a2=msg_ptr, a3=msg_len.
pub const SYS_IPC_SEND:          u64 = 6;
/// Receive a message from an IPC endpoint (blocking if empty).
/// a1=ipc_cap, a2=buf_ptr, a3=buf_len. Returns bytes received.
pub const SYS_IPC_RECV:          u64 = 7;
/// Create a new IPC endpoint. Returns new capability handle.
pub const SYS_IPC_CREATE:        u64 = 8;
/// Send a message and transfer a capability in one atomic operation.
/// a1=ipc_cap, a2=msg_ptr, a3=msg_len, a4=cap_handle_to_send.
pub const SYS_IPC_SEND_CAP:      u64 = 12;
/// Receive a message and accept an in-flight capability.
/// a1=ipc_cap, a2=buf_ptr, a3=buf_len, a4=out_handle_ptr (*mut u64).
/// *a4 = new handle if a cap was transferred, u64::MAX otherwise.
pub const SYS_IPC_RECV_CAP:      u64 = 13;
/// Non-blocking receive; returns EAGAIN immediately if ring is empty.
/// a1=ipc_cap, a2=buf_ptr, a3=buf_len.
pub const SYS_IPC_POLL:          u64 = 39;
/// Bind a name to an IPC endpoint (global registry, requires GRANT right).
/// a1=ipc_cap, a2=name_ptr, a3=name_len (max 128 bytes UTF-8).
pub const SYS_IPC_BIND:          u64 = 40;
/// Look up a named IPC endpoint; returns new handle with requested rights.
/// a1=name_ptr, a2=name_len, a3=rights_mask. Returns handle or ENOENT.
pub const SYS_IPC_LOOKUP:        u64 = 41;
/// Blocking IPC recv with millisecond timeout.
/// a1=ipc_cap, a2=buf_ptr, a3=buf_len, a4=timeout_ms. Returns bytes or EAGAIN.
pub const SYS_IPC_RECV_TIMEOUT:  u64 = 42;
/// Blocking IPC send with millisecond timeout.
/// a1=ipc_cap, a2=msg_ptr, a3=msg_len, a4=timeout_ms. Returns 0 or EAGAIN.
pub const SYS_IPC_SEND_TIMEOUT:  u64 = 43;

// ── Serial I/O ────────────────────────────────────────────────────────────────

/// Read bytes from COM1 serial port into a user buffer (blocks until ≥1 byte).
/// a1=buf_ptr, a2=buf_len. Returns bytes read.
pub const SYS_SERIAL_READ:       u64 = 14;
/// Check whether COM1 has data available without consuming it.
/// Returns 1 if data ready, 0 if not.
pub const SYS_SERIAL_AVAIL:      u64 = 30;

// ── Time ──────────────────────────────────────────────────────────────────────

/// Milliseconds elapsed since kernel boot (APIC tick counter).
pub const SYS_TIME:              u64 = 15;
/// Unix epoch milliseconds since 1970-01-01 00:00:00 UTC.
/// Anchored from CMOS RTC at boot; advances via APIC tick counter.
pub const SYS_TIME_EPOCH:        u64 = 44;
/// Return the UID of the calling task.
pub const SYS_GETUID:            u64 = 45;
/// Return the GID of the calling task.
pub const SYS_GETGID:            u64 = 46;
/// Set the UID of the calling task. Root (uid=0) may set any uid; others may only set own uid.
/// Returns 0 on success, ENOPERM if not permitted.
pub const SYS_SETUID:            u64 = 47;
/// Set the GID of the calling task. Root may set any gid; others may only set own gid.
/// Returns 0 on success, ENOPERM if not permitted.
pub const SYS_SETGID:            u64 = 48;
/// Sleep for at least a1 nanoseconds (rounded up to next ms tick).
pub const SYS_NANOSLEEP:         u64 = 33;

// ── Task introspection ────────────────────────────────────────────────────────

/// Query task liveness by ID.
/// Returns 0=dead/missing, 1=running, 2=ready, 3=blocked.
pub const SYS_TASK_STATUS:       u64 = 16;
/// Fill a buffer with TaskInfo structs for all live tasks.
/// a1=buf_ptr (*mut TaskInfo), a2=buf_capacity. Returns entries written.
pub const SYS_TASK_LIST:         u64 = 17;
/// Return free 4 KiB frame count as u64.
pub const SYS_MEM_STAT:          u64 = 18;
/// Terminate a task by ID.
/// a1=task_id. Returns 0 or EINVAL.
pub const SYS_TASK_KILL:         u64 = 19;
/// Block the calling task until task a1 exits (returns 0 immediately if dead).
pub const SYS_TASK_WAIT:         u64 = 31;
/// List tasks with extended info (name, priority) into PsEntry structs.
/// a1=buf_ptr (*mut PsEntry), a2=buf_capacity. Returns entries written.
pub const SYS_PS:                u64 = 37;
/// Set scheduling priority for a task.
/// a1=task_id, a2=priority (0=low, 1=normal, 2=high). Returns 0 or EINVAL.
pub const SYS_TASK_SETPRIORITY:  u64 = 34;

// ── Memory management ─────────────────────────────────────────────────────────

/// Set or query the program break (heap top).
/// a1=new_brk (0 = query). Returns new/current break address.
pub const SYS_BRK:               u64 = 38;

// ── Block device ──────────────────────────────────────────────────────────────

/// Read one 512-byte sector from the VirtIO block device.
/// a1=sector (u64), a2=buf_ptr (user *mut u8, 512 bytes).
pub const SYS_BLK_READ:          u64 = 20;
/// Write one 512-byte sector to the VirtIO block device.
/// a1=sector (u64), a2=buf_ptr (user *const u8, 512 bytes).
pub const SYS_BLK_WRITE:         u64 = 21;

// ── Filesystem (RFS VFS) ──────────────────────────────────────────────────────

/// Open a file. a1=path_ptr, a2=path_len. Returns fd or error.
pub const SYS_OPEN:              u64 = 22;
/// Read from fd. a1=fd, a2=buf_ptr, a3=len. Returns bytes read or error.
pub const SYS_READ:              u64 = 23;
/// Write to a writable fd. a1=fd, a2=buf_ptr, a3=len. Returns bytes written or error.
pub const SYS_WRITE:             u64 = 24;
/// Close fd. a1=fd.
pub const SYS_CLOSE:             u64 = 25;
/// Stat a path. a1=path_ptr, a2=path_len, a3=stat_buf_ptr (48 bytes).
pub const SYS_STAT:              u64 = 26;
/// Read directory entries. a1=path_ptr, a2=path_len, a3=buf_ptr, a4=buf_len.
/// Returns entry count or error.
pub const SYS_READDIR:           u64 = 27;
/// Create a new empty file and return a writable fd.
/// a1=path_ptr, a2=path_len. Returns fd or error.
pub const SYS_CREATE:            u64 = 28;
/// Delete a file. a1=path_ptr, a2=path_len. Returns 0 or error.
pub const SYS_UNLINK:            u64 = 29;
/// Create a directory. a1=path_ptr, a2=path_len. Returns 0 or error.
pub const SYS_MKDIR:             u64 = 32;
/// Rename/move a file. a1=old_path_ptr, a2=old_len, a3=new_path_ptr, a4=new_len.
pub const SYS_RENAME:            u64 = 35;
/// Seek within an open fd. a1=fd, a2=offset (i64 as u64), a3=whence (0=SET,1=CUR,2=END).
/// Returns new offset or error.
pub const SYS_SEEK:              u64 = 36;

// ── UDP networking ────────────────────────────────────────────────────────────

/// Create a UDP socket. Returns socket fd or ENOSYS if stack absent.
pub const SYS_SOCKET:            u64 = 50;
/// Bind a socket to a local UDP port. a1=socket_fd, a2=port (u16).
pub const SYS_BIND:              u64 = 51;
/// Send a UDP datagram. a1=fd, a2=buf_ptr, a3=len, a4=dst_ip (u32 BE), a5=dst_port (u16).
pub const SYS_SENDTO:            u64 = 52;
/// Receive a UDP datagram (blocking). a1=fd, a2=buf_ptr, a3=len,
/// a4=src_ip_out (*mut u32), a5=src_port_out (*mut u16). Returns bytes.
pub const SYS_RECVFROM:          u64 = 53;
/// Close a socket or file descriptor. a1=fd.
pub const SYS_NET_CLOSE:         u64 = 54;

// ── Power management ──────────────────────────────────────────────────────────

/// Power off the machine (ACPI S5). Does not return.
pub const SYS_POWEROFF:          u64 = 55;

// ── Bookkeeping ───────────────────────────────────────────────────────────────

/// Highest assigned syscall number. Calls above this return ENOSYS.
/// Number 49 is an unassigned gap and also returns ENOSYS.
pub const SYSCALL_MAX:           u64 = SYS_POWEROFF;
