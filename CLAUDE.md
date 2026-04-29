# OROS (Open Runtime Operating System) — userspace development reference

OROS is the userspace layer that runs on the Lythos (Capability-Aware System Kernel) microkernel. This file is the guide for building OROS components. For kernel internals, see `lythos/CLAUDE.md`.

---

## Repository layout

```
OROS/
  lythd/          — PID 1 init process and service supervisor
  lythdist/       — capability distributor daemon
  lythmsg/        — IPC bus daemon
  lythos-std/     — native runtime (thin syscall wrappers)
  lysh/           — system shell
  rpkg/           — package manager
```

Each crate targets `x86_64-unknown-linux-musl` (for the compat layer) or a native lythos target (TBD). Start with musl for bootstrapping — `lythos-linux-compat` provides the translation layer.

---

## Lythos syscall ABI

Entry: `syscall` instruction. Return value in RAX.

| Register | Entry | Return |
|----------|-------|--------|
| RAX | syscall number | return value |
| RDI | argument 1 | |
| RSI | argument 2 | |
| RDX | argument 3 | |
| R10 | argument 4 (not RCX — clobbered by `syscall`) | |
| R8 | argument 5 | |
| R9 | argument 6 | |

### Syscall numbers

| Nr | Name | a1 | a2 | a3 | a4 | Returns |
|----|------|----|----|----|----|---------|
| 0 | SYS_YIELD | — | — | — | — | 0 |
| 1 | SYS_TASK_EXIT | — | — | — | — | never |
| 2 | SYS_MMAP | virt (u64) | phys (u64) | flags (u64) | — | 0 |
| 3 | SYS_MUNMAP | virt (u64) | — | — | — | 0 |
| 4 | SYS_CAP_GRANT | handle (u64) | target_task_id (u64) | rights_mask (u8 as u64) | — | new handle or error |
| 5 | SYS_CAP_REVOKE | handle (u64) | — | — | — | 0 or error |
| 6 | SYS_IPC_SEND | cap_handle (u64) | msg_ptr (u64) | msg_len (u64) | — | 0 |
| 7 | SYS_IPC_RECV | cap_handle (u64) | buf_ptr (u64) | buf_len (u64) | — | bytes received |
| 8 | SYS_IPC_CREATE | — | — | — | — | new cap handle |
| 9 | SYS_ROLLBACK | — | — | — | — | never (requires Rollback cap) |
| 10 | SYS_EXEC | elf_ptr (u64) | elf_len (u64) | caps_ptr (u64) | caps_len (u64) | new TaskId |

### Error codes

Errors are returned as large u64 values (equivalent to negative i64):

| Value | Constant | Meaning |
|-------|----------|---------|
| `0xFFFF_FFFF_FFFF_FFFF` | ENOSYS | Unknown syscall number |
| `0xFFFF_FFFF_FFFF_FFFE` | ENOCAP | Invalid or stale capability handle |
| `0xFFFF_FFFF_FFFF_FFFD` | ENOPERM | Insufficient capability rights |
| `0xFFFF_FFFF_FFFF_FFFC` | EINVAL | Invalid argument (bad task ID, self-grant, etc.) |

---

## Page flags for SYS_MMAP

The `flags` argument to `SYS_MMAP` is a raw x86-64 page table entry flag word:

| Flag | Value | Meaning |
|------|-------|---------|
| PRESENT | `1 << 0` | Page is mapped |
| WRITABLE | `1 << 1` | Page is writable |
| USER | `1 << 2` | Page accessible from ring 3 — **required for user pages** |
| NX | `1 << 63` | No-execute |

Common combinations:
- User read-execute (code): `PRESENT | USER = 0x5`
- User read-write (stack/data): `PRESENT | WRITABLE | USER | NX = 0x8000_0000_0000_0007`

---

## Capability system

Capabilities are opaque `u64` handles. The kernel maps each handle to a typed object with rights.

**CapKind:** `Memory`, `Ipc`, `Rollback`

**CapRights bitflags:**

| Bit | Right | Meaning |
|-----|-------|---------|
| 0 | READ | Can read / receive on this cap |
| 1 | WRITE | Can write / send on this cap |
| 2 | GRANT | Can grant derived copies to other tasks |
| 3 | REVOKE | Can revoke this cap and all derived copies |

**SYS_CAP_GRANT:** Copies a capability to another task with rights masked by `rights_mask`. You can only grant rights you hold — the kernel intersects. Returns the new handle in the recipient's table, or an error if the handle is invalid (`ENOCAP`), you lack grant rights (`ENOPERM`), or the target task doesn't exist (`EINVAL`).

**SYS_CAP_REVOKE:** Removes the cap from your own table. The `REVOKE` right is required.

---

## IPC

All IPC is through capability-gated endpoints. Each endpoint is a 4 KiB shared ring buffer.

**Ring buffer layout (in the shared page):**

```
offset 0:  u32  head  — read cursor (advanced by receiver)
offset 4:  u32  tail  — write cursor (advanced by sender)
offset 8:  [u8; 4088] data
```

- Fixed slot size: **64 bytes** (`MSG_SIZE`).
- Ring capacity: **63 messages**.
- Head and tail are free-running `u32` counters. Position = `counter % 63`.
- Ring is full when `tail - head == 63`. Ring is empty when `head == tail`.

**Blocking semantics:**
- `SYS_IPC_SEND` blocks the calling task if the ring is full. Resumes when a receiver consumes a slot.
- `SYS_IPC_RECV` blocks the calling task if the ring is empty. Resumes when a sender posts a message.

**SYS_IPC_CREATE** allocates a new endpoint and returns a cap handle with full rights. The physical page backing the ring can be mapped into a peer's address space via `SYS_MMAP` using the physical address from the Memory cap.

---

## lythd boot protocol

lythd is the first userspace process. It receives three capabilities at fixed handles:

| Handle | CapKind | Rights | Contents |
|--------|---------|--------|----------|
| 0 | Memory | ALL | Root memory cap — all free physical frames at boot |
| 1 | Rollback | ALL | `SYS_ROLLBACK` gate — only lythd holds this |
| 2 | Ipc | ALL | Boot-info endpoint — one pre-queued `BootInfo` message |

**First action:** call `SYS_IPC_RECV` on handle 2 to consume the BootInfo message.

**BootInfo message format (64 bytes):**

```
offset  0:  u64  signature   = 0xB007_1000_B007_1000
offset  8:  u64  mem_bytes   = total usable RAM in bytes
offset 16:  u64  free_frames = number of free 4 KiB frames at boot
offset 24:  [u8; 12] vendor  = CPUID leaf 0 vendor string (12 ASCII bytes)
offset 36:  [u8; 28] _pad    = zeroed
```

---

## SYS_EXEC

Loads and runs a static ELF64 binary. The ELF must be:
- `ET_EXEC` (executable), `EM_X86_64`, little-endian
- Static — no dynamic linker
- Entry point set in ELF header (`e_entry`)

Caps are inherited: `caps_ptr` points to a `u64` array of handle values from the calling task's cap table. They are copied into the new task's table as handles 0, 1, 2, ... in order.

The new task gets:
- Its own kernel stack
- A user stack at the next available slot above `0x0000_7FFF_0000_0000`
- An initial ABI stack frame: `argc=0`, null argv, null envp
- The inherited capabilities

Returns the new `TaskId` on success, `EINVAL` on ELF parse error.

---

## User address space layout

| Region | Address | Description |
|--------|---------|-------------|
| User code | `0x0000_0001_0000_0000`+ | Recommended; above the kernel's 0→1 GiB identity map |
| User stacks | `0x0000_7FFF_0000_0000`+ | Allocated by the kernel on `SYS_EXEC`; 8 MiB usable per task |
| Kernel | `0xFFFF_8000_0000_0000`+ | Not accessible from ring 3 (SMEP enforced) |

The first 1 GiB (`0x0000_0000`–`0x4000_0000`) is identity-mapped by the kernel with no NX. Avoid placing user code here — the kernel executes from this range and it is not isolated from the kernel's own execution.

---

## Suggested lythd startup sequence

```
1. SYS_IPC_RECV(handle=2, buf, 64)   — consume BootInfo
2. Parse BootInfo (signature check, read free_frames)
3. SYS_EXEC(lythdist_elf, [mem_cap, ...])   — spawn lythdist
4. SYS_IPC_RECV(...)                  — wait for lythdist ready signal
5. SYS_EXEC(lythmsg_elf, [...])       — spawn lythmsg
6. Start stability timer (30s)
7. Read service definitions, spawn non-critical services
8. Supervisor loop: yield, check for dead critical tasks
```

---

## lythos-std — native syscall wrappers

The `lythos-std` crate provides thin safe wrappers around the syscall ABI. Use it as the foundation for all OROS programs instead of raw `syscall` instructions.

Suggested API shape:

```rust
pub fn sys_yield();
pub fn sys_task_exit() -> !;
pub fn sys_ipc_send(cap: u64, msg: &[u8]) -> Result<(), u64>;
pub fn sys_ipc_recv(cap: u64, buf: &mut [u8]) -> Result<usize, u64>;
pub fn sys_ipc_create() -> Result<u64, u64>;
pub fn sys_cap_grant(handle: u64, target_task: u64, rights: u8) -> Result<u64, u64>;
pub fn sys_cap_revoke(handle: u64) -> Result<(), u64>;
pub fn sys_exec(elf: &[u8], caps: &[u64]) -> Result<u64, u64>;
pub fn sys_mmap(virt: u64, phys: u64, flags: u64) -> Result<(), u64>;
pub fn sys_munmap(virt: u64) -> Result<(), u64>;
```
