# ELF loader

## Scope

`src/elf.rs` implements a minimal ELF64 loader sufficient to run statically
linked, position-dependent (non-PIE) x86_64 executables. It does not support
shared libraries, dynamic linking, or relocations.

---

## Entry point: `exec`

```rust
pub fn exec(elf_data: &[u8], caps: &[CapHandle]) -> TaskId
```

`exec` performs the following steps:

### 1. Parse ELF header

Validates:
- Magic: `\x7FELF`
- Class: `ELFCLASS64` (2)
- Data: `ELFDATA2LSB` (little-endian)
- Type: `ET_EXEC` (2) — not `ET_DYN`; PIE not supported
- Machine: `EM_X86_64` (62)

Panics on any mismatch — the kernel does not return a graceful error for a
malformed ELF. (User-facing error handling is a future improvement.)

### 2. Load PT_LOAD segments

For each program header where `p_type == PT_LOAD`:

1. Allocate enough 4 KiB frames to cover `[p_vaddr, p_vaddr + p_memsz)`.
2. Map each frame at the corresponding virtual address with flags derived
   from the segment's `p_flags`:
   - `PF_X` (1) → no NX bit (executable)
   - `PF_W` (2) → Write bit
   - `PF_R` (4) → Present bit
   - Always set User bit (ring-3 accessible)
3. Copy `p_filesz` bytes from the ELF data at `p_offset` into the segment.
4. Zero-fill `p_memsz - p_filesz` bytes (BSS padding).

`p_vaddr` must be 4 KiB-aligned. Segments may not overlap.

### 3. Allocate user stack

`alloc_user_stack` claims the next slot from `NEXT_STACK_SLOT` and maps
2048 usable 4 KiB pages starting one page above the guard:

```
0x0000_7FFF_0000_0000 + slot × (2050 × 4096)
  [0]        guard page   — unmapped
  [1..2048]  usable stack — P + W + U, NX
  [2049]     gap page     — unmapped
```

The initial stack pointer is set to the top of the usable region
(`slot_base + 2049 * 4096`), then decremented to write the initial frame.

### 4. Write initial ABI stack frame

The x86_64 SysV ABI requires a valid frame on the stack at `_start`. `exec`
writes:

```
rsp →  argc    (u64) = 0
       argv[0] (u64) = 0   (NULL terminator)
       envp[0] (u64) = 0   (NULL terminator)
```

Stack pointer is decremented by 24 bytes to make room, then aligned down to
16 bytes.

### 5. Inherit capabilities

The `caps` slice is copied into the new task's `CapabilityTable`:
- Handle 0 → `caps[0]`
- Handle 1 → `caps[1]`
- Handle N → `caps[N]`

Handles are cloned from the kernel's internal representation; the caller
retains its own copies.

### 6. Spawn exec trampoline

A kernel task is spawned running `exec_trampoline(entry, stack_top)`. The
trampoline calls `enter_userspace(entry, stack_top)` which uses `iretq` to
switch to ring 3:

- CS = user code segment (ring 3, 64-bit)
- SS = user data segment (ring 3)
- RIP = ELF `e_entry`
- RSP = stack top (after ABI frame)
- RFLAGS = `IF` set (interrupts enabled), everything else cleared

The trampoline is a kernel task rather than a direct `iretq` from `exec`
because `exec` may be called from within a kernel task context that needs
to continue after spawning.

---

## Embedded ELF blobs

`src/elf.rs` contains several ELF binaries compiled into the kernel image
as `&[u8]` byte slices (via `include_bytes!`):

| Symbol | Purpose |
|--------|---------|
| `SMOKE_ELF` | Simple userspace smoke test (step 11) |
| `LYTHD_ELF` | lythd init process; also loaded from phys 0x400000 |
| `IPC_SENDER_ELF` | Step 14 IPC sender task |
| `IPC_RECEIVER_ELF` | Step 14 IPC receiver task |

These are compiled from `src/elf/` userspace sources. They target the same
x86_64-lythos toolchain and link against no libraries — each is a tiny
standalone ELF that makes syscalls directly.

---

## Known limitations

- **No ASLR.** Segments are loaded at their `p_vaddr` exactly.
- **No relocation support.** Only `ET_EXEC` (fixed-address) ELFs work.
- **No dynamic linker.** All code must be statically linked.
- **Page table per process is the kernel's page table.** There is no
  per-process PML4 yet — all tasks share the kernel mapping. User segments
  are mapped into the shared table. Full process isolation via per-process
  page tables is a planned future step.
- **Segment overlap not checked.** If two PT_LOAD segments overlap in
  virtual address space, the second will silently overwrite the first.
