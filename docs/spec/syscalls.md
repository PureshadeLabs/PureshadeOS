# Syscall reference

## Entry mechanism

User code invokes the `syscall` instruction. The CPU transfers control to
`syscall_entry` (address stored in the LSTAR MSR). FMASK clears IF on entry,
so interrupts are disabled for the duration of the syscall handler.
`sysretq` restores the user RIP from RCX and RFLAGS from R11.

### Register conventions

| Register | Role on entry | Role on return |
|----------|--------------|----------------|
| RAX | Syscall number | Return value |
| RDI | Argument 1 (a1) | — |
| RSI | Argument 2 (a2) | — |
| RDX | Argument 3 (a3) | — |
| R10 | Argument 4 (a4) | — |
| R8  | Argument 5 (a5) | — |
| R9  | Argument 6 (a6) | — |
| RCX | User RIP (saved by CPU) | — |
| R11 | User RFLAGS (saved by CPU) | — |

RCX and R11 are clobbered by the `syscall` instruction. User code passing 4+
arguments must use R10 for a4, not RCX.

---

## Error codes

Errors are returned in RAX as large `u64` values (two's-complement negative
`i64`). User code should cast RAX to `i64` and check `< 0`.

| Value (u64) | i64 | Name | Meaning |
|---|---|---|---|
| `0xFFFF_FFFF_FFFF_FFFF` | -1  | `ENOSYS`  | Unknown or unassigned syscall number |
| `0xFFFF_FFFF_FFFF_FFFE` | -2  | `ENOCAP`  | Invalid or stale capability handle |
| `0xFFFF_FFFF_FFFF_FFFD` | -3  | `ENOPERM` | Capability rights insufficient |
| `0xFFFF_FFFF_FFFF_FFFC` | -4  | `EINVAL`  | Invalid argument |
| `0xFFFF_FFFF_FFFF_FFFB` | -5  | `ENOENT`  | No such file or directory |
| `0xFFFF_FFFF_FFFF_FFFA` | -6  | `EBADF`   | Bad file descriptor |
| `0xFFFF_FFFF_FFFF_FFF9` | -7  | `EAGAIN`  | Resource temporarily unavailable (IPC timeout, non-blocking empty/full) |
| `0xFFFF_FFFF_FFFF_FFF8` | -8  | `ENOTDIR` | Path component is not a directory (SYS_CREATE, SYS_MKDIR, SYS_RENAME) |
| `0xFFFF_FFFF_FFFF_FFF7` | -9  | `ENOMNT`  | Filesystem not mounted; returned by all VFS syscalls when RFS is absent |
| `0xFFFF_FFFF_FFFF_FFF6` | -10 | `EMFILE`  | Too many open file descriptors (SYS_OPEN, SYS_CREATE) |
| `0xFFFF_FFFF_FFFF_FFF5` | -11 | `EEXIST`  | File or directory already exists (SYS_CREATE, SYS_MKDIR, SYS_RENAME) |
| `0xFFFF_FFFF_FFFF_FFF4` | -12 | `ENOSPC`  | No space left on device (SYS_CREATE, SYS_MKDIR) |
| `0xFFFF_FFFF_FFFF_FFF3` | -13 | `EMOUNTED` | A mount already exists at the mount point (SYS_MOUNT) |
| `0xFFFF_FFFF_FFFF_FFF2` | -14 | `EROFS`   | Write to a read-only / sealed path — read-only-after-realize on the store mount (SYS_WRITE, SYS_CREATE, SYS_MKDIR, SYS_UNLINK, SYS_RENAME) |
| `0xFFFF_FFFF_FFFF_FFF1` | -15 | `EISDIR`  | Path is a directory where a regular file is required (SYS_OPEN) |
| `0xFFFF_FFFF_FFFF_FFF0` | -16 | `ENOTEMPTY` | Directory not empty (SYS_RENAME onto a non-empty directory; future rmdir) |
| `0xFFFF_FFFF_FFFF_FFEF` | -17 | `EIO`     | I/O or integrity fault — block device error, failed authentication, corrupt on-disk structure (any FS syscall, SYS_MOUNT). Distinct from ENOMNT |

`SYSCALL_MAX = 66`. Syscall numbers above 66 and the unassigned gaps 49, 50–54
and 59 always return `ENOSYS` (59 = retired `SYS_UNSEAL`; 50–54 = retired UDP
socket API — networking moved to a userspace `netd` driver over the device
framework; never reuse any of these numbers).

---

## Syscall table

| Nr | Name | Brief |
|----|------|-------|
| 0 | `SYS_YIELD` | Voluntarily yield CPU |
| 1 | `SYS_TASK_EXIT` | Terminate calling task |
| 2 | `SYS_MMAP` | Map a new anonymous page |
| 3 | `SYS_MUNMAP` | Unmap a virtual page |
| 4 | `SYS_CAP_GRANT` | Copy cap to another task with reduced rights |
| 5 | `SYS_CAP_REVOKE` | Cascade-revoke a capability |
| 6 | `SYS_IPC_SEND` | Send message to IPC endpoint |
| 7 | `SYS_IPC_RECV` | Receive message from IPC endpoint |
| 8 | `SYS_IPC_CREATE` | Create new IPC endpoint |
| 9 | `SYS_ROLLBACK` | Trigger kernel rollback |
| 10 | `SYS_EXEC` | Load ELF and spawn new task |
| 11 | `SYS_LOG` | Write to kernel serial console |
| 12 | `SYS_IPC_SEND_CAP` | Send message and transfer capability |
| 13 | `SYS_IPC_RECV_CAP` | Receive message and accept capability |
| 14 | `SYS_SERIAL_READ` | Read bytes from COM1 |
| 15 | `SYS_TIME` | Milliseconds since boot |
| 16 | `SYS_TASK_STATUS` | Query task liveness |
| 17 | `SYS_TASK_LIST` | List all live tasks |
| 18 | `SYS_MEM_STAT` | Free physical frame count |
| 19 | `SYS_TASK_KILL` | Terminate a task by ID |
| 20 | `SYS_BLK_READ` | Read 512-byte sector from block device |
| 21 | `SYS_BLK_WRITE` | Write 512-byte sector to block device |
| 22 | `SYS_OPEN` | Open a file |
| 23 | `SYS_READ` | Read from open fd |
| 24 | `SYS_WRITE` | Write to open fd |
| 25 | `SYS_CLOSE` | Close fd |
| 26 | `SYS_STAT` | Query file metadata |
| 27 | `SYS_READDIR` | Read directory entries |
| 28 | `SYS_CREATE` | Create a new empty file |
| 29 | `SYS_UNLINK` | Delete a file |
| 30 | `SYS_SERIAL_AVAIL` | Check serial/keyboard data availability |
| 31 | `SYS_TASK_WAIT` | Block until a task exits |
| 32 | `SYS_MKDIR` | Create a directory |
| 33 | `SYS_NANOSLEEP` | Sleep for at least N nanoseconds |
| 34 | `SYS_TASK_SETPRIORITY` | Set task scheduling priority |
| 35 | `SYS_RENAME` | Rename or move a file |
| 36 | `SYS_SEEK` | Seek within open fd |
| 37 | `SYS_PS` | List tasks with extended info |
| 38 | `SYS_BRK` | Set or query heap break |
| 39 | `SYS_IPC_POLL` | Non-blocking IPC recv |
| 40 | `SYS_IPC_BIND` | Bind name to IPC endpoint |
| 41 | `SYS_IPC_LOOKUP` | Look up named IPC endpoint |
| 42 | `SYS_IPC_RECV_TIMEOUT` | IPC recv with millisecond timeout |
| 43 | `SYS_IPC_SEND_TIMEOUT` | IPC send with millisecond timeout |
| 44 | `SYS_TIME_EPOCH` | Unix epoch milliseconds (UTC) |
| 45 | `SYS_GETUID` | Return calling task's UID |
| 46 | `SYS_GETGID` | Return calling task's GID |
| 47 | `SYS_SETUID` | Set calling task's UID |
| 48 | `SYS_SETGID` | Set calling task's GID |
| 49 | *(unassigned)* | Return `ENOSYS` |
| 50–54 | *(retired)* | Was UDP socket API — networking is now userspace (`netd`); return `ENOSYS`, never reuse |
| 55 | `SYS_POWEROFF` | Power off the machine |
| 56 | `SYS_MOUNT` | Mount a filesystem backend at a path |
| 57 | `SYS_SYMLINK` | Create a symbolic link |
| 58 | `SYS_READLINK` | Read a symlink's target |
| 59 | *(retired)* | Was `SYS_UNSEAL`; return `ENOSYS`, never reuse |
| 60 | `SYS_STORE_REMOVE` | Remove an unreferenced store path |
| 61 | `SYS_DEV_CLAIM` | Claim a registered device (Rollback-gated); mint its Device cap |
| 62 | `SYS_DEV_CFG_READ` | Read the device's PCI config space (Device-cap gated) |
| 63 | `SYS_DEV_MMIO_MAP` | Map a device MMIO BAR uncacheable (Device-cap gated) |
| 64 | `SYS_DEV_DMA_ALLOC` | Allocate a DMA buffer, return its phys addr (Device-cap gated) |
| 65 | `SYS_DEV_IRQ_WAIT` | Block until the device raises its IRQ (Device-cap gated) |
| 66 | `SYS_DEV_IRQ_ACK` | Acknowledge/unmask the device IRQ (Device-cap gated) |

---

## Userspace device-driver framework (61–66)

A ring-3 driver owns exactly one PCI device via an unforgeable `CapKind::Device`
capability. The kernel enumerates devices it does not drive into a registry;
`SYS_DEV_CLAIM` (gated on the Rollback cap, so only lythd claims) mints one
Device cap per device, which lythd hands to the intended driver. The driver then
reads its config space, maps its MMIO BARs (uncacheable), allocates DMA buffers,
and waits on its IRQ — all gated on that one cap, gate-before-args (a caller with
no Device cap gets `ENOPERM` before any argument is examined). No ambient
authority: with no Device cap a process can touch no device. virtio-blk stays
kernel-owned and is deliberately not registered. DMA is trusted-for-DMA (no
IOMMU) but routed through `SYS_DEV_DMA_ALLOC` so an IOMMU can later be programmed
there. IRQ model: on the device IRQ the kernel masks the IOAPIC line, wakes the
waiter, and requires `SYS_DEV_IRQ_ACK` (after the driver clears the device ISR)
to unmask — so a level-triggered PCI line does not storm.

---

## Syscall definitions

### SYS_YIELD — 0

Voluntarily yield the CPU to the next ready task.

**Arguments:** none  
**Returns:** 0

The scheduler runs immediately; the calling task is placed at the back of the
ready queue and resumes when scheduled again.

---

### SYS_TASK_EXIT — 1

Terminate the calling task with an exit code.

**Arguments:**
- a1 — exit code; only the low 8 bits are meaningful (0..=255). `0` = success.

**Returns:** never

Marks the task Dead, **retains its exit status** for a later `SYS_TASK_WAIT`,
and switches to the next ready task. The task's kernel stack and capability
table are freed by the reaper (`sweep_dead`). User mappings are not reclaimed
(no reference counting on page tables yet).

#### Exit-status encoding

The status a waiter reads back is a `u32` status word (mirrored in
`abi/lythos-abi/src/exit.rs`):

| Case | Encoding |
|---|---|
| Normal exit (`SYS_TASK_EXIT code`) | bit 8 clear; low 8 bits = `code` (0..=255) |
| Killed (`SYS_TASK_KILL`) | bit 8 set (`EXIT_ABNORMAL = 0x100`); low byte = `0` (`EXIT_REASON_KILLED`) |
| Faulted (unrecoverable CPU exception) | bit 8 set; low byte = `1` (`EXIT_REASON_FAULT`) |

Every status word is `< 0x1_0000`, far below the error-sentinel floor
(`ERR_MIN`), so `SYS_TASK_WAIT` can return a status directly in RAX and the
caller still distinguishes it from an error with the errno predicate.

#### Retention / reaping model

The exit status lives in an **exit-record table** in the kernel, separate from
the `Task` struct — so the Task itself can be reaped (stack + page table freed)
the moment it dies, while the status survives. The model is **reap-required with
a spawner-held claim**: each record is owned by the task that spawned the child.

- **Created** when the task exits (`SYS_TASK_EXIT`), is killed
  (`SYS_TASK_KILL`), or faults. The record is tagged with the id of the task
  that spawned the child (its parent, captured at spawn time).
- **Consumed** (removed) by the first `SYS_TASK_WAIT` that reads it — one
  waiter reaps one exit.
- **Cascade-freed on spawner death.** When a task dies, the unreaped records of
  any children *it* spawned are freed — the only task entitled to reap them is
  gone.
- **Never evicted.** The table is a heap `Vec`, not a fixed-size ring. A record
  is *never* dropped to make room for a newer one, so a real exit status is
  never silently lost. Capacity is bounded by live spawners and their
  outstanding unreaped children, not by a fixed size.

This replaces the earlier fixed 64-record ring with round-robin eviction. Under
that scheme an evicted record was indistinguishable from a task that never
existed — both returned `ENOENT` — so a builder that exited with, say, code 5
could be reported to the executor that spawned it as "not found," turning a
build *failure* into a silent lookup failure. With no eviction, a live spawner
that waits on a child it spawned **always** reads that child's real status, no
matter how many unrelated tasks exited in between. `SYS_TASK_WAIT` now returns
`ENOENT` only for a child that never existed, was already reaped, or whose
spawner has since died (cascade-freed).

---

### SYS_MMAP — 2

Map a new anonymous page into the calling task's address space. The kernel
allocates a fresh physical frame from the PMM — userspace cannot specify a
physical address. Naming a physical address would bypass the capability model:
physical and MMIO mapping is gated by a `Device` capability and is not covered
by this syscall.

**Arguments:**
- a1 — virtual address (must be 4 KiB-aligned)
- a2 — **reserved** — must be 0; ignored by the kernel
- a3 — page flags (bit 0 = Present, bit 1 = Writable, bit 63 = NX); bit 2
       (User) is forced set by the kernel regardless of a3

**Returns:** 0 on success; `EINVAL` on argument errors; `ENOPERM` if caller
holds no `Memory` capability with `WRITE` right

**Restrictions:**
- a1 must be 4 KiB-aligned.
- a1 must be ≥ `0x4000_0000` (1 GiB). Addresses below this are within the
  kernel's 2 MiB huge-page identity-map region; the kernel rejects them to
  prevent corruption of shared page-table entries.
- a1 must be < `0x0000_8000_0000_0000` (canonical user/kernel split).
- The virtual address must not already be mapped by this task (double-map
  returns `EINVAL`).
- Returns `EINVAL` if the PMM has no free frames.

The caller must hold a `CapKind::Memory` capability with `WRITE` right.
Currently a Memory capability grants access to the entire PMM pool; per-range
restriction is a planned improvement.

---

### SYS_MUNMAP — 3

Unmap a virtual page from the calling task's address space.

**Arguments:**
- a1 — virtual address (must be 4 KiB-aligned, within the user range, and
       previously mapped by this task)

**Returns:** 0 on success; `EINVAL` if unaligned, out of user range, or not
previously mapped by this task

Issues `invlpg` after unmapping. The underlying physical frame is **not**
freed; the caller is responsible for frame ownership.

---

### SYS_CAP_GRANT — 4

Copy a capability to another task with reduced rights.

**Arguments:**
- a1 — source capability handle in the calling task's table
- a2 — target task ID
- a3 — rights mask (low bits of u64, values 0–15: READ=1, WRITE=2, GRANT=4,
       REVOKE=8); result rights = source\_rights & mask

**Returns:** new handle in the target task's table, or error

**Errors:** `ENOCAP` if source handle invalid; `ENOPERM` if source lacks
`GRANT` right; `EINVAL` if target task does not exist or is self.

See [capabilities.md](capabilities.md) for rights values.

---

### SYS_CAP_REVOKE — 5

Cascade-revoke a capability: removes it from the calling task's table and
recursively revokes all capabilities derived from it across all live tasks.

**Arguments:**
- a1 — capability handle to revoke

**Returns:** 0 on success; `ENOCAP` if handle invalid; `ENOPERM` if caller
lacks `REVOKE` right

Revocation is always cascading. All handles granted (directly or transitively)
from this capability are also removed. The kernel walks all live tasks to find
derived handles. See [capabilities.md](capabilities.md) for `cap_cascade_revoke`.

---

### SYS_IPC_SEND — 6

Send a message to an IPC endpoint.

**Arguments:**
- a1 — IPC capability handle
- a2 — pointer to message buffer in user address space
- a3 — message length in bytes (max 64)

**Returns:** 0 on success; `ENOCAP` if handle invalid; `ENOPERM` if lacks
`WRITE` right

If the ring buffer is full the calling task blocks until a receiver drains the
ring. The kernel validates that `[a2, a2+a3)` is within user address space
before copying. See [ipc.md](ipc.md).

---

### SYS_IPC_RECV — 7

Receive a message from an IPC endpoint.

**Arguments:**
- a1 — IPC capability handle
- a2 — pointer to destination buffer in user address space
- a3 — buffer length in bytes

**Returns:** bytes copied (up to 64); `ENOCAP` / `ENOPERM` on error

If the ring buffer is empty the calling task blocks until a sender writes a
message. Return value is `min(a3, MSG_SIZE)`. See [ipc.md](ipc.md).

---

### SYS_IPC_CREATE — 8

Create a new IPC endpoint and return a capability to it.

**Arguments:** none  
**Returns:** new IPC capability handle with all rights (`ALL = 15`)

Allocates one 4 KiB physical page for the ring buffer and maps it into the
kernel IPC window at `0xFFFF_D000_0000_0000 + idx * 4096`.

---

### SYS_ROLLBACK — 9

Invoke the kernel rollback mechanism.

**Arguments:** none  
**Returns:** never on success; `ENOPERM` if caller holds no `CapKind::Rollback`
capability

Currently a stub (panics with "rollback invoked"). Full checkpoint/restore is
not yet implemented.

---

### SYS_EXEC — 10

Load an ELF binary from caller memory and spawn a new userspace task.

**Arguments:**
- a1 — pointer to ELF data in caller's address space
- a2 — ELF data length in bytes
- a3 — pointer to capability handle array (`u64[]`) in caller's address space
- a4 — number of capabilities to inherit (entries in the cap array)
- a5 — pointer to argv buffer in caller's address space (0 if no argv)
- a6 — total byte length of the argv buffer (0 if no argv)

**Returns:** new `TaskId` on success; error code on failure

**argv format:** flat null-terminated strings — `"arg0\0arg1\0..."`. The
kernel splits on null bytes to produce the argument list. `a6` must be ≤ 4000
bytes; `a5` is ignored when `a6 == 0`.

**Initial stack frame:** the new task's `rsp` is 16-byte aligned and points
at a SysV-style frame the kernel wrote at the top of the task's stack
(N = argc):

| Offset from rsp | Contents |
|---|---|
| `+0` | argc |
| `+8 … +8·N` | argv[0..N-1] — pointers into the string area |
| `+8·(N+1)` | 0 (argv terminator) |
| `+8·(N+2)` | 0 (envp terminator — Lythos has no environment) |
| `+8·(N+3) … +8·(N+6)` | auxv: `AT_PAGESZ` (6), 4096, `AT_NULL` (0), 0 |
| `+8·(N+7)` | string data: `"argv[0]\0argv[1]\0…"` |

The strings live above the initial `rsp` and stay valid for the task's
lifetime (the stack grows down). Runtime access: `lythos_rt::entry!`
captures the frame; `lythos_rt::args` / lythos-libstd `env::args()` expose
it. With no argv the frame is present with argc = 0.

The kernel copies the ELF data, capability list, and argv from caller address
space, parses and loads the ELF, allocates a user stack, writes the initial
ABI frame, and spawns a kernel task. The new task's capability table is
populated from the handle array: slot 0 = caps[0], slot 1 = caps[1], etc.

See [elf.md](elf.md) for load details.

---

### SYS_LOG — 11

Write a UTF-8 string to the kernel serial console.

**Arguments:**
- a1 — pointer to string in user address space
- a2 — string length in bytes

**Returns:** 0

Debug aid only. No capability check. Output goes to COM1 and is not buffered.

---

### SYS_IPC_SEND_CAP — 12

Send a message and transfer a capability to an IPC endpoint atomically.

**Arguments:**
- a1 — IPC capability handle
- a2 — pointer to message buffer in user address space
- a3 — message length in bytes (max 64)
- a4 — handle of the capability to transfer

**Returns:** 0 on success; `ENOCAP` / `ENOPERM` on error

See [ipc.md](ipc.md) for the capability-transfer protocol.

---

### SYS_IPC_RECV_CAP — 13

Receive a message and accept any in-flight capability from an IPC endpoint.

**Arguments:**
- a1 — IPC capability handle
- a2 — pointer to destination buffer in user address space
- a3 — buffer length in bytes
- a4 — pointer to `u64` in user address space to receive the transferred handle

**Returns:** bytes copied (up to 64); `ENOCAP` / `ENOPERM` on error

On return, `*a4` contains the new `CapHandle` if a capability was transferred,
or `u64::MAX` (`0xFFFF_FFFF_FFFF_FFFF`) if no capability was attached to the
message. See [ipc.md](ipc.md).

---

### SYS_SERIAL_READ — 14

Read bytes from the COM1 serial port into a user buffer.

**Arguments:**
- a1 — pointer to destination buffer in user address space
- a2 — buffer length in bytes

**Returns:** number of bytes read

Blocks (yielding the CPU) until at least one byte is available, then reads as
many bytes as are ready up to `a2`.

---

### SYS_TIME — 15

Return milliseconds elapsed since kernel boot.

**Arguments:** none  
**Returns:** `u64` millisecond count; always succeeds

---

### SYS_TASK_STATUS — 16

Return the liveness status of a task.

**Arguments:**
- a1 — task ID

**Returns:**
- 0 — not found or dead
- 1 — running
- 2 — ready
- 3 — blocked

---

### SYS_TASK_LIST — 17

Fill a user buffer with `TaskInfo` structs, one per live task.

**Arguments:**
- a1 — pointer to buffer in user address space (`*mut TaskInfo`)
- a2 — maximum number of entries to write

**Returns:** number of entries written

Each entry is 24 bytes. See [Struct layouts](#struct-layouts).

---

### SYS_MEM_STAT — 18

Return the count of free 4 KiB physical frames.

**Arguments:** none  
**Returns:** free frame count as `u64`

---

### SYS_TASK_KILL — 19

Terminate a task by ID.

**Arguments:**
- a1 — task ID

**Returns:** 0 on success; `EINVAL` if the task is not found, already dead, or
is a protected kernel task

---

### SYS_BLK_READ — 20

Read one 512-byte sector from the VirtIO block device into a user buffer.

**Arguments:**
- a1 — sector number (0-based, `u64`)
- a2 — pointer to 512-byte destination buffer in caller's address space

**Returns:** 0 on success; `ENOSYS` if no block device is present; `EINVAL` on
bad arguments or I/O error

---

### SYS_BLK_WRITE — 21

Write one 512-byte sector from a user buffer to the VirtIO block device.

**Arguments:**
- a1 — sector number (0-based, `u64`)
- a2 — pointer to 512-byte source buffer in caller's address space

**Returns:** 0 on success; `ENOSYS` if no block device; `EINVAL` on bad
arguments or I/O error

---

### SYS_OPEN — 22

Open a file on the RFS filesystem.

**Arguments:**
- a1 — pointer to path string in user address space
- a2 — path length in bytes

**Returns:** file descriptor (≥ 0) on success; `ENOENT` if not found; `EISDIR`
if the path is a directory; `EMFILE` if the per-process fd table is full;
`ENOMNT` if the filesystem is not mounted; `EINVAL` on bad arguments

---

### SYS_READ — 23

Read bytes from an open file descriptor.

**Arguments:**
- a1 — file descriptor
- a2 — pointer to destination buffer in user address space
- a3 — number of bytes to read

**Returns:** bytes read; `EBADF` if fd invalid; `EINVAL` on other errors

---

### SYS_WRITE — 24

Write bytes to an open writable file descriptor.

**Arguments:**
- a1 — file descriptor
- a2 — pointer to source buffer in user address space
- a3 — number of bytes to write

**Returns:** bytes written; `EBADF` if fd invalid or not writable; `ENOSPC` if
the device has no free blocks; `ENOMNT` if the filesystem is not mounted;
`EINVAL` on other errors

---

### SYS_CLOSE — 25

Close an open file descriptor.

**Arguments:**
- a1 — file descriptor

**Returns:** 0 on success; `EBADF` if fd invalid

---

### SYS_STAT — 26

Query metadata for a path.

**Arguments:**
- a1 — pointer to path string in user address space
- a2 — path length in bytes
- a3 — pointer to 48-byte output buffer in user address space

**Returns:** 0 on success; `ENOENT` if path not found; `EINVAL` on bad
arguments

Writes 48 bytes into `*a3`. See [Struct layouts — Stat](#stat--48-bytes).

---

### SYS_READDIR — 27

Read directory entries for a path.

**Arguments:**
- a1 — pointer to path string in user address space
- a2 — path length in bytes
- a3 — pointer to output buffer in user address space (`*mut DirEntry`)
- a4 — output buffer length in bytes

**Returns:** number of entries written; `ENOENT` if path not found; `EINVAL`
on bad arguments

Each entry is 264 bytes. Entries written = `min(actual_count, a4 / 264)`. See
[Struct layouts — DirEntry](#direntry--264-bytes).

---

### SYS_CREATE — 28

Create a new empty regular file.

**Arguments:**
- a1 — pointer to path string in user address space
- a2 — path length in bytes

**Returns:** writable file descriptor on success; `EEXIST` if the path already
exists; `ENOTDIR` if a path component is not a directory; `EMFILE` if the fd
table is full; `ENOSPC` if the device has no free blocks; `ENOMNT` if not
mounted; `EINVAL` on bad arguments

**Exclusive-create guarantee:** `SYS_CREATE` is atomic create-if-absent —
the existence check and the creation happen inside one uninterrupted syscall
(the kernel is single-threaded and FS paths are never preempted). Of any
number of concurrent creators of the same path, exactly one receives a
writable fd; every other caller receives `EEXIST`. There is no truncate-if-
exists mode. This is the locking primitive for userspace lock files
(e.g. the shade store-db lock): winner holds the lock, losers back off on
`EEXIST`, release is `SYS_UNLINK`.

**Ownership:** the created inode is owned by uid/gid 0 with mode `0644`; caller
identity is not recorded and no ownership argument is honored (rfs2 create-time
policy — see [`rfs-v2/07 §3`](../rfs-v2/07-directories.md)). The kernel path
still carries the caller's uid/gid, so a chown-equivalent can be added later
without an ABI break; chmod/chown are deferred.

---

### SYS_UNLINK — 29

Delete a regular file.

**Arguments:**
- a1 — pointer to path string in user address space
- a2 — path length in bytes

**Returns:** 0 on success; `ENOENT` if not found; `EINVAL` on other errors

---

### SYS_SERIAL_AVAIL — 30

Check whether keyboard or serial data is available without blocking.

**Arguments:** none  
**Returns:** 1 if data is available; 0 if not

---

### SYS_TASK_WAIT — 31

Block the calling task until the target task exits, then reap and return its
exit status.

**Arguments:**
- a1 — task ID to wait for

**Returns:**
- the target's **exit status word** (see [SYS_TASK_EXIT](#sys_task_exit--1)
  encoding; always `< 0x1_0000`) once it has exited. A clean exit with code `0`
  returns `0`.
- `ENOENT` if the target is not a live task **and** has no retained exit record
  — it never existed, its status was already consumed by an earlier waiter, or
  the task that spawned it has since died (which cascade-frees the record).
  Records are never evicted to make room, so this never happens to a live
  spawner waiting on a child it spawned.

If the target is still alive, the caller blocks until it exits (no polling). If
the target has already exited, the call returns its retained status immediately
and consumes the record (a second wait on the same id returns `ENOENT`).

> **Disambiguation.** The pre-exit-code ABI returned `0` for both "exited" and
> "not found". Those are now distinct: a status word (`0` for a clean zero
> exit) versus the `ENOENT` error sentinel. Callers test with the errno
> predicate (`errno::is_err`) — an error means "not found", anything else is a
> status.

---

### SYS_MKDIR — 32

Create a new directory.

**Arguments:**
- a1 — pointer to path string in user address space
- a2 — path length in bytes

**Returns:** 0 on success; `EEXIST` if the path already exists; `ENOTDIR` if a
path component is not a directory; `ENOSPC` if the device has no free blocks;
`ENOMNT` if not mounted; `EINVAL` on bad arguments

**Ownership:** the created directory is owned by uid/gid 0 with mode `0755`;
caller identity is not recorded and no ownership argument is honored (rfs2
create-time policy — see [`rfs-v2/07 §3`](../rfs-v2/07-directories.md)). The
kernel path still carries the caller's uid/gid, so a chown-equivalent can be
added later without an ABI break; chmod/chown are deferred.

---

### SYS_NANOSLEEP — 33

Sleep for at least the specified number of nanoseconds.

**Arguments:**
- a1 — duration in nanoseconds (rounded up to the next millisecond)

**Returns:** 0 when the deadline has passed

Blocks the calling task for at least the requested duration.

---

### SYS_TASK_SETPRIORITY — 34

Set the scheduling priority for a task.

**Arguments:**
- a1 — task ID
- a2 — priority: 0 = low, 1 = normal, 2 = high

**Returns:** 0 on success; `EINVAL` if task not found or priority out of range

---

### SYS_RENAME — 35

Rename or move a regular file.

**Arguments:**
- a1 — pointer to old path string in user address space
- a2 — old path length in bytes
- a3 — pointer to new path string in user address space
- a4 — new path length in bytes

**Returns:** 0 on success; `ENOENT` if the source path is not found; `EEXIST`
if the destination already exists; `ENOTDIR` if a path component is not a
directory; `ENOMNT` if not mounted; `EINVAL` on bad arguments

---

### SYS_SEEK — 36

Seek within an open file descriptor.

**Arguments:**
- a1 — file descriptor
- a2 — offset (`i64` encoded as `u64`)
- a3 — whence: 0 = `SEEK_SET`, 1 = `SEEK_CUR`, 2 = `SEEK_END`

**Returns:** new file offset on success; `EBADF` if fd invalid; `EINVAL` on
other errors

---

### SYS_PS — 37

List live tasks with extended info.

**Arguments:**
- a1 — pointer to output buffer in user address space (`*mut PsEntry`)
- a2 — maximum number of entries to write

**Returns:** number of entries written

Each entry is 48 bytes. See [Struct layouts — PsEntry](#psentry--48-bytes).

---

### SYS_BRK — 38

Set or query the program break (heap top) for the calling task.

**Arguments:**
- a1 — new break address; 0 = query current break without changing it

**Returns:** new break address on success; highest address actually mapped if
OOM (partial success); `EINVAL` if `a1` falls within the stack area or, on
shrink, below the heap base (`0x0000_0004_0000_0000`)

Growing maps zeroed anonymous pages up to `round_up(a1, 4096)`.  Shrinking
(`a1 <= current break`) unmaps and frees every whole page above
`round_up(a1, 4096)`; pages below that boundary stay mapped.  Requires a
`Memory` capability with `WRITE` right (`ENOPERM` otherwise).

---

### SYS_IPC_POLL — 39

Non-blocking receive from an IPC endpoint.

**Arguments:**
- a1 — IPC capability handle
- a2 — pointer to destination buffer in user address space
- a3 — buffer length in bytes

**Returns:** bytes received; `EAGAIN` if the ring is empty; `ENOCAP` /
`ENOPERM` on capability errors

Returns immediately without blocking. See [ipc.md](ipc.md).

---

### SYS_IPC_BIND — 40

Bind a name to an IPC endpoint so peers can look it up without a pre-shared
handle.

**Arguments:**
- a1 — IPC capability handle
- a2 — pointer to name string in user address space (UTF-8)
- a3 — name length in bytes (max 128)

**Returns:** 0 on success; `ENOSYS` if the name is already registered;
`ENOCAP` if handle invalid; `ENOPERM` if caller lacks `GRANT` right

Names persist until kernel restart and are not garbage-collected.

---

### SYS_IPC_LOOKUP — 41

Look up a named IPC endpoint and obtain a new capability handle.

**Arguments:**
- a1 — pointer to name string in user address space
- a2 — name length in bytes
- a3 — requested rights mask (values 0–15; `GRANT` and `REVOKE` bits are
       stripped by the kernel before creating the returned handle)

**Returns:** new `CapHandle` on success; `ENOENT` if name not registered

---

### SYS_IPC_RECV_TIMEOUT — 42

Receive from an IPC endpoint with a millisecond deadline.

**Arguments:**
- a1 — IPC capability handle
- a2 — pointer to destination buffer in user address space
- a3 — buffer length in bytes
- a4 — timeout in milliseconds

**Returns:** bytes received; `EAGAIN` if timeout expires before a message
arrives; `ENOCAP` / `ENOPERM` on capability errors

---

### SYS_IPC_SEND_TIMEOUT — 43

Send to an IPC endpoint with a millisecond deadline.

**Arguments:**
- a1 — IPC capability handle
- a2 — pointer to message buffer in user address space
- a3 — message length in bytes (max 64)
- a4 — timeout in milliseconds

**Returns:** 0 on success; `EAGAIN` if timeout expires before the ring drains;
`ENOCAP` / `ENOPERM` on capability errors

---

### SYS_TIME_EPOCH — 44

Return Unix epoch milliseconds since 1970-01-01 00:00:00 UTC.

**Arguments:** none  
**Returns:** current time as a `u64` millisecond count since the Unix epoch

**Contract:**
- Anchored from the CMOS RTC (MC146818) at boot, converted to Unix epoch
  milliseconds using Hinnant's days-from-civil algorithm.
- Advances monotonically via the APIC tick counter (~1 ms resolution).
- UTC assumed; no timezone conversion.
- Never returns an error sentinel — the return value is always a valid timestamp.
- Accuracy within boot-overhead of the CMOS RTC read (~seconds).

---

### SYS_GETUID — 45

Return the effective user ID of the calling task.

**Arguments:** none  
**Returns:** `u32` UID in RAX (always 0 / root until `SYS_SETUID` is implemented)

---

### SYS_GETGID — 46

Return the effective group ID of the calling task.

**Arguments:** none  
**Returns:** `u32` GID in RAX (always 0 / root until `SYS_SETGID` is implemented)

---

### SYS_SETUID — 47

Set the effective user ID of the calling task.

**Arguments:** `a1` = new UID (u32)  
**Returns:** 0 on success; `ENOPERM` if the calling task is not root and `new_uid ≠ current_uid`

**Security model:** Root (uid=0) may set any uid; non-root tasks may only confirm their own uid (no-op). Call `SYS_SETGID` before `SYS_SETUID` when dropping from root — once uid transitions away from 0, the task loses the right to change gid freely.

---

### SYS_SETGID — 48

Set the effective group ID of the calling task.

**Arguments:** `a1` = new GID (u32)  
**Returns:** 0 on success; `ENOPERM` if the calling task is not root and `new_gid ≠ current_gid`

---

### Number 49 — unassigned

Returns `ENOSYS`. Reserved for future assignment.

---

### SYS_SOCKET — 50

Create a UDP socket.

**Arguments:** none  
**Returns:** socket file descriptor (≥ 0); `ENOSYS` if the network stack is
not present

---

### SYS_BIND — 51

Bind a socket to a local UDP port.

**Arguments:**
- a1 — socket file descriptor
- a2 — local port (u16)

**Returns:** 0 on success; `EINVAL` on bad arguments

---

### SYS_SENDTO — 52

Send a UDP datagram.

**Arguments:**
- a1 — socket file descriptor
- a2 — pointer to data buffer in user address space
- a3 — data length in bytes
- a4 — destination IP address (u32, big-endian)
- a5 — destination port (u16, big-endian)

**Returns:** 0 on success; `EAGAIN` if ARP resolution is pending; `EINVAL` on
bad arguments

---

### SYS_RECVFROM — 53

Receive a UDP datagram (blocking).

**Arguments:**
- a1 — socket file descriptor
- a2 — pointer to data buffer in user address space
- a3 — buffer length in bytes
- a4 — pointer to `u32` in user address space that receives the source IP
       (big-endian)
- a5 — pointer to `u16` in user address space that receives the source port
       (big-endian)

**Returns:** bytes received; `EBADF` if fd invalid

Blocks until a datagram arrives.

---

### SYS_NET_CLOSE — 54

Close a network socket.

**Arguments:**
- a1 — socket file descriptor

**Returns:** 0 on success; `EBADF` if fd invalid

---

### SYS_POWEROFF — 55

Power off the machine.

**Arguments:** none  
**Returns:** does not return

Issues ACPI S5 shutdown via QEMU PM1a port `0x604`.

---

### SYS_MOUNT — 56

Mount a filesystem backend at a path. The mount table is generic
(longest-prefix routing over N mounts); the root filesystem is the mount at
`/` installed at boot.

**Capability:** requires a `Filesystem` capability with the `WRITE` right in
the caller's table. No ambient authority — without the capability the call
returns `ENOPERM` regardless of arguments.

**Arguments:**

| Reg | Meaning |
|-----|---------|
| a1 | `at_ptr` — mount point path (UTF-8) |
| a2 | `at_len` — path length in bytes |
| a3 | `source` — backend selector: `0` = `MOUNT_SRC_RFS2_RAM` (fresh RAM-backed RFS V2, formatted at mount time, volatile); `1` = `MOUNT_SRC_RFS2_BLK` (persistent RFS V2 on the secondary virtio-blk device, formatted on first-ever boot then mounted, survives a power cycle) |
| a4 | `flags` — bit 0 = `MOUNT_STORE` (attach read-only-after-realize store semantics to this mount) |

**Returns:** 0 on success; `ENOPERM` if the caller holds no `Filesystem`
capability with `WRITE`; `EMOUNTED` if a mount already exists at the mount
point; `ENOENT` if the mount point does not exist in the covering
filesystem; `ENOTDIR` if the mount point is not a directory; `EINVAL` for a
bad pointer/length, non-UTF-8 or relative path, or unknown `source`/`flags`;
`ENOMNT` if no root filesystem is mounted.

The mount point must be an existing directory in the covering filesystem.
Paths under it then resolve in the mounted backend (longest-prefix wins,
component-wise); the covering filesystem's own entries beneath the mount
point become unreachable until unmounted. Open fds keep addressing the
backend they were opened on.

With `MOUNT_STORE`, the mount enforces read-only-after-realize: a top-level
entry `<digest>-<name>-<version>` becomes immutable (`EROFS` on any write
into it) once a temp path is atomically renamed onto it; renaming onto an
already-sealed name is an idempotent no-op success (re-realize); temp paths
remain freely writable until sealed. The seal is **absolute** — no syscall
lifts it in place. On a persistent (`MOUNT_SRC_RFS2_BLK`) store mount the seal
set is reconstructed from the on-disk store contents at mount time, so it holds
across a power cycle. Dead store entries are reclaimed only by whole-path
removal (`SYS_STORE_REMOVE`, below), never by making sealed content writable.

### SYS_STORE_REMOVE (60)

Remove an entire unreferenced store path — the sole store-reclamation
primitive. `a1=path_ptr`, `a2=path_len`. The whole tree is deleted (files
unlinked, directories rmdir'd, blocks freed) **below** the seal — it never opens
a sealed file for write — and the entry's in-kernel seal is dropped as the last
lifecycle step. Requires a `Filesystem` capability with `WRITE` (store-owner
authority — a builder cannot reclaim). `path` must name a single top-level entry
on a store (realize-guarded) mount. Returns 0 (idempotent; already-gone is
success) or `ENOPERM` / `EINVAL` (not a store mount, or a nested path) / mount
errnos. Safe under input-addressing: a later realize of the same digest
reproduces byte-identical content.

> **Number 59 (`SYS_UNSEAL`) is retired** — a permanently-unassigned `ENOSYS`
> gap (like 49). It once lifted a seal to make sealed content writable for GC;
> that is gone by design (the seal is absolute). Never reuse 59.

---

## Struct layouts

All fields are little-endian. All padding bytes are zeroed by the kernel.

### Canonical task state encoding

All task-state fields in all structs and syscalls use this encoding:

| Value | Meaning |
|-------|---------|
| 0 | dead or not found (not included in list output) |
| 1 | running |
| 2 | ready |
| 3 | blocked |

`SYS_TASK_STATUS` currently returns a non-canonical encoding; see its entry
above and `docs/plans/followup-code-tasks.md`.

### TaskInfo — 24 bytes

Used by `SYS_TASK_LIST` (17).

```
Offset  Size  Type    Field
──────  ────  ──────  ─────────────────────────────────────────────────
0       8     u64 LE  task_id     — unique task identifier
8       8     u64 LE  state       — canonical task state (see above)
16      1     u8      kind        — 0 = kernel task, 1 = userspace task
17      7     [u8;7]  _pad        — zeroed
```

### PsEntry — 48 bytes

Used by `SYS_PS` (37).

```
Offset  Size  Type     Field
──────  ────  ───────  ─────────────────────────────────────────────────
0       8     u64 LE   id          — unique task identifier
8       8     u64 LE   state       — canonical task state (see above)
16      1     u8       kind        — 0 = kernel task, 1 = userspace task
17      1     u8       priority    — 0 = low, 1 = normal, 2 = high
18      1     u8       name_len    — length of name in bytes (0–16)
19      5     [u8;5]   _pad        — zeroed
24      16    [u8;16]  name        — task name; first name_len bytes valid, rest zeroed
40      8     [u8;8]   _pad2       — zeroed
```

### Stat — 48 bytes

Used by `SYS_STAT` (26). All fields naturally aligned (every field at an
offset that is a multiple of its own size). Safe to cast a `[u8; 48]` buffer
to a `#[repr(C, packed)]`-free struct if field order matches exactly.

```
Offset  Size  Type    Field
──────  ────  ──────  ─────────────────────────────────────────────────
0       8     u64 LE  size        — file size in bytes
8       8     u64 LE  mtime       — last-modified time (Unix epoch milliseconds,
                                    ms since 1970-01-01 00:00:00 UTC; same epoch
                                    as SYS_TIME_EPOCH)
16      8     u64 LE  ctime       — creation time (Unix epoch milliseconds,
                                    ms since 1970-01-01 00:00:00 UTC; same epoch
                                    as SYS_TIME_EPOCH)
24      4     u32 LE  flags       — inode flags (see below)
28      4     u32 LE  uid         — owner user ID
32      4     u32 LE  gid         — owner group ID
36      4     u32 LE  nlink       — hard link count
40      2     u16 LE  mode        — permission bits (UNIX-style, e.g. 0o644)
42      6     [u8;6]  _pad        — zeroed
```

`flags` bit meanings:

| Bit | Value | Meaning |
|-----|-------|---------|
| 0 | 0x01 | `INODE_USED` — always set for a valid inode |
| 1 | 0x02 | `INODE_DIR` — entry is a directory |
| 2 | 0x04 | `INODE_SYMLINK` — entry is a symbolic link |
| 3 | 0x08 | `INODE_FAST_SYM` — symlink name stored inline |

**Timestamp note:** `mtime` and `ctime` use Unix epoch milliseconds (same epoch
as `SYS_TIME_EPOCH`). Both are populated by the kernel at file/directory
creation. `mtime` is updated on write.

### DirEntry — 264 bytes

Used by `SYS_READDIR` (27). One entry per directory member.

```
Offset  Size  Type      Field
──────  ────  ────────  ─────────────────────────────────────────────────
0       4     u32 LE    ino         — inode number
4       1     u8        file_type   — entry type (see below)
5       1     u8        name_len    — length of name in bytes (0–255)
6       2     [u8;2]    _pad        — zeroed
8       256   [u8;256]  name        — filename; first name_len bytes valid,
                                     remainder zeroed; max 255 usable bytes
```

`file_type` values:

| Value | Meaning | Corresponding Stat.flags bit |
|-------|---------|------------------------------|
| 1 | regular file | — (bit0=USED set, bit1 and bit2 clear) |
| 2 | directory | bit1=0x02 (`INODE_DIR`) |
| 3 | symbolic link | bit2=0x04 (`INODE_SYMLINK`) |

The numeric values do not equal the Stat `flags` bit positions, but the
semantic meaning is consistent: a directory entry with `file_type=2` will have
`Stat.flags & 0x02` set if you stat the same path.
