# lythd — init process

## Role

lythd is the first userspace process. It is the parent of all other
processes and acts as the system's init / service manager. The kernel
starts exactly one lythd instance at boot and never starts another. If
lythd exits, the kernel panics (this is intentional — there is no recovery
from a dead init).

lythd is part of the OROS userspace repository, not the Lythos kernel
repository. The compiled ELF is embedded in the kernel image for bootstrap
purposes (see below).

---

## Loading

### Embedding in the kernel image

`src/elf.rs` includes the lythd ELF as a byte slice:

```rust
static LYTHD_ELF: &[u8] = include_bytes!("elf/lythd");
```

This is a fallback for environments where QEMU's device loader cannot be
used. The embedded copy is always present.

### Loading from a fixed physical address (primary path)

QEMU's device loader writes the lythd ELF binary to physical address
`0x400000` before the CPU starts:

```
-device loader,file=lythd,addr=0x400000,force-raw=on
```

`kmain` reads from `0x400000` if the ELF magic is present, falling back to
the embedded copy if not. The PMM reserves frames `0x400000..0x480000`
(512 KiB; `LYTHD_MODULE_ADDR` / `LYTHD_MODULE_MAX`) at `pmm::init` time to
prevent the allocator from reclaiming those frames before the copy.

The ELF is copied to the kernel heap immediately in `kmain`, after which
the physical frames at `0x400000` could be reclaimed (this is not done
currently — the PMM reservation persists for the lifetime of the kernel).

---

## Boot protocol

### Step 1 — lythd receives three capabilities

`kmain` calls `elf::exec(LYTHD_ELF, &[mem_cap, rollback_cap, boot_cap])`.
lythd starts with:

| Handle | Kind | Rights | Description |
|--------|------|--------|-------------|
| 0 | `Memory` | `ALL` | All free physical frames at boot |
| 1 | `Rollback` | `ALL` | Kernel rollback / checkpoint access |
| 2 | `Ipc` | `ALL` | Boot IPC endpoint with one pre-queued BootInfo |

### Step 2 — lythd receives BootInfo

lythd's first action must be:

```
SYS_IPC_RECV(handle=2, buf, 64)
```

This consumes the pre-queued `BootInfo` message. If lythd does not call
recv before doing anything that blocks (unlikely but possible), the message
stays in the ring — but lythd should read it before proceeding.

### BootInfo message layout (64 bytes, MSG_SIZE)

| Offset | Size | Field | Description |
|--------|------|-------|-------------|
| 0 | 8 | signature | `0xB007_1000_B007_1000` — sanity check |
| 8 | 8 | mem_bytes | `free_frames × 4096` — usable RAM in bytes |
| 16 | 8 | free_frames | PMM free frame count at the moment of boot |
| 24 | 12 | vendor | CPUID leaf 0 vendor string: EBX\|\|EDX\|\|ECX |
| 36 | 28 | _pad | zeroed |

The `vendor` string is the 12-byte ASCII result of CPUID with EAX=0, in
the order the CPU reports it. For QEMU/KVM this is typically `TCGTCGTCGTCG`
(TCG) or `GenuineIntel` or `AuthenticAMD`.

### Step 3 — lythd spawns lythdist

After reading BootInfo, lythd uses `SYS_EXEC` to launch `lythdist`, the
service manager. It passes an appropriate subset of its capabilities to
`lythdist` via the caps array argument. The exact capability delegation
policy is defined in the OROS repository.

### Step 4 — normal operation

lythd then enters a loop, typically:
- Listening on an IPC endpoint for service registration requests.
- Reaping dead tasks (no waitpid yet — this is future work).
- Optionally handling the rollback capability if a checkpoint/restore
  mechanism is implemented.

---

## Capability delegation policy

lythd starts with all capabilities. It is responsible for splitting and
attenuating them before passing to children:

- **Memory capability** — lythd should derive restricted Memory caps for
  each child, limiting them to a range of physical frames. (Range
  restriction is a planned kernel feature; for now, child tasks receive
  a copy of the full Memory cap.)
- **Rollback capability** — should be held by lythd only, not delegated
  to untrusted code.
- **IPC endpoints** — lythd creates new IPC endpoints (via `SYS_IPC_CREATE`)
  for each service and passes capabilities to the appropriate parties.

---

## Relationship to OROS

OROS (the userspace repository) contains:
- `lythd` source code
- `lythdist` service manager
- `lysh` interactive shell
- `lythos-std` — a thin library wrapping Lythos syscalls for userspace use

The kernel only specifies the binary protocol (capabilities at fixed handles,
BootInfo layout, syscall ABI). lythd's internal structure is an OROS concern.
