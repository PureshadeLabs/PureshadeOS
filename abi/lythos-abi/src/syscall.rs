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

// ── Networking (RETIRED) ──────────────────────────────────────────────────────
//
// Numbers 50-54 are RETIRED, permanently-unassigned gaps. They were the
// in-kernel UDP socket API: 50 SYS_SOCKET, 51 SYS_BIND, 52 SYS_SENDTO,
// 53 SYS_RECVFROM, 54 SYS_NET_CLOSE. The in-kernel virtio-net driver + TCP/IP
// stack were torn down; networking is now a userspace concern — a `netd`
// driver owns the virtio-net device via a CapKind::Device capability over the
// userspace device-driver framework (SYS_DEV_* below), and higher-level
// protocol access goes through IPC to that daemon, not these syscalls. Do NOT
// reuse 50-54: an old socket-API caller must fail ENOSYS, not hit an unrelated
// handler (same rationale as gaps 49 and 59).

// ── Power management ──────────────────────────────────────────────────────────

/// Power off the machine (ACPI S5). Does not return.
pub const SYS_POWEROFF:          u64 = 55;

// ── Mount management ──────────────────────────────────────────────────────────

/// Mount a filesystem backend at a path. Requires a Filesystem capability
/// with WRITE right (no ambient authority).
/// a1=at_ptr, a2=at_len (mount point, UTF-8), a3=source (MOUNT_SRC_*),
/// a4=flags (MOUNT_* bitfield). Returns 0 or negative errno
/// (ENOPERM, EMOUNTED, ENOENT, ENOTDIR, EINVAL, …).
pub const SYS_MOUNT:             u64 = 56;

/// SYS_MOUNT a3: fresh RAM-backed RFS V2 instance, formatted at mount time.
/// Volatile — content is lost on power cycle. Warm-only; use for scratch.
pub const MOUNT_SRC_RFS2_RAM:    u64 = 0;

/// SYS_MOUNT a3: persistent RFS V2 instance on the secondary virtio-blk
/// device (a fixed backing disk image, e.g. `store.img`). Mounted if already
/// formatted, formatted-then-mounted on first ever boot — content survives a
/// full power cycle. This is what backs `/shade/store` so a cold boot brings
/// up a generation whose closure still exists on disk.
pub const MOUNT_SRC_RFS2_BLK:    u64 = 1;

/// SYS_MOUNT a4 flag: attach the realize-guard (read-only-after-realize
/// store semantics) to this mount.
pub const MOUNT_STORE:           u64 = 1 << 0;

// ── Symlinks ──────────────────────────────────────────────────────────────────

/// Create a symbolic link at `link` pointing at `target` (target is stored
/// verbatim, not resolved or validated — dangling links are legal). The final
/// component of `link` is not followed; parent components are. On a store
/// (realize-guarded) mount, creating a link inside a sealed entry is EROFS.
/// a1=target_ptr, a2=target_len, a3=link_ptr, a4=link_len. Returns 0 or
/// negative errno (EEXIST, ENOENT, ENOTDIR, EROFS, EINVAL, …).
pub const SYS_SYMLINK:           u64 = 57;
/// Read a symlink's target. The final component is not followed (it must BE
/// the symlink); parent components are. a1=path_ptr, a2=path_len,
/// a3=buf_ptr, a4=buf_len. Returns target length in bytes (truncated to
/// buf_len if shorter) or negative errno (ENOENT, EINVAL if not a symlink).
pub const SYS_READLINK:          u64 = 58;

// ── Store reclamation ─────────────────────────────────────────────────────────
//
// Number 59 is a RETIRED, PERMANENTLY UNASSIGNED gap. It was `SYS_UNSEAL`,
// which lifted a realize-seal to make sealed store content writable again for
// GC. That primitive is GONE by design: the seal must be ABSOLUTE — no syscall
// may ever make sealed content mutable. GC reclaims space by removing whole
// unreferenced paths (SYS_STORE_REMOVE below), never by reopening sealed files
// for write. Do NOT reuse 59: leaving it a hole is deliberate so an old caller
// that still emits SYS_UNSEAL fails ENOSYS instead of silently invoking an
// unrelated, possibly destructive, handler. (Same rationale as gap 49.)

/// Remove an entire unreferenced store path (whole-tree delete: unlink every
/// file, rmdir every directory, free the blocks via RFS2 free-space) —
/// operating BELOW the seal layer, so it never opens a sealed file for write.
/// This is the sole store-reclamation primitive after SYS_UNSEAL's removal.
/// Requires a Filesystem capability with WRITE right (store-owner authority —
/// a builder cannot reclaim). Content-addressing keeps it safe: a later
/// realize of the same digest reproduces byte-identical content. `path` must
/// name a top-level entry on a store (realize-guarded) mount; the kernel drops
/// that name's in-kernel seal as part of the lifecycle removal.
/// a1=path_ptr, a2=path_len. Returns 0 or negative errno
/// (ENOPERM, EINVAL, ENOMNT, ENOENT, …).
pub const SYS_STORE_REMOVE:      u64 = 60;

// ── Userspace device-driver framework ─────────────────────────────────────────
//
// Give a ring-3 driver ownership of one PCI device it holds a CapKind::Device
// capability for. All authority flows through that unforgeable cap: the driver
// can touch only the device the cap names — its config space, its BARs, its
// IRQ, and DMA buffers minted for it. No ambient authority: a process with no
// Device cap cannot map MMIO, wait on an IRQ, or allocate DMA (gate-before-args
// → ENOPERM, like the SYS_MOUNT gate).
//
// DMA trust model: without an IOMMU a DMA-programming driver can address any
// physical memory, so a Device-cap holder is trusted-for-DMA for now. DMA
// buffers are nonetheless allocated THROUGH the framework (SYS_DEV_DMA_ALLOC)
// so an IOMMU domain can later be programmed at that single chokepoint.

/// Claim a PCI device from the kernel registry by name, minting a Device
/// capability into the caller's table. Gated on the Rollback capability
/// (lythd-exclusive) so only init claims devices; init then delegates each
/// device cap to its driver. A device may be claimed at most once.
/// a1=name_ptr, a2=name_len (UTF-8). Returns a CapHandle or negative errno
/// (ENOPERM if no Rollback cap, ENOENT if no such device, EINVAL on bad args or
/// a device already claimed).
pub const SYS_DEV_CLAIM:         u64 = 61;
/// Read one 32-bit dword from the named device's PCI config space. Lets a
/// userspace driver walk the modern virtio-pci capability list (and any other
/// config-space structure) without needing port-I/O access. Device-cap gated.
/// a1=dev_cap, a2=offset (dword-aligned, < 256). Returns the u32 value or
/// ENOPERM / EINVAL.
pub const SYS_DEV_CFG_READ:      u64 = 62;
/// Map a device MMIO BAR into the caller's address space, uncacheable, gated on
/// the Device cap. The whole BAR region is mapped starting at `virt`.
/// a1=dev_cap, a2=bar_index (0..6), a3=virt (page-aligned user addr). Returns
/// the BAR length in bytes or ENOPERM / EINVAL.
pub const SYS_DEV_MMIO_MAP:      u64 = 63;
/// Allocate a physically-contiguous, zeroed DMA buffer, map it into the caller
/// at `virt`, and return its physical address (to program into the device).
/// Wiped on free. Device-cap gated. a1=dev_cap, a2=virt (page-aligned),
/// a3=size (bytes, rounded up to whole pages), a4=out_phys_ptr (*mut u64).
/// Returns 0 or ENOPERM / EINVAL.
pub const SYS_DEV_DMA_ALLOC:     u64 = 64;
/// Block the calling driver until its device raises an IRQ. The kernel masks
/// the device's IOAPIC line on fire (so a level-triggered PCI line does not
/// storm before the driver clears the device's ISR); the driver must call
/// SYS_DEV_IRQ_ACK after servicing to resume delivery. Device-cap gated.
/// a1=dev_cap. Returns 0 or ENOPERM.
pub const SYS_DEV_IRQ_WAIT:      u64 = 65;
/// Acknowledge and unmask the device IRQ after servicing (the driver reads the
/// device ISR register to deassert the level line first). Device-cap gated.
/// a1=dev_cap. Returns 0 or ENOPERM.
pub const SYS_DEV_IRQ_ACK:       u64 = 66;

// ── Bookkeeping ───────────────────────────────────────────────────────────────

/// Highest assigned syscall number. Calls above this return ENOSYS.
/// Numbers 49, 50-54 and 59 are unassigned gaps and also return ENOSYS
/// (59 = retired SYS_UNSEAL, 50-54 = retired UDP socket API — never reuse them).
pub const SYSCALL_MAX:           u64 = SYS_DEV_IRQ_ACK;
