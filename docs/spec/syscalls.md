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
| RDI | Argument 1 (a1) | Preserved |
| RSI | Argument 2 (a2) | Preserved |
| RDX | Argument 3 (a3) | Preserved |
| R10 | Argument 4 (a4) | Preserved |
| R8 | Argument 5 (a5) | Preserved |
| R9 | Argument 6 (a6) | Preserved |
| RCX | User RIP (saved by CPU) | — |
| R11 | User RFLAGS (saved by CPU) | — |

RCX and R11 are clobbered by the `syscall` instruction itself. User code that
needs to pass 4+ arguments must use R10 for a4, not RCX.

---

## Error codes

Error values are returned in RAX as large `u64` values, equivalent to
negative `i64`. User code should cast RAX to `i64` and check `< 0`.

| Value (u64) | i64 | Name | Meaning |
|-------------|-----|------|---------|
| `0xFFFF_FFFF_FFFF_FFFF` | -1 | `ENOSYS` | Unknown syscall number |
| `0xFFFF_FFFF_FFFF_FFFE` | -2 | `ENOCAP` | Invalid or stale capability handle |
| `0xFFFF_FFFF_FFFF_FFFD` | -3 | `ENOPERM` | Capability rights insufficient |
| `0xFFFF_FFFF_FFFF_FFFC` | -4 | `EINVAL` | Invalid argument |

---

## Syscall table

### SYS_YIELD — 0

Voluntarily yield the CPU to the next ready task.

**Arguments:** none  
**Returns:** 0

The scheduler runs immediately; the calling task is placed at the back of
the ready queue and will resume when scheduled again.

---

### SYS_TASK_EXIT — 1

Terminate the calling task.

**Arguments:** none  
**Returns:** never

Marks the task Dead and switches to the next ready task. The task's kernel
stack and capability table are freed. User mappings are not yet reclaimed
(no reference counting on page tables yet).

---

### SYS_MMAP — 2

Map a physical frame into the calling task's address space.

**Arguments:**
- a1 — virtual address (must be 4 KiB-aligned)
- a2 — physical address (must be 4 KiB-aligned)
- a3 — page flags (same encoding as VMM flags: bit 0=P, 1=W, 2=U, 63=NX)

**Returns:** 0 on success, `EINVAL` if addresses are unaligned

The caller must hold a Memory capability to invoke this syscall. Currently
the capability check validates presence of a `CapKind::Memory` handle but
does not enforce range restrictions — that is a planned improvement.

---

### SYS_MUNMAP — 3

Unmap a virtual page.

**Arguments:**
- a1 — virtual address (must be 4 KiB-aligned)

**Returns:** 0 on success, `EINVAL` if unaligned

Calls `vmm::unmap_page` and issues `invlpg`. The underlying physical frame
is **not** freed — the caller is responsible for tracking ownership.

---

### SYS_CAP_GRANT — 4

Copy a capability to another task with reduced rights.

**Arguments:**
- a1 — source capability handle in the calling task's table
- a2 — target task ID
- a3 — rights mask (bitfield; result rights = source_rights & mask)

**Returns:** new handle in target task's table, or error

Errors: `ENOCAP` if handle invalid, `ENOPERM` if source lacks `GRANT` right,
`EINVAL` if target task does not exist or is self.

See [capabilities.md](capabilities.md) for rights bitfield values.

---

### SYS_CAP_REVOKE — 5

Remove a capability from the calling task's table.

**Arguments:**
- a1 — capability handle to revoke

**Returns:** 0 on success, `ENOCAP` if handle invalid, `ENOPERM` if lacks
`REVOKE` right

---

### SYS_IPC_SEND — 6

Send a message to an IPC endpoint.

**Arguments:**
- a1 — IPC capability handle
- a2 — pointer to message buffer in user address space
- a3 — message length in bytes (max 64)

**Returns:** 0 on success, `ENOCAP` if handle invalid, `ENOPERM` if lacks
`WRITE` right

If the ring buffer is full the calling task **blocks** until a receiver
drains the ring. The kernel validates that [a2, a2+a3) is within user
address space before copying.

See [ipc.md](ipc.md) for ring buffer layout and message format.

---

### SYS_IPC_RECV — 7

Receive a message from an IPC endpoint.

**Arguments:**
- a1 — IPC capability handle
- a2 — pointer to destination buffer in user address space
- a3 — buffer length in bytes

**Returns:** bytes copied on success (up to 64), `ENOCAP` / `ENOPERM` on error

If the ring buffer is empty the calling task **blocks** until a sender writes
a message. Messages are fixed at 64 bytes on the wire; the returned value
is `min(a3, MSG_SIZE)`.

---

### SYS_IPC_CREATE — 8

Create a new IPC endpoint and return a capability to it.

**Arguments:** none  
**Returns:** new IPC capability handle (with `CapRights::ALL`)

Allocates one 4 KiB physical page for the ring buffer and maps it into the
kernel IPC window at `0xFFFF_D000_0000_0000 + idx * 4096`.

---

### SYS_ROLLBACK — 9

Invoke the rollback mechanism (system checkpoint restore).

**Arguments:** none  
**Returns:** never (on success); `ENOPERM` if no Rollback cap

Requires a `CapKind::Rollback` capability. Currently a stub that panics
with "rollback invoked" — full checkpoint/restore is not yet implemented.

---

### SYS_EXEC — 10

Load an ELF binary and spawn a new userspace task.

**Arguments:**
- a1 — pointer to ELF data in caller's address space
- a2 — ELF data length in bytes
- a3 — pointer to capability handle array (`u64[]`)
- a4 — number of capabilities to inherit

**Returns:** new `TaskId` on success, error code on failure

The kernel copies the ELF data and capability list from user space, parses
and loads the ELF, allocates a user stack, writes the initial ABI frame,
and spawns a kernel task running the exec trampoline. The new task's
capability table is populated from the handle array — handle 0 = caps[0],
handle 1 = caps[1], etc.

See [elf.md](elf.md) for load details.

---

### SYS_BLK_READ — 20

Read one 512-byte sector from the VirtIO block device.

**Arguments:**
- a1 — sector number (0-based, `u64`)
- a2 — pointer to 512-byte destination buffer in caller's address space

**Returns:** 0 on success, `ENOSYS` if no block device is present

The kernel validates that [a2, a2+512) is within user address space, then
performs a synchronous (polled) VirtIO read. See [drivers.md](drivers.md).

---

### SYS_BLK_WRITE — 21

Write one 512-byte sector to the VirtIO block device.

**Arguments:**
- a1 — sector number (0-based, `u64`)
- a2 — pointer to 512-byte source buffer in caller's address space

**Returns:** 0 on success, `ENOSYS` if no block device is present

---

## Planned syscalls

These are defined in `TODO.md` and not yet implemented.

| Nr | Name | Purpose |
|----|------|---------|
| 22 | `SYS_OPEN` | Open a file on the RFS filesystem |
| 23 | `SYS_READ` | Read from an open file descriptor |
| 24 | `SYS_WRITE` | Write to an open file descriptor |
| 25 | `SYS_CLOSE` | Close a file descriptor |
| 26 | `SYS_STAT` | Query file metadata |
| 27 | `SYS_READDIR` | Read directory entries |
