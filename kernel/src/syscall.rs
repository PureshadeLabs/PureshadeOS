/// Syscall interface — `syscall`/`sysretq` entry stub, dispatch, and
/// `enter_userspace`.
///
/// ## Syscall ABI
///
/// | Register | Role on entry          | Role on return |
/// |----------|------------------------|----------------|
/// | RAX      | syscall number         | return value   |
/// | RDI      | argument 1             |                |
/// | RSI      | argument 2             |                |
/// | RDX      | argument 3             |                |
/// | R10      | argument 4 (RCX clobbered by `syscall`) | |
/// | R8       | argument 5             |                |
/// | R9       | argument 6             |                |
/// | RCX      | user RIP (saved by CPU)|                |
/// | R11      | user RFLAGS (saved by CPU) |            |
///
/// ## Stack switch
///
/// The `syscall` instruction does not switch stacks.  `syscall_entry` saves
/// the user RSP in `SYSCALL_USER_RSP` and loads the kernel RSP from
/// `SYSCALL_KERN_RSP`.  `enter_userspace` sets both of these (plus
/// `tss::RSP0`) to the current task's kernel-stack top before `iretq`.
///
/// ## Syscall numbers
///
/// | Nr | Name            |
/// |----|-----------------|
/// |  0 | SYS_YIELD       |
/// |  1 | SYS_TASK_EXIT   |
/// |  2 | SYS_MMAP        |
/// |  3 | SYS_MUNMAP      |
/// |  4 | SYS_CAP_GRANT   |
/// |  5 | SYS_CAP_REVOKE  |
/// |  6 | SYS_IPC_SEND    |
/// |  7 | SYS_IPC_RECV    |
/// |  8 | SYS_IPC_CREATE  |
/// |  9 | SYS_ROLLBACK    |
/// | 10 | SYS_EXEC        |
/// | 11 | SYS_LOG         |
/// | 12 | SYS_IPC_SEND_CAP |
/// | 13 | SYS_IPC_RECV_CAP |
/// | 14 | SYS_SERIAL_READ  |
/// | 15 | SYS_TIME         |
/// | 16 | SYS_TASK_STATUS  |
/// | 17 | SYS_TASK_LIST   |
/// | 18 | SYS_MEM_STAT    |
/// | 19 | SYS_TASK_KILL   |
/// | 20 | SYS_BLK_READ    |
/// | 21 | SYS_BLK_WRITE   |
/// | 30 | SYS_SERIAL_AVAIL|
/// | 31 | SYS_TASK_WAIT   |
/// | 22 | SYS_OPEN        |
/// | 23 | SYS_READ        |
/// | 24 | SYS_WRITE       |
/// | 25 | SYS_CLOSE       |
/// | 26 | SYS_STAT        |
/// | 27 | SYS_READDIR     |
/// | 28 | SYS_CREATE      |
/// | 29 | SYS_UNLINK      |
/// | 32 | SYS_MKDIR       |
/// | 35 | SYS_RENAME      |
/// | 36 | SYS_SEEK        |
/// | 37 | SYS_PS          |
/// | 38 | SYS_BRK         |
/// | 39 | SYS_IPC_POLL    |
/// | 40 | SYS_IPC_BIND    |
/// | 41 | SYS_IPC_LOOKUP  |
/// | 42 | SYS_IPC_RECV_TIMEOUT |
/// | 43 | SYS_IPC_SEND_TIMEOUT |
/// | 44 | SYS_TIME_EPOCH  |
/// | 45 | SYS_GETUID      |
/// | 46 | SYS_GETGID      |
/// | 47 | SYS_SETUID      |
/// | 48 | SYS_SETGID      |

use core::arch::global_asm;

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
/// Privileged system reset.  Requires `CapKind::Rollback`.  Granted only to `lythd`.
pub const SYS_ROLLBACK:   u64 = 9;
/// Exec a new userspace process from an ELF blob in user memory.
pub const SYS_EXEC:       u64 = 10;
/// Write a UTF-8 string to the kernel serial console.  Debug aid only.
pub const SYS_LOG:        u64 = 11;
/// Send a message **and** transfer a capability over an IPC endpoint.
/// a1=ipc_cap_handle, a2=msg_ptr, a3=msg_len, a4=cap_handle_to_send
pub const SYS_IPC_SEND_CAP: u64 = 12;
/// Receive a message **and** accept any in-flight capability from an endpoint.
/// a1=ipc_cap_handle, a2=buf_ptr, a3=buf_len, a4=out_handle_ptr (user *mut u64)
/// Returns bytes received; writes new CapHandle to *out_handle_ptr (u64::MAX if none).
pub const SYS_IPC_RECV_CAP: u64 = 13;
/// Read bytes from the COM1 serial port into a user buffer.
/// a1=buf_ptr (user VA), a2=buf_len.
/// Blocks (yielding the CPU) until at least one byte is available, then
/// reads as many bytes as are ready (up to buf_len).  Returns bytes read.
pub const SYS_SERIAL_READ:  u64 = 14;
/// Return milliseconds elapsed since kernel boot (APIC tick counter).
/// No arguments.  Always succeeds; return value is a `u64` millisecond count.
pub const SYS_TIME:         u64 = 15;
/// Return the liveness status of a task by ID.
/// a1 = TaskId.
/// Returns: 0 = not found / dead, 1 = running or ready, 2 = blocked.
pub const SYS_TASK_STATUS:  u64 = 16;
/// Fill a user buffer with TaskInfo structs (one per live task).
/// a1 = buf_ptr (user VA, *mut TaskInfo), a2 = buf_capacity (max entries).
/// Returns number of entries written.  Each entry is 24 bytes:
///   offset  0: u64  task_id
///   offset  8: u64  state  (1=running, 2=ready, 3=blocked)
///   offset 16: u8   kind   (0=kernel, 1=userspace)
///   offset 17: [u8; 7] pad
pub const SYS_TASK_LIST:    u64 = 17;
/// Return physical memory statistics.
/// No arguments.  Returns free 4 KiB frame count as a u64.
pub const SYS_MEM_STAT:     u64 = 18;
/// Terminate a task by ID.
/// a1 = TaskId.
/// Returns 0 on success, EINVAL if not found / already dead / protected task.
pub const SYS_TASK_KILL:    u64 = 19;
/// Check whether keyboard or serial data is ready to read without blocking.
/// No arguments. Returns 1 if data is available, 0 if not.
pub const SYS_SERIAL_AVAIL: u64 = 30;
/// Block the calling task until task `a1` (TaskId) exits.
/// Returns 0 immediately if the target is not found or already Dead.
pub const SYS_TASK_WAIT:    u64 = 31;
pub const SYS_BLK_READ:     u64 = 20;
/// Write one 512-byte sector from a user buffer to the VirtIO block device.
/// a1 = sector (u64), a2 = buf_ptr (user VA, *const u8, must be 512 bytes).
/// Returns 0 on success; ENOSYS if no block device; EINVAL on bad args or I/O error.
pub const SYS_BLK_WRITE:    u64 = 21;
/// Open a file. a1=path_ptr, a2=path_len. Returns fd (≥ 0) or error.
pub const SYS_OPEN:         u64 = 22;
/// Read from fd. a1=fd, a2=buf_ptr, a3=len. Returns bytes read or error.
pub const SYS_READ:         u64 = 23;
/// Write to an open writable fd. a1=fd, a2=buf_ptr, a3=len. Returns bytes written or error.
pub const SYS_WRITE:        u64 = 24;
/// Close fd. a1=fd. Returns 0 or error.
pub const SYS_CLOSE:        u64 = 25;
/// Stat a path. a1=path_ptr, a2=path_len, a3=stat_ptr. Returns 0 or error.
pub const SYS_STAT:         u64 = 26;
/// Readdir. a1=path_ptr, a2=path_len, a3=buf_ptr, a4=buf_len. Returns entry count or error.
pub const SYS_READDIR:      u64 = 27;
/// Create a new empty regular file. a1=path_ptr, a2=path_len. Returns writable fd or error.
pub const SYS_CREATE:       u64 = 28;
/// Delete a file. a1=path_ptr, a2=path_len. Returns 0 or error.
pub const SYS_UNLINK:       u64 = 29;
/// Create a directory. a1=path_ptr, a2=path_len. Returns 0 or error.
pub const SYS_MKDIR:        u64 = 32;
/// Rename/move a regular file. a1=old_path_ptr, a2=old_path_len, a3=new_path_ptr, a4=new_path_len. Returns 0 or error.
pub const SYS_RENAME:       u64 = 35;
/// Seek within an open fd. a1=fd, a2=offset (i64 as u64), a3=whence (0=SET,1=CUR,2=END). Returns new offset or error.
pub const SYS_SEEK:         u64 = 36;
/// List tasks with extended info. a1=buf_ptr (*mut PsEntry), a2=buf_capacity.
/// Each entry is 48 bytes: u64 id | u64 state | u8 kind | u8 priority | u8 name_len | [u8;5] pad | [u8;16] name | [u8;8] pad.
/// Returns entry count written.
pub const SYS_PS:           u64 = 37;
/// Sleep for at least `a1` nanoseconds (rounded up to the next millisecond).
/// Blocks the calling task; returns 0 when the deadline has passed.
pub const SYS_NANOSLEEP:    u64 = 33;
/// Set or query the program break (heap top) for the calling task.
/// a1 = new break address (0 = query current break).
/// Returns the new break address on success.  On OOM, returns the highest
/// address actually mapped (partial success).  EINVAL if new_brk >= stack area.
pub const SYS_BRK:          u64 = 38;
/// Non-blocking receive from an IPC endpoint.
/// a1=ipc_cap_handle, a2=buf_ptr, a3=buf_len.
/// Returns bytes received, or EAGAIN if the ring is empty.
pub const SYS_IPC_POLL:     u64 = 39;
/// Bind a name to an IPC endpoint so peers can look it up without hard-coded handles.
/// a1=ipc_cap_handle, a2=name_ptr, a3=name_len (max 128 bytes).
/// Requires GRANT right on the cap.  Returns 0 on success, ENOSYS if name taken.
pub const SYS_IPC_BIND:     u64 = 40;
/// Look up a named IPC endpoint and obtain a new cap handle with READ|WRITE rights.
/// a1=name_ptr, a2=name_len, a3=rights_mask (bits: READ=1 WRITE=2; GRANT/REVOKE stripped).
/// Returns new CapHandle on success, ENOENT if name not registered.
pub const SYS_IPC_LOOKUP:   u64 = 41;
/// Set scheduling priority for task `a1` (TaskId): a2=priority (0=low, 1=normal, 2=high).
/// Returns 0 on success, EINVAL if task not found or priority out of range.
pub const SYS_TASK_SETPRIORITY: u64 = 34;
/// Receive from IPC endpoint with timeout.
/// a1=ipc_cap_handle, a2=buf_ptr, a3=buf_len, a4=timeout_ms.
/// Returns bytes received, or EAGAIN if timeout expires before a message arrives.
pub const SYS_IPC_RECV_TIMEOUT: u64 = 42;
/// Send to IPC endpoint with timeout.
/// a1=ipc_cap_handle, a2=msg_ptr, a3=msg_len, a4=timeout_ms.
/// Returns 0 on success, or EAGAIN if timeout expires before the ring drains.
pub const SYS_IPC_SEND_TIMEOUT: u64 = 43;
/// Return Unix epoch milliseconds (ms since 1970-01-01 00:00:00 UTC).
/// No arguments.  Anchored from CMOS RTC at boot; advances via APIC tick counter.
/// Never returns an error sentinel; always a valid u64.
pub const SYS_TIME_EPOCH: u64 = 44;
/// Return the UID of the calling task. Returns u32 (always 0 until SYS_SETUID exists).
pub const SYS_GETUID:   u64 = 45;
/// Return the GID of the calling task. Returns u32.
pub const SYS_GETGID:   u64 = 46;
/// Set the UID of the calling task (root only, or setting own uid). Returns 0 or ENOPERM.
pub const SYS_SETUID:   u64 = 47;
/// Set the GID of the calling task (root only, or setting own gid). Returns 0 or ENOPERM.
pub const SYS_SETGID:   u64 = 48;
// Numbers 50-54 are RETIRED, permanently-unassigned gaps. They were the
// in-kernel UDP socket API (SYS_SOCKET/BIND/SENDTO/RECVFROM/NET_CLOSE). The
// in-kernel virtio-net driver and TCP/IP stack were torn down: networking is
// now a userspace concern (a `netd` driver process owning the virtio-net
// device via a Device capability, over the userspace device-driver framework).
// Never reuse 50-54 — an old socket-API caller must fail ENOSYS, not hit an
// unrelated handler (same rationale as gaps 49 and 59).
/// Power off the machine (ACPI S5 via QEMU PM1a port 0x604). No arguments. Does not return.
pub const SYS_POWEROFF:  u64 = 55;
/// Mount a filesystem backend at a path. Requires CapKind::Filesystem with WRITE.
/// a1=at_ptr, a2=at_len, a3=source (0 = fresh RAM-backed RFS V2), a4=flags.
/// Returns 0 or negative errno (ENOPERM, EMOUNTED, ENOENT, ENOTDIR, EINVAL, …).
pub const SYS_MOUNT:     u64 = 56;
/// Unmount the filesystem at exactly `at`, freeing its backend once no mount
/// namespace still routes to it. Same cap gate as SYS_MOUNT (Filesystem+WRITE).
/// The root mount (`/`) is pinned — unmounting it is EINVAL. a1=at_ptr, a2=at_len.
/// Returns 0 or negative errno (ENOPERM, ENOMNT, EINVAL). Numbered 69 because
/// 67-68 are reserved for the namespace syscalls (per-task-mount-namespace §1.1).
pub const SYS_UNMOUNT:   u64 = 69;
/// Create a symlink at `link` storing `target` verbatim (dangling legal; the
/// final component of `link` is not followed). On a store mount, creating a
/// link inside a sealed entry is EROFS.
/// a1=target_ptr, a2=target_len, a3=link_ptr, a4=link_len. Returns 0 or errno.
pub const SYS_SYMLINK:   u64 = 57;
/// Read a symlink's target (final component not followed — must BE a symlink,
/// else EINVAL). a1=path_ptr, a2=path_len, a3=buf_ptr, a4=buf_len.
/// Returns full target length (copied bytes = min(len, buf_len)) or errno.
pub const SYS_READLINK:  u64 = 58;
// Number 59 is a RETIRED, permanently-unassigned gap (was SYS_UNSEAL). The
// unseal primitive is gone by design — the realize-seal is absolute, no
// syscall makes sealed content writable. Never reuse 59 (an old SYS_UNSEAL
// caller must fail ENOSYS, not hit an unrelated handler). See
// abi/lythos-abi/src/syscall.rs.
/// Remove an entire unreferenced store path (whole-tree delete, freeing blocks)
/// below the seal layer — the sole store-reclamation primitive. Requires
/// CapKind::Filesystem with WRITE (store-owner authority). `path` names a
/// top-level entry on a store mount; the kernel drops that name's in-kernel
/// seal as part of the lifecycle removal. a1=path_ptr, a2=path_len.
pub const SYS_STORE_REMOVE: u64 = 60;

// ── Userspace device-driver framework ─────────────────────────────────────────
//
// These give a ring-3 driver ownership of a PCI device it holds a Device
// capability for: read its config space, map its MMIO BARs, allocate DMA
// buffers, and wait on its IRQ. All are gated on a CapKind::Device capability
// (gate-before-args → ENOPERM), EXCEPT SYS_DEV_CLAIM which is gated on the
// Rollback cap (lythd-exclusive) so only init claims devices from the registry.

/// Claim a PCI device from the kernel registry by name, minting a Device
/// capability into the caller's table. Rollback-cap gated (lythd only). A
/// device may be claimed once (re-claim → EINVAL). a1=name_ptr, a2=name_len.
/// Returns cap handle or ENOENT / ENOPERM / EINVAL.
pub const SYS_DEV_CLAIM:    u64 = 61;
/// Read one 32-bit dword from the device's PCI config space (lets a userspace
/// driver walk the modern-virtio capability list without port-I/O access).
/// a1=dev_cap, a2=offset (dword-aligned). Returns the u32 value or ENOPERM/EINVAL.
pub const SYS_DEV_CFG_READ: u64 = 62;
/// Map a device MMIO BAR into the caller's address space, uncacheable.
/// a1=dev_cap, a2=bar_index (0..6), a3=virt (page-aligned user addr). Maps the
/// whole BAR region. Returns BAR byte length or ENOPERM/EINVAL.
pub const SYS_DEV_MMIO_MAP: u64 = 63;
/// Allocate a contiguous, zeroed DMA buffer, mapped into the caller and handed
/// to the device. a1=dev_cap, a2=virt (page-aligned), a3=size (bytes, rounded
/// up to pages), a4=out_phys_ptr (*mut u64). Returns 0 or ENOPERM/EINVAL.
pub const SYS_DEV_DMA_ALLOC: u64 = 64;
/// Block until the device raises its IRQ (its IOAPIC line is masked-on-fire by
/// the kernel; the driver must SYS_DEV_IRQ_ACK after servicing). a1=dev_cap.
/// Returns 0 or ENOPERM.
pub const SYS_DEV_IRQ_WAIT: u64 = 65;
/// Acknowledge/unmask the device IRQ after the driver has serviced it (read the
/// device ISR to deassert the level line first). a1=dev_cap. Returns 0 or ENOPERM.
pub const SYS_DEV_IRQ_ACK:  u64 = 66;

/// Highest assigned syscall number. Update this when adding a new syscall.
/// Used by the fuzz test in main.rs to verify that numbers above this return ENOSYS.
/// Numbers 49, 50-54 and 59 are unassigned gaps (59 = retired SYS_UNSEAL,
/// 50-54 = retired UDP socket API); 67-68 are reserved for the not-yet-landed
/// namespace syscalls (SYS_NS_CREATE/SYS_NS_ENTER) and also return ENOSYS.
pub const SYSCALL_MAX: u64 = SYS_UNMOUNT;

// ── Error sentinel ────────────────────────────────────────────────────────────

/// Returned in RAX for unknown or unimplemented syscalls (analogous to ENOSYS).
pub const ENOSYS:  u64 = (-1i64) as u64;
/// Invalid or stale capability handle.
pub const ENOCAP:  u64 = (-2i64) as u64;
/// Insufficient capability rights for the requested operation.
pub const ENOPERM: u64 = (-3i64) as u64;
/// Invalid argument (e.g. target task not found, self-grant).
pub const EINVAL:  u64 = (-4i64) as u64;
/// No such file or directory.
pub const ENOENT:  u64 = (-5i64) as u64;
/// Bad file descriptor.
pub const EBADF:   u64 = (-6i64) as u64;
/// Resource temporarily unavailable (non-blocking op on empty/full resource).
pub const EAGAIN:  u64 = (-7i64) as u64;

// ── MSR addresses ─────────────────────────────────────────────────────────────

const IA32_EFER:  u32 = 0xC000_0080;
const IA32_STAR:  u32 = 0xC000_0081;
const IA32_LSTAR: u32 = 0xC000_0082;
const IA32_FMASK: u32 = 0xC000_0084;

// ── Per-CPU RSP save area ─────────────────────────────────────────────────────

/// Kernel RSP loaded on `syscall` entry.  Set by `enter_userspace`.
#[unsafe(no_mangle)]
pub static mut SYSCALL_KERN_RSP: u64 = 0;

/// User RSP saved on `syscall` entry, restored on `sysretq`.
#[unsafe(no_mangle)]
pub static mut SYSCALL_USER_RSP: u64 = 0;

// ── Assembly stubs ────────────────────────────────────────────────────────────
//
// Frame layout on the kernel stack after all pushes in syscall_entry
// (lower addresses first, [rsp+0] = r15):
//
//   [rsp +  0]  r15
//   [rsp +  8]  r14
//   [rsp + 16]  r13
//   [rsp + 24]  r12
//   [rsp + 32]  rbx
//   [rsp + 40]  rbp
//   [rsp + 48]  r11  — user RFLAGS (needed by sysretq)
//   [rsp + 56]  rcx  — user RIP    (needed by sysretq)
//   [rsp + 64]  rax  — syscall number
//   [rsp + 72]  rdi  — a1
//   [rsp + 80]  rsi  — a2
//   [rsp + 88]  rdx  — a3
//   [rsp + 96]  r10  — a4
//   [rsp +104]  r8   — a5
//   [rsp +112]  r9   — a6

global_asm!(r#"
.section .text

// ─────────────────────────────────────────────────────────────────────────────
// syscall_entry — LSTAR target.
//
// On entry: RAX=nr, RDI-R9=args, RCX=user RIP, R11=user RFLAGS, IF=0.
// ─────────────────────────────────────────────────────────────────────────────
.global syscall_entry
.type   syscall_entry, @function
syscall_entry:
    // Save user RSP to the global temporarily, then switch to the kernel stack.
    movq   %rsp, SYSCALL_USER_RSP(%rip)
    movq   SYSCALL_KERN_RSP(%rip), %rsp

    // Push user RSP FIRST (highest address on kernel stack, before the rest of
    // the frame).  This stores it per-task so that if yield_task() runs while
    // we are inside a syscall, another task's syscall cannot overwrite it via
    // the global SYSCALL_USER_RSP.
    pushq  SYSCALL_USER_RSP(%rip)   // saved user RSP (above SyscallFrame)

    pushq  %r9
    pushq  %r8
    pushq  %r10
    pushq  %rdx
    pushq  %rsi
    pushq  %rdi
    pushq  %rax      // nr
    pushq  %rcx      // user rip  (for sysretq)
    pushq  %r11      // user rflags (for sysretq)
    pushq  %rbp
    pushq  %rbx
    pushq  %r12
    pushq  %r13
    pushq  %r14
    pushq  %r15

    movq   %rsp, %rdi          // arg0: *mut SyscallFrame (r15 at lowest address)
    call   syscall_dispatch    // returns u64 in rax

    // Restore callee-saved regs without clobbering rax (return value).
    popq   %r15
    popq   %r14
    popq   %r13
    popq   %r12
    popq   %rbx
    popq   %rbp
    popq   %r11    // user rflags → R11 (used by sysretq)
    popq   %rcx    // user rip   → RCX (used by sysretq)

    // Restore user argument registers so they are preserved across syscalls.
    // Push order was: r9, r8, r10, rdx, rsi, rdi, rax(nr).
    // Skip nr (rax), then pop in reverse push order.
    addq   $8, %rsp            // skip nr (syscall number was in rax; we use rax for return value)
    popq   %rdi
    popq   %rsi
    popq   %rdx
    popq   %r10
    popq   %r8
    popq   %r9

    // Restore user RSP from the kernel stack (not the global — another task
    // may have overwritten SYSCALL_USER_RSP while we were inside yield_task).
    popq   %rsp
    sysretq

// ─────────────────────────────────────────────────────────────────────────────
// enter_userspace_asm(entry: u64, stack: u64) -> !
//
// rdi = user RIP,  rsi = user RSP
// Loads user segment registers, builds an iretq frame, and jumps to ring 3.
// ─────────────────────────────────────────────────────────────────────────────
.global enter_userspace_asm
.type   enter_userspace_asm, @function
enter_userspace_asm:
    // Set user data selector in DS/ES/FS/GS before crossing the ring boundary.
    movw   $0x1B, %ax   // USER_DATA_SEL | RPL=3  (0x18 | 3)
    movw   %ax,  %ds
    movw   %ax,  %es
    movw   %ax,  %fs
    movw   %ax,  %gs

    // iretq frame: SS | RSP | RFLAGS | CS | RIP  (pushed high → low)
    pushq  $0x1B          // SS:  user data | RPL=3
    pushq  %rsi           // RSP: user stack
    pushfq
    orq    $(1 << 9), (%rsp)   // ensure IF=1 in user RFLAGS
    pushq  $0x23          // CS:  user code | RPL=3  (0x20 | 3)
    pushq  %rdi           // RIP: user entry point
    iretq
"#, options(att_syntax));

unsafe extern "C" {
    fn syscall_entry();
    fn enter_userspace_asm(entry: u64, stack: u64) -> !;
}

// ── Syscall frame ─────────────────────────────────────────────────────────────

/// Register state pushed onto the kernel stack by `syscall_entry`.
///
/// Layout matches the push sequence in the assembly stub (r15 at the lowest
/// address, r9 at the highest).
#[repr(C)]
pub struct SyscallFrame {
    pub r15: u64,
    pub r14: u64,
    pub r13: u64,
    pub r12: u64,
    pub rbx: u64,
    pub rbp: u64,
    pub r11: u64,   // user RFLAGS
    pub rcx: u64,   // user RIP
    pub nr:  u64,   // syscall number (RAX on entry)
    pub a1:  u64,   // RDI
    pub a2:  u64,   // RSI
    pub a3:  u64,   // RDX
    pub a4:  u64,   // R10
    pub a5:  u64,   // R8
    pub a6:  u64,   // R9
}

// ── SMAP state ────────────────────────────────────────────────────────────────

/// Set to `true` by `init()` when SMAP is detected and CR4.SMAP is enabled.
/// Consulted by `with_user_access` to gate STAC/CLAC emission — those
/// instructions are `#UD` on CPUs that don't advertise SMAP support.
static SMAP_ENABLED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

/// Execute `f` inside a SMAP-safe window.
///
/// If SMAP is active this issues `stac` (sets AC, allowing kernel access to
/// user pages) before calling `f` and `clac` (clears AC) afterwards.  If
/// SMAP is not active the call is a direct passthrough with no overhead.
///
/// # Safety
/// `f` must only touch user memory that has already been validated by
/// `valid_user_range`; the window must be as narrow as possible.
#[inline]
pub(crate) unsafe fn with_user_access<F, R>(f: F) -> R
where
    F: FnOnce() -> R,
{
    if SMAP_ENABLED.load(core::sync::atomic::Ordering::Relaxed) {
        unsafe { core::arch::asm!("stac", options(nostack, preserves_flags)); }
        let r = f();
        unsafe { core::arch::asm!("clac", options(nostack, preserves_flags)); }
        r
    } else {
        f()
    }
}

// ── User-pointer validation ───────────────────────────────────────────────────

/// Return `true` if `[ptr, ptr+len)` lies entirely in canonical user space.
///
/// Rejects: null pointers, arithmetic overflow, and addresses at or above the
/// user/kernel split (`0x0000_8000_0000_0000`).  Zero-length ranges are
/// Check DAC permission. `bits`: 0x4=read, 0x2=write, 0x1=execute.
/// Root (uid=0) always passes. Returns true if access is permitted.
#[inline]
fn dac_check(mode: u16, inode_uid: u32, inode_gid: u32, task_uid: u32, task_gid: u32, bits: u16) -> bool {
    if task_uid == 0 { return true; }
    let eff = if task_uid == inode_uid {
        (mode >> 6) & 0x7
    } else if task_gid == inode_gid {
        (mode >> 3) & 0x7
    } else {
        mode & 0x7
    };
    (eff & bits) == bits
}

/// Return the parent directory path of `path` as a byte vec.
/// `/foo/bar` → `/foo`, `/foo` → `/`, `/` → `/`.
fn parent_of(path: &[u8]) -> alloc::vec::Vec<u8> {
    let s = match core::str::from_utf8(path) {
        Ok(s)  => s,
        Err(_) => return alloc::vec![b'/'],
    };
    match s.rfind('/') {
        None | Some(0) => alloc::vec![b'/'],
        Some(i)        => s[..i].as_bytes().to_vec(),
    }
}

/// accepted for any non-null pointer (no bytes are dereferenced).
#[inline]
pub(crate) fn valid_user_range(ptr: u64, len: u64) -> bool {
    if ptr == 0 { return false; }
    if len == 0 { return true; }
    match ptr.checked_add(len) {
        Some(end) => end <= 0x0000_8000_0000_0000,
        None      => false,
    }
}

// ── Syscall dispatch ──────────────────────────────────────────────────────────

/// Called by `syscall_entry` with a pointer to the kernel-stack frame.
/// The return value is placed in RAX before `sysretq`.
#[unsafe(no_mangle)]
pub extern "C" fn syscall_dispatch(frame: &mut SyscallFrame) -> u64 {
    match frame.nr {
        SYS_YIELD => {
            crate::task::yield_task();
            0
        }
        SYS_TASK_EXIT => {
            // a1 = exit code; only the low 8 bits are meaningful (0..=255),
            // 0 = success. Encoded as a normal (non-abnormal) status word.
            crate::task::task_exit(crate::task::exit_status_normal(frame.a1 as u32));
        }
        SYS_MMAP => {
            // Require page-aligned virtual address.
            if frame.a1 & 0xFFF != 0 { return EINVAL; }
            // Reject the 0→1 GiB identity-map range (2 MiB huge pages — walk_or_create
            // would panic on the PS=1 entry) and all kernel-space addresses (above the
            // canonical user/kernel split) to prevent corrupting shared kernel page
            // table entries via the U/S propagation in walk_or_create.
            if frame.a1 < 0x4000_0000 || frame.a1 >= 0x0000_8000_0000_0000 { return EINVAL; }

            // Require a Memory capability with write access.
            let current_id = crate::task::current_task_id();
            let table_ptr  = crate::task::cap_table_ptr(current_id);
            if table_ptr.is_null() { return ENOPERM; }
            let table = unsafe { &*table_ptr };
            if !table.has_kind_with_rights(
                crate::cap::CapKind::Memory,
                crate::cap::CapRights::WRITE,
            ) {
                return ENOPERM;
            }

            let virt = crate::vmm::VirtAddr(frame.a1);

            // Reject double-map before touching page tables or the PMM.
            if !crate::task::vma_insert(frame.a1) { return EINVAL; }

            // Sanitize flags: keep only user-safe bits and force USER.
            let allowed = crate::vmm::PageFlags::PRESENT.0
                | crate::vmm::PageFlags::WRITABLE.0
                | crate::vmm::PageFlags::USER.0
                | crate::vmm::PageFlags::NX.0;
            let flags = crate::vmm::PageFlags(
                (frame.a3 & allowed) | crate::vmm::PageFlags::USER.0
            );

            // Allocate a fresh PMM frame (user cannot name a physical address).
            let Some(phys) = crate::pmm::alloc_frame() else {
                // Undo the VMA reservation we just made.
                crate::task::vma_remove(frame.a1);
                return EINVAL;
            };

            match crate::task::current_page_table() {
                Some(pml4) => crate::vmm::map_page_in(
                    crate::pmm::PhysAddr(pml4), virt, phys, flags
                ),
                None       => crate::vmm::map_page(virt, phys, flags),
            }
            0
        }
        SYS_MUNMAP => {
            // Require page-aligned virtual address in the user range.
            if frame.a1 & 0xFFF != 0 { return EINVAL; }
            if frame.a1 < 0x4000_0000 || frame.a1 >= 0x0000_8000_0000_0000 { return EINVAL; }

            // Reject unmaps for addresses this task never mapped.
            if !crate::task::vma_remove(frame.a1) { return EINVAL; }

            let virt = crate::vmm::VirtAddr(frame.a1);
            match crate::task::current_page_table() {
                Some(pml4) => {
                    // unmap_page_in clears the PTE, invalidates the TLB
                    // (local + shootdown), and returns the backing frame.
                    if let Some(phys) = crate::vmm::unmap_page_in(
                        crate::pmm::PhysAddr(pml4), virt,
                    ) {
                        crate::pmm::free_frame(phys);
                    }
                }
                None => crate::vmm::unmap_page(virt),
            }
            0
        }
        SYS_CAP_GRANT => {
            let handle      = crate::cap::CapHandle(frame.a1);
            let target_id   = frame.a2;   // TaskId of recipient
            let rights_mask = crate::cap::CapRights(frame.a3 as u8);

            let current_id = crate::task::current_task_id();
            let from_ptr   = crate::task::cap_table_ptr(current_id);

            // Validate the handle first — ENOCAP takes priority over EINVAL.
            if from_ptr.is_null() { return ENOCAP; }
            let from = unsafe { &mut *from_ptr };
            if from.get(handle).is_err() { return ENOCAP; }

            let to_ptr = crate::task::cap_table_ptr(target_id);
            if to_ptr.is_null() || from_ptr == to_ptr {
                return EINVAL;
            }

            // SAFETY: from_ptr and to_ptr point to two *different* tasks' cap
            // tables; the single-threaded kernel guarantees no aliasing here.
            let to = unsafe { &mut *to_ptr };

            match crate::cap::cap_grant(from, handle, target_id, to, rights_mask) {
                Ok(new_handle) => new_handle.0,
                Err(crate::cap::CapError::NoGrant) => ENOPERM,
                Err(_) => ENOCAP,
            }
        }
        SYS_CAP_REVOKE => {
            let handle     = crate::cap::CapHandle(frame.a1);
            let current_id = crate::task::current_task_id();
            let table_ptr  = crate::task::cap_table_ptr(current_id);

            if table_ptr.is_null() { return ENOCAP; }
            let table = unsafe { &mut *table_ptr };

            match crate::cap::cap_cascade_revoke(table, handle, &mut |tid| {
                crate::task::cap_table_ptr(tid)
            }) {
                Ok(())                                    => 0,
                Err(crate::cap::CapError::NoRevoke)       => ENOPERM,
                Err(_)                                    => ENOCAP,
            }
        }
        SYS_IPC_CREATE => {
            // Allocate a ring-buffer page and register an IPC endpoint.
            // Returns a capability handle (CapHandle.0) to the caller.
            let endpoint_idx = match crate::ipc::create_endpoint() {
                Some(idx) => idx,
                None      => return EINVAL, // global endpoint cap reached
            };

            let obj = crate::cap::create_object(
                crate::cap::KernelObject::Ipc { endpoint_idx }
            ).expect("SYS_IPC_CREATE: KoTable OOM");

            let current_id = crate::task::current_task_id();
            let table_ptr  = crate::task::cap_table_ptr(current_id);
            if table_ptr.is_null() { return ENOCAP; }
            let table = unsafe { &mut *table_ptr };

            let handle = crate::cap::create_root_cap(
                table,
                crate::cap::CapKind::Ipc,
                crate::cap::CapRights::ALL,
                obj,
            );
            handle.0
        }
        SYS_IPC_SEND => {
            // a1 = CapHandle, a2 = msg_ptr (user VA), a3 = msg_len
            let handle  = crate::cap::CapHandle(frame.a1);
            let msg_len = (frame.a3 as usize).min(crate::ipc::MSG_SIZE);
            if !valid_user_range(frame.a2, msg_len as u64) { return EINVAL; }
            let msg_ptr = frame.a2 as *const u8;

            let current_id = crate::task::current_task_id();
            let table_ptr  = crate::task::cap_table_ptr(current_id);
            if table_ptr.is_null() { return ENOCAP; }
            let table = unsafe { &*table_ptr };

            let endpoint_idx = match table.get(handle) {
                Ok(c) if c.kind == crate::cap::CapKind::Ipc
                      && c.rights.has(crate::cap::CapRights::WRITE) => {
                    match crate::cap::get_object(c.object) {
                        Some(crate::cap::KernelObject::Ipc { endpoint_idx }) => *endpoint_idx,
                        _ => return ENOCAP,
                    }
                }
                Ok(_) => return ENOPERM,
                Err(_) => return ENOCAP,
            };

            let msg = unsafe { with_user_access(|| core::slice::from_raw_parts(msg_ptr, msg_len)) };
            crate::ipc::send(endpoint_idx, msg);
            0
        }
        SYS_IPC_RECV => {
            // a1 = CapHandle, a2 = buf_ptr (user VA), a3 = buf_len
            let handle  = crate::cap::CapHandle(frame.a1);
            let buf_len = (frame.a3 as usize).min(crate::ipc::MSG_SIZE);
            if !valid_user_range(frame.a2, buf_len as u64) { return EINVAL; }
            let buf_ptr = frame.a2 as *mut u8;

            let current_id = crate::task::current_task_id();
            let table_ptr  = crate::task::cap_table_ptr(current_id);
            if table_ptr.is_null() { return ENOCAP; }
            let table = unsafe { &*table_ptr };

            let endpoint_idx = match table.get(handle) {
                Ok(c) if c.kind == crate::cap::CapKind::Ipc
                      && c.rights.has(crate::cap::CapRights::READ) => {
                    match crate::cap::get_object(c.object) {
                        Some(crate::cap::KernelObject::Ipc { endpoint_idx }) => *endpoint_idx,
                        _ => return ENOCAP,
                    }
                }
                Ok(_) => return ENOPERM,
                Err(_) => return ENOCAP,
            };

            let mut buf = [0u8; crate::ipc::MSG_SIZE];
            let n = crate::ipc::recv(endpoint_idx, &mut buf);
            unsafe { with_user_access(|| {
                core::ptr::copy_nonoverlapping(buf.as_ptr(), buf_ptr, n.min(buf_len));
            }) };
            n as u64
        }
        SYS_ROLLBACK => {
            // Gate on the caller holding a CapKind::Rollback capability.
            let current_id = crate::task::current_task_id();
            let table_ptr  = crate::task::cap_table_ptr(current_id);
            if table_ptr.is_null() { return ENOPERM; }
            let table = unsafe { &*table_ptr };
            if !table.has_kind(crate::cap::CapKind::Rollback) { return ENOPERM; }

            // Privileged: halt the system for now.  lythd implements the actual
            // rollback policy; the kernel just verifies the capability and stops.
            crate::kprintln!("[rollback] triggered by task {} — halting", current_id);
            loop { unsafe { core::arch::asm!("hlt") }; }
        }
        SYS_EXEC => {
            // a1 = elf_ptr   (user VA, *const u8)
            // a2 = elf_len   (bytes)
            // a3 = caps_ptr  (user VA, *const u64 array of raw CapHandle values)
            // a4 = caps_len  (element count)
            // a5 = argv_ptr  (user VA, flat null-terminated strings: "arg0\0arg1\0…")
            // a6 = argv_bytes (total byte length of the argv buffer; 0 = no argv)
            extern crate alloc;
            use alloc::{string::String, vec::Vec};

            let elf_len    = frame.a2 as usize;
            let caps_len   = frame.a4 as usize;
            let argv_bytes = frame.a6 as usize;
            let caps_bytes = (caps_len as u64).saturating_mul(8);
            if !valid_user_range(frame.a1, elf_len as u64) { return EINVAL; }
            if caps_len > 0 && !valid_user_range(frame.a3, caps_bytes) { return EINVAL; }
            if argv_bytes > 0 && !valid_user_range(frame.a5, argv_bytes as u64) { return EINVAL; }
            if argv_bytes > 4000 { return EINVAL; } // fits in one stack page

            let elf_ptr  = frame.a1 as *const u8;
            let caps_ptr = frame.a3 as *const u64;

            // Copy ELF and caps into kernel-owned buffers while STAC is active.
            // The user pointers must NOT be dereferenced outside with_user_access
            // when SMAP is enabled.
            let mut elf_buf = alloc::vec![0u8; elf_len];
            unsafe { with_user_access(|| {
                core::ptr::copy_nonoverlapping(elf_ptr, elf_buf.as_mut_ptr(), elf_len);
            }) };

            let caps: Vec<crate::cap::CapHandle> = if caps_len == 0 {
                Vec::new()
            } else {
                let mut raw = alloc::vec![0u64; caps_len];
                unsafe { with_user_access(|| {
                    core::ptr::copy_nonoverlapping(caps_ptr, raw.as_mut_ptr(), caps_len);
                }) };
                raw.iter().map(|&h| crate::cap::CapHandle(h)).collect()
            };

            // Parse flat argv buffer: "arg0\0arg1\0…" → Vec<String>
            let argv_owned: Vec<String> = if argv_bytes == 0 || frame.a5 == 0 {
                Vec::new()
            } else {
                let mut argv_buf = alloc::vec![0u8; argv_bytes];
                unsafe { with_user_access(|| {
                    core::ptr::copy_nonoverlapping(
                        frame.a5 as *const u8,
                        argv_buf.as_mut_ptr(),
                        argv_bytes,
                    );
                }) };
                argv_buf.split(|&b| b == 0)
                   .filter(|s| !s.is_empty())
                   .filter_map(|s| core::str::from_utf8(s).ok().map(String::from))
                   .collect()
            };
            let argv_strs: Vec<&str> = argv_owned.iter().map(|s| s.as_str()).collect();

            match crate::elf::exec(&elf_buf, &caps, &argv_strs) {
                Ok(task_id) => task_id,
                Err(_)      => EINVAL,
            }
        }
        SYS_LOG => {
            // a1 = ptr (user VA, *const u8), a2 = len
            let len = frame.a2 as usize;
            if len == 0 { return 0; }
            if len > 4096 { return EINVAL; }
            if !valid_user_range(frame.a1, len as u64) { return EINVAL; }
            let bytes = unsafe { with_user_access(|| core::slice::from_raw_parts(frame.a1 as *const u8, len)) };
            if let Ok(s) = core::str::from_utf8(bytes) {
                crate::kprint!("{}", s);
            }
            0
        }
        SYS_IPC_SEND_CAP => {
            // a1=ipc_cap_handle, a2=msg_ptr, a3=msg_len, a4=cap_handle_to_send
            let ipc_handle      = crate::cap::CapHandle(frame.a1);
            let msg_len         = (frame.a3 as usize).min(crate::ipc::MSG_SIZE);
            if !valid_user_range(frame.a2, msg_len as u64) { return EINVAL; }
            let msg_ptr         = frame.a2 as *const u8;
            let send_cap_handle = crate::cap::CapHandle(frame.a4);

            let current_id = crate::task::current_task_id();
            let table_ptr  = crate::task::cap_table_ptr(current_id);
            if table_ptr.is_null() { return ENOCAP; }
            let table = unsafe { &mut *table_ptr };

            // Resolve the IPC endpoint (requires WRITE).
            let endpoint_idx = match table.get(ipc_handle) {
                Ok(c) if c.kind == crate::cap::CapKind::Ipc
                      && c.rights.has(crate::cap::CapRights::WRITE) => {
                    match crate::cap::get_object(c.object) {
                        Some(crate::cap::KernelObject::Ipc { endpoint_idx }) => *endpoint_idx,
                        _ => return ENOCAP,
                    }
                }
                Ok(_)  => return ENOPERM,
                Err(_) => return ENOCAP,
            };

            // Take (move) the capability out of the caller's table.
            let cap = match table.take(send_cap_handle) {
                Ok(c)  => c,
                Err(_) => return ENOCAP,
            };

            let msg = unsafe { with_user_access(|| core::slice::from_raw_parts(msg_ptr, msg_len)) };
            crate::ipc::send_cap(endpoint_idx, msg, cap);
            0
        }
        SYS_SERIAL_READ => {
            // a1 = buf_ptr (user VA, *mut u8), a2 = buf_len
            let buf_len = (frame.a2 as usize).min(4096);
            if buf_len == 0 { return 0; }
            if !valid_user_range(frame.a1, buf_len as u64) { return EINVAL; }
            let buf_ptr = frame.a1 as *mut u8;

            let mut tmp = [0u8; 4096];
            let mut n   = 0usize;

            // Block (yielding the CPU) until a byte arrives from COM1 or the
            // PS/2 keyboard ring buffer, then drain whatever is immediately ready.
            //
            // Syscall entry clears IF (FMASK). Re-enable interrupts before each
            // yield so keyboard/timer IRQs can fire while we wait.
            loop {
                if let Some(b) = crate::keyboard::try_read() {
                    tmp[n] = b; n += 1; break;
                }
                // Bind the read result in a `let` so the SERIAL guard drops at
                // the semicolon.  Matching on `SERIAL.lock().try_read_byte()`
                // directly keeps the scrutinee temporary — the lock — alive
                // through the whole match, i.e. across yield_task(): any other
                // task that then printed would spin on SERIAL with interrupts
                // off, deadlocking the CPU.
                let byte = crate::serial::SERIAL.lock().try_read_byte();
                match byte {
                    Some(b) => { tmp[n] = b; n += 1; break; }
                    None    => unsafe {
                        core::arch::asm!("sti", options(nostack));
                        crate::task::yield_task();
                        // IF may be 1 or 0 here; SERIAL.lock() on the next
                        // iteration saves and restores it correctly either way.
                    },
                }
            }
            while n < buf_len {
                if let Some(b) = crate::keyboard::try_read() {
                    tmp[n] = b; n += 1;
                } else {
                    let byte = crate::serial::SERIAL.lock().try_read_byte();
                    match byte {
                        Some(b) => { tmp[n] = b; n += 1; }
                        None    => break,
                    }
                }
            }

            unsafe { with_user_access(|| {
                core::ptr::copy_nonoverlapping(tmp.as_ptr(), buf_ptr, n);
            }) };
            // TEMP DEBUG: track RX delivery for the idle diagnostics.
            crate::serial::RX_DELIVERED
                .fetch_add(n as u64, core::sync::atomic::Ordering::Relaxed);
            n as u64
        }
        SYS_IPC_RECV_CAP => {
            // a1=ipc_cap_handle, a2=buf_ptr, a3=buf_len, a4=out_handle_ptr (*mut u64)
            let ipc_handle      = crate::cap::CapHandle(frame.a1);
            let buf_len         = (frame.a3 as usize).min(crate::ipc::MSG_SIZE);
            if !valid_user_range(frame.a2, buf_len as u64) { return EINVAL; }
            // out_handle_ptr is optional (0 = ignored); validate only if provided.
            if frame.a4 != 0 && !valid_user_range(frame.a4, 8) { return EINVAL; }
            let buf_ptr         = frame.a2 as *mut u8;
            let out_handle_ptr  = frame.a4 as *mut u64;

            let current_id = crate::task::current_task_id();
            let table_ptr  = crate::task::cap_table_ptr(current_id);
            if table_ptr.is_null() { return ENOCAP; }
            let table = unsafe { &*table_ptr };

            // Resolve the IPC endpoint (requires READ).
            let endpoint_idx = match table.get(ipc_handle) {
                Ok(c) if c.kind == crate::cap::CapKind::Ipc
                      && c.rights.has(crate::cap::CapRights::READ) => {
                    match crate::cap::get_object(c.object) {
                        Some(crate::cap::KernelObject::Ipc { endpoint_idx }) => *endpoint_idx,
                        _ => return ENOCAP,
                    }
                }
                Ok(_)  => return ENOPERM,
                Err(_) => return ENOCAP,
            };

            let mut buf = [0u8; crate::ipc::MSG_SIZE];
            let (n, maybe_cap) = crate::ipc::recv_cap(endpoint_idx, &mut buf);
            unsafe { with_user_access(|| {
                core::ptr::copy_nonoverlapping(buf.as_ptr(), buf_ptr, n.min(buf_len));
            }) };

            // Insert the received capability (if any) and write the handle.
            if out_handle_ptr as u64 != 0 {
                let handle_val = if let Some(cap) = maybe_cap {
                    let table_mut = unsafe { &mut *table_ptr };
                    table_mut.insert(cap).0
                } else {
                    u64::MAX
                };
                unsafe { with_user_access(|| out_handle_ptr.write(handle_val)) };
            }

            n as u64
        }
        SYS_TIME => {
            // Return milliseconds since boot — APIC tick counter (1 tick ≈ 1 ms).
            crate::apic::ticks()
        }
        SYS_TASK_STATUS => {
            // a1 = TaskId; returns canonical: 0=dead/missing, 1=running, 2=ready, 3=blocked.
            crate::task::task_status_raw(frame.a1)
        }
        SYS_TASK_LIST => {
            // a1=buf_ptr (*mut TaskInfo), a2=buf_capacity (max entries)
            // TaskInfo layout: u64 id | u64 state | u8 kind | [u8;7] pad  (24 bytes)
            let cap = frame.a2 as usize;
            if cap == 0 { return 0; }
            const ENTRY: usize = 24;
            let bytes = cap.saturating_mul(ENTRY);
            if !valid_user_range(frame.a1, bytes as u64) { return EINVAL; }
            let buf = frame.a1 as *mut u8;
            let n = crate::task::for_each_task(|idx, id, state_raw, kind| {
                if idx >= cap { return; }
                let entry = unsafe { buf.add(idx * ENTRY) };
                unsafe {
                    with_user_access(|| {
                        core::ptr::write_unaligned(entry as *mut u64, id);
                        core::ptr::write_unaligned(entry.add(8) as *mut u64, state_raw);
                        core::ptr::write(entry.add(16), kind);
                        core::ptr::write_bytes(entry.add(17), 0, 7);
                    });
                }
            });
            n as u64
        }
        SYS_MEM_STAT => {
            crate::pmm::free_frame_count() as u64
        }
        SYS_TASK_KILL => {
            // a1 = TaskId to kill. Only root may kill arbitrary tasks.
            if crate::task::current_task_uid() != 0 { return ENOPERM; }
            let target = frame.a1;
            if crate::task::kill_task(target) { 0 } else { EINVAL }
        }
        SYS_BLK_READ => {
            // a1 = sector (u64), a2 = buf_ptr (user VA, *mut u8, 512 bytes)
            if crate::task::current_task_uid() != 0 { return ENOPERM; }
            if !crate::virtio_blk::is_present() { return ENOSYS; }
            let sector  = frame.a1;
            let buf_ptr = frame.a2;
            if !valid_user_range(buf_ptr, crate::virtio_blk::SECTOR_SIZE as u64) {
                return EINVAL;
            }
            let mut kbuf = [0u8; crate::virtio_blk::SECTOR_SIZE];
            if !crate::virtio_blk::read_sector(sector, &mut kbuf) { return EINVAL; }
            unsafe {
                with_user_access(|| {
                    core::ptr::copy_nonoverlapping(
                        kbuf.as_ptr(),
                        buf_ptr as *mut u8,
                        crate::virtio_blk::SECTOR_SIZE,
                    );
                });
            }
            0
        }
        SYS_BLK_WRITE => {
            // a1 = sector (u64), a2 = buf_ptr (user VA, *const u8, 512 bytes)
            if crate::task::current_task_uid() != 0 { return ENOPERM; }
            if !crate::virtio_blk::is_present() { return ENOSYS; }
            let sector  = frame.a1;
            let buf_ptr = frame.a2;
            if !valid_user_range(buf_ptr, crate::virtio_blk::SECTOR_SIZE as u64) {
                return EINVAL;
            }
            let mut kbuf = [0u8; crate::virtio_blk::SECTOR_SIZE];
            unsafe {
                with_user_access(|| {
                    core::ptr::copy_nonoverlapping(
                        buf_ptr as *const u8,
                        kbuf.as_mut_ptr(),
                        crate::virtio_blk::SECTOR_SIZE,
                    );
                });
            }
            if !crate::virtio_blk::write_sector(sector, &kbuf) { return EINVAL; }
            0
        }
        SYS_OPEN => {
            // a1=path_ptr, a2=path_len
            let path_len = frame.a2 as usize;
            if path_len == 0 || path_len > 4096 { return EINVAL; }
            if !valid_user_range(frame.a1, frame.a2) { return EINVAL; }
            let mut kpath = alloc::vec![0u8; path_len];
            unsafe { with_user_access(|| core::ptr::copy_nonoverlapping(
                frame.a1 as *const u8, kpath.as_mut_ptr(), path_len,
            )); }
            let mut stat = crate::vfs::Stat::default();
            if !crate::vfs::stat_path(&kpath, &mut stat) { return ENOENT; }
            let uid = crate::task::current_task_uid();
            let gid = crate::task::current_task_gid();
            if !dac_check(stat.mode, stat.uid, stat.gid, uid, gid, 0x4) { return ENOPERM; }
            let fd = crate::vfs::open(&kpath);
            fd as u64
        }
        SYS_READ => {
            // a1=fd, a2=buf_ptr, a3=len
            let len = (frame.a3 as usize).min(1024 * 1024);
            if len > 0 && !valid_user_range(frame.a2, frame.a3) { return EINVAL; }
            let mut kbuf = alloc::vec![0u8; len];
            let n = crate::vfs::read(frame.a1, &mut kbuf);
            if n < 0 { return n as u64; }
            if n > 0 {
                unsafe { with_user_access(|| core::ptr::copy_nonoverlapping(
                    kbuf.as_ptr(), frame.a2 as *mut u8, n as usize,
                )); }
            }
            n as u64
        }
        SYS_WRITE => {
            // a1=fd, a2=buf_ptr, a3=len
            let len = (frame.a3 as usize).min(1024 * 1024);
            if len > 0 && !valid_user_range(frame.a2, frame.a3) { return EINVAL; }
            let mut kbuf = alloc::vec![0u8; len];
            if len > 0 {
                unsafe { with_user_access(|| core::ptr::copy_nonoverlapping(
                    frame.a2 as *const u8, kbuf.as_mut_ptr(), len,
                )); }
            }
            let n = crate::vfs::write(frame.a1, &kbuf);
            n as u64
        }
        SYS_CLOSE => {
            // a1=fd
            let r = crate::vfs::close(frame.a1);
            r as u64
        }
        SYS_STAT => {
            // a1=path_ptr, a2=path_len, a3=stat_ptr (user, 48 bytes)
            let path_len = frame.a2 as usize;
            if path_len == 0 || path_len > 4096 { return EINVAL; }
            if !valid_user_range(frame.a1, frame.a2) { return EINVAL; }
            if !valid_user_range(frame.a3, 48) { return EINVAL; }
            let mut kpath = alloc::vec![0u8; path_len];
            unsafe { with_user_access(|| core::ptr::copy_nonoverlapping(
                frame.a1 as *const u8, kpath.as_mut_ptr(), path_len,
            )); }
            let mut stat = crate::vfs::Stat::default();
            if !crate::vfs::stat_path(&kpath, &mut stat) { return ENOENT; }
            // Serialise Stat into a 48-byte user buffer (all LE, canonical layout):
            // [0..8]=size [8..16]=mtime [16..24]=ctime [24..28]=flags [28..32]=uid
            // [32..36]=gid [36..40]=nlink [40..42]=mode [42..48]=_pad
            let mut buf = [0u8; 48];
            buf[ 0.. 8].copy_from_slice(&stat.size.to_le_bytes());
            buf[ 8..16].copy_from_slice(&stat.mtime.to_le_bytes());
            buf[16..24].copy_from_slice(&stat.ctime.to_le_bytes());
            buf[24..28].copy_from_slice(&stat.flags.to_le_bytes());
            buf[28..32].copy_from_slice(&stat.uid.to_le_bytes());
            buf[32..36].copy_from_slice(&stat.gid.to_le_bytes());
            buf[36..40].copy_from_slice(&stat.nlink.to_le_bytes());
            buf[40..42].copy_from_slice(&stat.mode.to_le_bytes());
            unsafe { with_user_access(|| core::ptr::copy_nonoverlapping(
                buf.as_ptr(), frame.a3 as *mut u8, 48,
            )); }
            0
        }
        SYS_READDIR => {
            // a1=path_ptr, a2=path_len, a3=buf_ptr, a4=buf_len
            // Each entry written as: ino(4) ft(1) name_len(1) _pad(2) name[256] = 264 bytes.
            let path_len = frame.a2 as usize;
            if path_len == 0 || path_len > 4096 { return EINVAL; }
            if !valid_user_range(frame.a1, frame.a2) { return EINVAL; }
            let buf_len = frame.a4 as usize;
            if buf_len > 0 && !valid_user_range(frame.a3, frame.a4) { return EINVAL; }
            let mut kpath = alloc::vec![0u8; path_len];
            unsafe { with_user_access(|| core::ptr::copy_nonoverlapping(
                frame.a1 as *const u8, kpath.as_mut_ptr(), path_len,
            )); }
            let entries = match crate::vfs::readdir_path(&kpath) {
                Some(e) => e,
                None    => return ENOENT,
            };
            const ENTRY_SIZE: usize = 264;
            let max_entries = buf_len / ENTRY_SIZE;
            let count = entries.len().min(max_entries);
            let mut kbuf = alloc::vec![0u8; count * ENTRY_SIZE];
            for (i, e) in entries.iter().take(count).enumerate() {
                let off = i * ENTRY_SIZE;
                kbuf[off..off+4].copy_from_slice(&e.ino.to_le_bytes());
                kbuf[off+4] = e.file_type;
                let name_bytes = e.name.as_bytes();
                let name_len = name_bytes.len().min(255);
                kbuf[off+5] = name_len as u8;
                kbuf[off+8..off+8+name_len].copy_from_slice(&name_bytes[..name_len]);
            }
            unsafe { with_user_access(|| core::ptr::copy_nonoverlapping(
                kbuf.as_ptr(), frame.a3 as *mut u8, count * ENTRY_SIZE,
            )); }
            count as u64
        }
        SYS_CREATE => {
            // a1=path_ptr, a2=path_len
            let path_len = frame.a2 as usize;
            if path_len == 0 || path_len > 4096 { return EINVAL; }
            if !valid_user_range(frame.a1, frame.a2) { return EINVAL; }
            let mut kpath = alloc::vec![0u8; path_len];
            unsafe { with_user_access(|| core::ptr::copy_nonoverlapping(
                frame.a1 as *const u8, kpath.as_mut_ptr(), path_len,
            )); }
            let uid = crate::task::current_task_uid();
            let gid = crate::task::current_task_gid();
            let parent = parent_of(&kpath);
            let mut pstat = crate::vfs::Stat::default();
            if !crate::vfs::stat_path(&parent, &mut pstat) { return ENOENT; }
            if !dac_check(pstat.mode, pstat.uid, pstat.gid, uid, gid, 0x2) { return ENOPERM; }
            crate::vfs::create(&kpath, uid, gid) as u64
        }
        SYS_UNLINK => {
            // a1=path_ptr, a2=path_len
            let path_len = frame.a2 as usize;
            if path_len == 0 || path_len > 4096 { return EINVAL; }
            if !valid_user_range(frame.a1, frame.a2) { return EINVAL; }
            let mut kpath = alloc::vec![0u8; path_len];
            unsafe { with_user_access(|| core::ptr::copy_nonoverlapping(
                frame.a1 as *const u8, kpath.as_mut_ptr(), path_len,
            )); }
            let uid = crate::task::current_task_uid();
            let gid = crate::task::current_task_gid();
            let parent = parent_of(&kpath);
            let mut pstat = crate::vfs::Stat::default();
            if !crate::vfs::stat_path(&parent, &mut pstat) { return ENOENT; }
            if !dac_check(pstat.mode, pstat.uid, pstat.gid, uid, gid, 0x2) { return ENOPERM; }
            crate::vfs::unlink(&kpath) as u64
        }
        SYS_MKDIR => {
            // a1=path_ptr, a2=path_len
            let path_len = frame.a2 as usize;
            if path_len == 0 || path_len > 4096 { return EINVAL; }
            if !valid_user_range(frame.a1, frame.a2) { return EINVAL; }
            let mut kpath = alloc::vec![0u8; path_len];
            unsafe { with_user_access(|| core::ptr::copy_nonoverlapping(
                frame.a1 as *const u8, kpath.as_mut_ptr(), path_len,
            )); }
            let uid = crate::task::current_task_uid();
            let gid = crate::task::current_task_gid();
            let parent = parent_of(&kpath);
            let mut pstat = crate::vfs::Stat::default();
            if !crate::vfs::stat_path(&parent, &mut pstat) { return ENOENT; }
            if !dac_check(pstat.mode, pstat.uid, pstat.gid, uid, gid, 0x2) { return ENOPERM; }
            crate::vfs::mkdir(&kpath, uid, gid) as u64
        }
        SYS_SERIAL_AVAIL => {
            if crate::keyboard::data_ready() || crate::serial::SERIAL.lock().data_ready() { 1 } else { 0 }
        }

        SYS_TASK_WAIT => {
            // a1 = target TaskId. Block until it exits; return its exit status
            // (lythos_abi::exit encoding, < 0x1_0000), or ENOENT if the task is
            // not found and has no retained exit record.
            crate::task::wait_for_task(frame.a1)
        }

        SYS_NANOSLEEP => {
            // a1 = nanoseconds. Rounded up to next ms; 0 ns just yields.
            let ns = frame.a1;
            let ms = (ns + 999_999) / 1_000_000;
            if ms == 0 {
                crate::task::yield_task();
            } else {
                let wake_tick = crate::apic::ticks() + ms;
                crate::task::sleep_until(wake_tick);
            }
            0
        }

        SYS_TASK_SETPRIORITY => {
            // a1 = TaskId, a2 = priority (0=low, 1=normal, 2=high)
            let id       = frame.a1;
            let priority = frame.a2 as u8;
            if crate::task::set_task_priority(id, priority) { 0 } else { EINVAL }
        }

        SYS_RENAME => {
            // a1=old_path_ptr, a2=old_path_len, a3=new_path_ptr, a4=new_path_len
            let old_len = frame.a2 as usize;
            let new_len = frame.a4 as usize;
            if old_len == 0 || old_len > 4096 || new_len == 0 || new_len > 4096 { return EINVAL; }
            if !valid_user_range(frame.a1, frame.a2) { return EINVAL; }
            if !valid_user_range(frame.a3, frame.a4) { return EINVAL; }
            let mut old_path = alloc::vec![0u8; old_len];
            let mut new_path = alloc::vec![0u8; new_len];
            unsafe { with_user_access(|| {
                core::ptr::copy_nonoverlapping(frame.a1 as *const u8, old_path.as_mut_ptr(), old_len);
                core::ptr::copy_nonoverlapping(frame.a3 as *const u8, new_path.as_mut_ptr(), new_len);
            }); }
            let uid = crate::task::current_task_uid();
            let gid = crate::task::current_task_gid();
            let old_parent = parent_of(&old_path);
            let new_parent = parent_of(&new_path);
            let mut pstat = crate::vfs::Stat::default();
            if !crate::vfs::stat_path(&old_parent, &mut pstat) { return ENOENT; }
            if !dac_check(pstat.mode, pstat.uid, pstat.gid, uid, gid, 0x2) { return ENOPERM; }
            if !crate::vfs::stat_path(&new_parent, &mut pstat) { return ENOENT; }
            if !dac_check(pstat.mode, pstat.uid, pstat.gid, uid, gid, 0x2) { return ENOPERM; }
            crate::vfs::rename(&old_path, &new_path) as u64
        }

        SYS_SEEK => {
            // a1=fd, a2=offset (i64 as u64), a3=whence (0=SET,1=CUR,2=END)
            let offset = frame.a2 as i64;
            let whence = frame.a3 as u32;
            crate::vfs::seek(frame.a1, offset, whence) as u64
        }

        SYS_PS => {
            // a1=buf_ptr (*mut PsEntry), a2=buf_capacity (max entries)
            // Entry layout (48 bytes):
            //   [0..8]   u64 id
            //   [8..16]  u64 state (1=running/ready, 2=blocked)
            //   [16]     u8  kind  (0=kernel, 1=user)
            //   [17]     u8  priority
            //   [18]     u8  name_len
            //   [19..24] _pad
            //   [24..40] name[16] null-padded
            //   [40..48] _pad
            let cap = frame.a2 as usize;
            if cap == 0 { return 0; }
            const ENTRY: usize = 48;
            let bytes = cap.saturating_mul(ENTRY);
            if !valid_user_range(frame.a1, bytes as u64) { return EINVAL; }
            let buf = frame.a1 as *mut u8;
            let n = crate::task::for_each_task_ps(|idx, id, state_raw, kind, priority, name| {
                if idx >= cap { return; }
                let entry = unsafe { buf.add(idx * ENTRY) };
                let name_len = name.iter().position(|&b| b == 0).unwrap_or(16) as u8;
                unsafe { with_user_access(|| {
                    core::ptr::write_unaligned(entry          as *mut u64, id);
                    core::ptr::write_unaligned(entry.add(8)   as *mut u64, state_raw);
                    core::ptr::write(entry.add(16), kind);
                    core::ptr::write(entry.add(17), priority);
                    core::ptr::write(entry.add(18), name_len);
                    core::ptr::write_bytes(entry.add(19), 0, 5);
                    core::ptr::copy_nonoverlapping(name.as_ptr(), entry.add(24), 16);
                    core::ptr::write_bytes(entry.add(40), 0, 8);
                }); }
            });
            n as u64
        }

        SYS_BRK => {
            // a1 = new break address (0 = query). Returns new/current break, or
            // EINVAL if the address would collide with the user stack region.
            //
            // Heap region: USERSPACE_HEAP_BASE .. 0x0000_7FFE_0000_0000
            // (stacks start at 0x0000_7FFF_0000_0000, kept well clear).
            const HEAP_BASE:  u64 = 0x0000_0004_0000_0000;
            const STACK_GUARD: u64 = 0x0000_7FFE_0000_0000;

            // Require a Memory capability with write access.
            let current_id = crate::task::current_task_id();
            let table_ptr  = crate::task::cap_table_ptr(current_id);
            if table_ptr.is_null() { return ENOPERM; }
            let table = unsafe { &*table_ptr };
            if !table.has_kind_with_rights(
                crate::cap::CapKind::Memory,
                crate::cap::CapRights::WRITE,
            ) {
                return ENOPERM;
            }

            let cur_brk = {
                let b = crate::task::current_task_brk();
                if b == 0 { HEAP_BASE } else { b }
            };

            let new_brk = frame.a1;
            if new_brk == 0 {
                // Query — return the current break.
                crate::task::set_current_task_brk(cur_brk);
                return cur_brk;
            }
            if new_brk <= cur_brk {
                // Shrink: unmap and free every whole page above the new break.
                // Pages below round_up(new_brk) stay mapped (may be partially
                // in use by the caller's heap).
                if new_brk < HEAP_BASE { return EINVAL; }
                let keep_top = (new_brk + 0xFFF) & !0xFFF;
                let old_top  = (cur_brk + 0xFFF) & !0xFFF;
                if let Some(pml4) = crate::task::current_page_table() {
                    let mut va = keep_top;
                    while va < old_top {
                        if let Some(pa) = crate::vmm::unmap_page_in(
                            crate::pmm::PhysAddr(pml4),
                            crate::vmm::VirtAddr(va),
                        ) {
                            crate::pmm::free_frame(pa);
                        }
                        va += 0x1000;
                    }
                }
                crate::task::set_current_task_brk(new_brk);
                return new_brk;
            }
            if new_brk >= STACK_GUARD { return EINVAL; }

            // Page-align: map every page that new_brk reaches into.
            let old_page_top = (cur_brk + 0xFFF) & !0xFFF;
            let new_page_top = (new_brk + 0xFFF) & !0xFFF;

            let flags = crate::vmm::PageFlags(
                crate::vmm::PageFlags::PRESENT.0
                | crate::vmm::PageFlags::WRITABLE.0
                | crate::vmm::PageFlags::USER.0
                | crate::vmm::PageFlags::NX.0
            );

            let mut va = old_page_top;
            while va < new_page_top {
                let Some(phys) = crate::pmm::alloc_frame() else {
                    // OOM: partial success — update brk to what was mapped.
                    crate::task::set_current_task_brk(va);
                    return va;
                };
                match crate::task::current_page_table() {
                    Some(pml4) => crate::vmm::map_page_in(
                        crate::pmm::PhysAddr(pml4),
                        crate::vmm::VirtAddr(va),
                        phys,
                        flags,
                    ),
                    None => crate::vmm::map_page(crate::vmm::VirtAddr(va), phys, flags),
                }
                va += 0x1000;
            }

            crate::task::set_current_task_brk(new_brk);
            new_brk
        }

        SYS_IPC_POLL => {
            // Non-blocking recv: a1=cap_handle, a2=buf_ptr, a3=buf_len
            let handle  = crate::cap::CapHandle(frame.a1);
            let buf_len = (frame.a3 as usize).min(crate::ipc::MSG_SIZE);
            if !valid_user_range(frame.a2, buf_len as u64) { return EINVAL; }
            let buf_ptr = frame.a2 as *mut u8;

            let current_id = crate::task::current_task_id();
            let table_ptr  = crate::task::cap_table_ptr(current_id);
            if table_ptr.is_null() { return ENOCAP; }
            let table = unsafe { &*table_ptr };

            let endpoint_idx = match table.get(handle) {
                Ok(c) if c.kind == crate::cap::CapKind::Ipc
                      && c.rights.has(crate::cap::CapRights::READ) => {
                    match crate::cap::get_object(c.object) {
                        Some(crate::cap::KernelObject::Ipc { endpoint_idx }) => *endpoint_idx,
                        _ => return ENOCAP,
                    }
                }
                Ok(_) => return ENOPERM,
                Err(_) => return ENOCAP,
            };

            let mut buf = [0u8; crate::ipc::MSG_SIZE];
            match crate::ipc::try_recv(endpoint_idx, &mut buf) {
                None    => EAGAIN,
                Some(n) => {
                    unsafe { with_user_access(|| {
                        core::ptr::copy_nonoverlapping(buf.as_ptr(), buf_ptr, n.min(buf_len));
                    }) };
                    n as u64
                }
            }
        }

        SYS_IPC_BIND => {
            // a1=cap_handle, a2=name_ptr, a3=name_len
            let handle   = crate::cap::CapHandle(frame.a1);
            let name_len = frame.a3 as usize;
            if name_len == 0 || name_len > 128 { return EINVAL; }
            if !valid_user_range(frame.a2, name_len as u64) { return EINVAL; }

            let current_id = crate::task::current_task_id();
            let table_ptr  = crate::task::cap_table_ptr(current_id);
            if table_ptr.is_null() { return ENOCAP; }
            let table = unsafe { &*table_ptr };

            let endpoint_idx = match table.get(handle) {
                Ok(c) if c.kind == crate::cap::CapKind::Ipc
                      && c.rights.has(crate::cap::CapRights::GRANT) => {
                    match crate::cap::get_object(c.object) {
                        Some(crate::cap::KernelObject::Ipc { endpoint_idx }) => *endpoint_idx,
                        _ => return ENOCAP,
                    }
                }
                Ok(_) => return ENOPERM,
                Err(_) => return ENOCAP,
            };

            let mut kname = [0u8; 128];
            unsafe { with_user_access(|| core::ptr::copy_nonoverlapping(
                frame.a2 as *const u8, kname.as_mut_ptr(), name_len,
            )); }
            let name = match core::str::from_utf8(&kname[..name_len]) {
                Ok(s) => s,
                Err(_) => return EINVAL,
            };

            if crate::ipc::bind_name(name, endpoint_idx) { 0 } else { ENOSYS }
        }

        SYS_IPC_LOOKUP => {
            // a1=name_ptr, a2=name_len, a3=rights_mask
            let name_len = frame.a2 as usize;
            if name_len == 0 || name_len > 128 { return EINVAL; }
            if !valid_user_range(frame.a1, name_len as u64) { return EINVAL; }

            let mut kname = [0u8; 128];
            unsafe { with_user_access(|| core::ptr::copy_nonoverlapping(
                frame.a1 as *const u8, kname.as_mut_ptr(), name_len,
            )); }
            let name = match core::str::from_utf8(&kname[..name_len]) {
                Ok(s) => s,
                Err(_) => return EINVAL,
            };

            let endpoint_idx = match crate::ipc::lookup_name(name) {
                Some(idx) => idx,
                None      => return ENOENT,
            };

            // Strip GRANT/REVOKE from the requested mask — lookup can't elevate rights.
            let rights = crate::cap::CapRights(
                (frame.a3 as u8) & (crate::cap::CapRights::READ.0 | crate::cap::CapRights::WRITE.0)
            );
            // Default to READ|WRITE if no valid bits were set.
            let rights = if rights.0 == 0 {
                crate::cap::CapRights(crate::cap::CapRights::READ.0 | crate::cap::CapRights::WRITE.0)
            } else {
                rights
            };

            let obj = crate::cap::create_object(
                crate::cap::KernelObject::Ipc { endpoint_idx }
            ).expect("SYS_IPC_LOOKUP: KoTable OOM");

            let current_id = crate::task::current_task_id();
            let table_ptr  = crate::task::cap_table_ptr(current_id);
            if table_ptr.is_null() { return ENOCAP; }
            let table = unsafe { &mut *table_ptr };

            let handle = crate::cap::create_root_cap(
                table,
                crate::cap::CapKind::Ipc,
                rights,
                obj,
            );
            handle.0
        }

        // ── IPC with timeout ─────────────────────────────────────────────────────

        SYS_IPC_RECV_TIMEOUT => {
            // a1=cap_handle, a2=buf_ptr, a3=buf_len, a4=timeout_ms
            let handle     = crate::cap::CapHandle(frame.a1);
            let buf_len    = (frame.a3 as usize).min(crate::ipc::MSG_SIZE);
            let timeout_ms = frame.a4;
            if !valid_user_range(frame.a2, buf_len as u64) { return EINVAL; }
            let buf_ptr = frame.a2 as *mut u8;

            let current_id = crate::task::current_task_id();
            let table_ptr  = crate::task::cap_table_ptr(current_id);
            if table_ptr.is_null() { return ENOCAP; }
            let table = unsafe { &*table_ptr };

            let endpoint_idx = match table.get(handle) {
                Ok(c) if c.kind == crate::cap::CapKind::Ipc
                      && c.rights.has(crate::cap::CapRights::READ) => {
                    match crate::cap::get_object(c.object) {
                        Some(crate::cap::KernelObject::Ipc { endpoint_idx }) => *endpoint_idx,
                        _ => return ENOCAP,
                    }
                }
                Ok(_) => return ENOPERM,
                Err(_) => return ENOCAP,
            };

            let mut buf = [0u8; crate::ipc::MSG_SIZE];
            match crate::ipc::recv_timeout(endpoint_idx, &mut buf, timeout_ms) {
                Err(()) => EAGAIN,
                Ok(n) => {
                    unsafe { with_user_access(|| {
                        core::ptr::copy_nonoverlapping(buf.as_ptr(), buf_ptr, n.min(buf_len));
                    }) };
                    n as u64
                }
            }
        }

        SYS_IPC_SEND_TIMEOUT => {
            // a1=cap_handle, a2=msg_ptr, a3=msg_len, a4=timeout_ms
            let handle     = crate::cap::CapHandle(frame.a1);
            let msg_len    = (frame.a3 as usize).min(crate::ipc::MSG_SIZE);
            let timeout_ms = frame.a4;
            if !valid_user_range(frame.a2, msg_len as u64) { return EINVAL; }
            let msg_ptr = frame.a2 as *const u8;

            let current_id = crate::task::current_task_id();
            let table_ptr  = crate::task::cap_table_ptr(current_id);
            if table_ptr.is_null() { return ENOCAP; }
            let table = unsafe { &*table_ptr };

            let endpoint_idx = match table.get(handle) {
                Ok(c) if c.kind == crate::cap::CapKind::Ipc
                      && c.rights.has(crate::cap::CapRights::WRITE) => {
                    match crate::cap::get_object(c.object) {
                        Some(crate::cap::KernelObject::Ipc { endpoint_idx }) => *endpoint_idx,
                        _ => return ENOCAP,
                    }
                }
                Ok(_) => return ENOPERM,
                Err(_) => return ENOCAP,
            };

            let msg = unsafe { with_user_access(|| core::slice::from_raw_parts(msg_ptr, msg_len)) };
            match crate::ipc::send_timeout(endpoint_idx, msg, timeout_ms) {
                Ok(()) => 0,
                Err(()) => EAGAIN,
            }
        }

        SYS_TIME_EPOCH => {
            crate::time::epoch_ms()
        }

        SYS_GETUID => crate::task::current_task_uid() as u64,

        SYS_GETGID => crate::task::current_task_gid() as u64,

        SYS_SETUID => {
            let new_uid = frame.a1 as u32;
            let cur_uid = crate::task::current_task_uid();
            if cur_uid != 0 && new_uid != cur_uid { return ENOPERM; }
            crate::task::set_current_task_uid(new_uid);
            0
        }

        SYS_SETGID => {
            let new_gid = frame.a1 as u32;
            let cur_uid = crate::task::current_task_uid();
            let cur_gid = crate::task::current_task_gid();
            if cur_uid != 0 && new_gid != cur_gid { return ENOPERM; }
            crate::task::set_current_task_gid(new_gid);
            0
        }

        // Syscalls 50-54 (UDP socket API) are retired — see the SYS_SOCKET-era
        // gap comment above. They fall through to the ENOSYS default arm.

        // ── Userspace device-driver framework ─────────────────────────────────
        //
        // All device authority resolves through a CapKind::Device capability
        // (SYS_DEV_CLAIM is Rollback-gated so only lythd claims devices). The
        // handlers gate-before-args in crate::device, returning ENOPERM to a
        // caller lacking the cap before any argument is examined.

        SYS_DEV_CLAIM => crate::device::sys_dev_claim(frame.a1, frame.a2),
        SYS_DEV_CFG_READ => crate::device::sys_dev_cfg_read(frame.a1, frame.a2),
        SYS_DEV_MMIO_MAP => crate::device::sys_dev_mmio_map(frame.a1, frame.a2, frame.a3),
        SYS_DEV_DMA_ALLOC => crate::device::sys_dev_dma_alloc(frame.a1, frame.a2, frame.a3, frame.a4),
        SYS_DEV_IRQ_WAIT => crate::device::sys_dev_irq_wait(frame.a1),
        SYS_DEV_IRQ_ACK  => crate::device::sys_dev_irq_ack(frame.a1),

        SYS_POWEROFF => {
            crate::acpi::shutdown();
        }

        SYS_MOUNT => {
            // a1=at_ptr, a2=at_len, a3=source, a4=flags.
            //
            // Capability gate FIRST, on the syscall boundary: mounting requires
            // CapKind::Filesystem with the WRITE right — no ambient authority,
            // and no argument validation before the authority check (a caller
            // without the cap learns nothing about the mount table).
            let current_id = crate::task::current_task_id();
            let table_ptr  = crate::task::cap_table_ptr(current_id);
            if table_ptr.is_null() { return ENOPERM; }
            let table = unsafe { &*table_ptr };
            if !table.has_kind_with_rights(
                crate::cap::CapKind::Filesystem,
                crate::cap::CapRights::WRITE,
            ) {
                return ENOPERM;
            }

            let path_len = frame.a2 as usize;
            if path_len == 0 || path_len > 4096 { return EINVAL; }
            if !valid_user_range(frame.a1, frame.a2) { return EINVAL; }
            let mut kpath = alloc::vec![0u8; path_len];
            unsafe { with_user_access(|| core::ptr::copy_nonoverlapping(
                frame.a1 as *const u8, kpath.as_mut_ptr(), path_len,
            )); }
            let Ok(at) = core::str::from_utf8(&kpath) else { return EINVAL; };
            crate::vfs::mount(at, frame.a3, frame.a4) as u64
        }

        SYS_UNMOUNT => {
            // a1=at_ptr, a2=at_len.
            //
            // Capability gate FIRST, identically to SYS_MOUNT: tearing a mount
            // down is the same Filesystem+WRITE authority as making one — no
            // ambient authority, and no argument validation before the check
            // (a caller without the cap learns nothing about the mount table).
            let current_id = crate::task::current_task_id();
            let table_ptr  = crate::task::cap_table_ptr(current_id);
            if table_ptr.is_null() { return ENOPERM; }
            let table = unsafe { &*table_ptr };
            if !table.has_kind_with_rights(
                crate::cap::CapKind::Filesystem,
                crate::cap::CapRights::WRITE,
            ) {
                return ENOPERM;
            }

            let path_len = frame.a2 as usize;
            if path_len == 0 || path_len > 4096 { return EINVAL; }
            if !valid_user_range(frame.a1, frame.a2) { return EINVAL; }
            let mut kpath = alloc::vec![0u8; path_len];
            unsafe { with_user_access(|| core::ptr::copy_nonoverlapping(
                frame.a1 as *const u8, kpath.as_mut_ptr(), path_len,
            )); }
            let Ok(at) = core::str::from_utf8(&kpath) else { return EINVAL; };
            crate::vfs::unmount(at) as u64
        }

        SYS_SYMLINK => {
            // a1=target_ptr, a2=target_len, a3=link_ptr, a4=link_len
            let target_len = frame.a2 as usize;
            let link_len = frame.a4 as usize;
            if target_len == 0 || target_len > 4096 { return EINVAL; }
            if link_len == 0 || link_len > 4096 { return EINVAL; }
            if !valid_user_range(frame.a1, frame.a2) { return EINVAL; }
            if !valid_user_range(frame.a3, frame.a4) { return EINVAL; }
            let mut target = alloc::vec![0u8; target_len];
            let mut link = alloc::vec![0u8; link_len];
            unsafe { with_user_access(|| {
                core::ptr::copy_nonoverlapping(frame.a1 as *const u8, target.as_mut_ptr(), target_len);
                core::ptr::copy_nonoverlapping(frame.a3 as *const u8, link.as_mut_ptr(), link_len);
            }); }
            // Same DAC as SYS_CREATE: write permission on the link's parent.
            let uid = crate::task::current_task_uid();
            let gid = crate::task::current_task_gid();
            let parent = parent_of(&link);
            let mut pstat = crate::vfs::Stat::default();
            if !crate::vfs::stat_path(&parent, &mut pstat) { return ENOENT; }
            if !dac_check(pstat.mode, pstat.uid, pstat.gid, uid, gid, 0x2) { return ENOPERM; }
            crate::vfs::symlink(&target, &link) as u64
        }

        SYS_READLINK => {
            // a1=path_ptr, a2=path_len, a3=buf_ptr, a4=buf_len
            let path_len = frame.a2 as usize;
            let buf_len = frame.a4 as usize;
            if path_len == 0 || path_len > 4096 { return EINVAL; }
            if buf_len > 4096 { return EINVAL; }
            if !valid_user_range(frame.a1, frame.a2) { return EINVAL; }
            if buf_len > 0 && !valid_user_range(frame.a3, frame.a4) { return EINVAL; }
            let mut kpath = alloc::vec![0u8; path_len];
            unsafe { with_user_access(|| core::ptr::copy_nonoverlapping(
                frame.a1 as *const u8, kpath.as_mut_ptr(), path_len,
            )); }
            let mut kbuf = alloc::vec![0u8; buf_len];
            let r = crate::vfs::readlink(&kpath, &mut kbuf);
            if r > 0 && buf_len > 0 {
                let n = (r as usize).min(buf_len);
                unsafe { with_user_access(|| core::ptr::copy_nonoverlapping(
                    kbuf.as_ptr(), frame.a3 as *mut u8, n,
                )); }
            }
            r as u64
        }

        SYS_STORE_REMOVE => {
            // a1=path_ptr, a2=path_len.
            //
            // Capability gate FIRST (as SYS_MOUNT): reclaiming an unreferenced
            // store entry is store-owner authority — CapKind::Filesystem with
            // WRITE. A builder holding no such cap cannot reclaim. This removes
            // a whole path BELOW the seal (it never opens a sealed file for
            // write); the seal itself remains unliftable.
            let current_id = crate::task::current_task_id();
            let table_ptr  = crate::task::cap_table_ptr(current_id);
            if table_ptr.is_null() { return ENOPERM; }
            let table = unsafe { &*table_ptr };
            if !table.has_kind_with_rights(
                crate::cap::CapKind::Filesystem,
                crate::cap::CapRights::WRITE,
            ) {
                return ENOPERM;
            }

            let path_len = frame.a2 as usize;
            if path_len == 0 || path_len > 4096 { return EINVAL; }
            if !valid_user_range(frame.a1, frame.a2) { return EINVAL; }
            let mut kpath = alloc::vec![0u8; path_len];
            unsafe { with_user_access(|| core::ptr::copy_nonoverlapping(
                frame.a1 as *const u8, kpath.as_mut_ptr(), path_len,
            )); }
            crate::vfs::store_remove_tree(&kpath) as u64
        }

        _ => ENOSYS,
    }
}

// ── MSR helpers ───────────────────────────────────────────────────────────────

#[inline]
unsafe fn wrmsr(msr: u32, value: u64) {
    unsafe {
        core::arch::asm!(
            "wrmsr",
            in("ecx") msr,
            in("eax") value as u32,
            in("edx") (value >> 32) as u32,
            options(nostack, nomem),
        );
    }
}

#[inline]
unsafe fn rdmsr(msr: u32) -> u64 {
    let lo: u32;
    let hi: u32;
    unsafe {
        core::arch::asm!(
            "rdmsr",
            in("ecx") msr,
            out("eax") lo,
            out("edx") hi,
            options(nostack, nomem),
        );
    }
    (hi as u64) << 32 | lo as u64
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Initialise the syscall machinery.
///
/// 1. Enable `syscall`/`sysret` via `IA32_EFER.SCE` (bit 0).
/// 2. Configure `IA32_STAR`: kernel CS = 0x08, sysretq base = 0x10.
/// 3. Point `IA32_LSTAR` at `syscall_entry`.
/// 4. Set `IA32_FMASK` to clear IF on entry.
/// 5. Enable SMEP (CR4 bit 20) — prevents kernel-mode execution of user pages.
///
/// Must be called after `gdt::init()` and `vmm::init()`.
pub fn init() {
    unsafe {
        // 1. Set SCE in EFER
        let efer = rdmsr(IA32_EFER);
        wrmsr(IA32_EFER, efer | 1);

        // 2. STAR: bits[47:32] = kernel CS (0x08), bits[63:48] = sysretq base (0x10)
        //    sysretq: CS = 0x10 + 16 = 0x20 | RPL=3,  SS = 0x10 + 8 = 0x18 | RPL=3
        let star = (0x0010u64 << 48) | (0x0008u64 << 32);
        wrmsr(IA32_STAR, star);

        // 3. LSTAR = syscall_entry
        wrmsr(IA32_LSTAR, syscall_entry as *const () as u64);

        // 4. FMASK: clear IF (bit 9) on syscall entry
        wrmsr(IA32_FMASK, 1 << 9);

        // 5. Enable SMEP (CR4[20]) and SMAP (CR4[21]) if CPUID leaf 7 reports support.
        // CPUID.07H:EBX[7]  = SMEP,  EBX[20] = SMAP.
        // STAC/CLAC are only valid on CPUs that advertise SMAP; SMAP_ENABLED
        // gates their emission in the syscall handlers.
        {
            let ebx: u32;
            core::arch::asm!(
                "push rbx",
                "xor eax, eax",
                "xor ecx, ecx",
                "mov eax, 7",
                "cpuid",
                "mov {0:e}, ebx",
                "pop rbx",
                out(reg) ebx,
                lateout("eax") _,
                lateout("ecx") _,
                lateout("edx") _,
                options(nostack),
            );
            let mut cr4: u64;
            core::arch::asm!("mov {0}, cr4", out(reg) cr4, options(nostack, nomem));
            if (ebx >> 7) & 1 == 1  { cr4 |= 1u64 << 20; } // SMEP
            if (ebx >> 20) & 1 == 1 {
                cr4 |= 1u64 << 21;                           // SMAP
                SMAP_ENABLED.store(true, core::sync::atomic::Ordering::Relaxed);
            }
            core::arch::asm!("mov cr4, {0}", in(reg) cr4, options(nostack, nomem));
        }
    }
}

/// Enter ring-3 at `entry` with the user stack at `stack`.
///
/// Before the `iretq`, sets `SYSCALL_KERN_RSP` and `tss::RSP0` to the
/// current task's kernel-stack top so that subsequent syscalls and hardware
/// interrupts in ring 3 land on the correct kernel stack.  Never returns.
pub fn enter_userspace(entry: crate::vmm::VirtAddr, stack: crate::vmm::VirtAddr) -> ! {
    let kstack_top = crate::task::current_kernel_stack_top();
    if kstack_top != 0 {
        crate::tss::set_rsp0(kstack_top);
        unsafe { SYSCALL_KERN_RSP = kstack_top; }
    }
    unsafe { enter_userspace_asm(entry.as_u64(), stack.as_u64()) }
}
