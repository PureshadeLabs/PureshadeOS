# Lythos (Capability-Aware System Kernel) — kernel development reference

Lythos is a completed x86_64 microkernel written in Rust. All 14 build steps are done and the kernel boots cleanly to `[integration] all checks passed` under QEMU. This file is the guide for working on the kernel itself. For writing userspace that runs on lythos, see `OROS/CLAUDE.md`.

---

## Build & run

Use the top-level `Makefile` which orchestrates OROS + kernel + disk image in the right order:

```bash
# build everything (OROS userspace → rootfs/bin/, kernel, disk.img)
make

# run debug kernel under QEMU (with RFS disk)
make run

# run release kernel
make run-release

# run with interrupt/triple-fault tracing
make debug

# build steps individually
make oros    # compile OROS and copy lythd/lythdist/lysh to rootfs/bin/
make kernel  # compile kernel + rebuild disk.img via build.rs
```

QEMU flags used by the Makefile:
```
-serial stdio -display none
-drive file=disk.img,format=raw,if=none,id=hd0
-device virtio-blk-pci,drive=hd0
```

The target is set by default in `.cargo/config.toml` (`x86_64-lythos.json`). The linker script is `linker.ld`.

---

## Key source files

| File | Role |
|------|------|
| `src/main.rs` | `kmain` — boot sequence, all smoke tests, `step14_smoke` integration test |
| `src/arch/x86_64/boot.s` | 32→64-bit boot stub, Multiboot1/2 headers, BSS zeroing, page table bootstrap |
| `src/arch/x86_64/isr_stubs.s` | ISR stubs for vectors 0–31, `exception_common`, `isr_stub_table` |
| `src/pmm.rs` | Physical memory manager — bitmap allocator, `alloc_frame` / `free_frame` |
| `src/vmm.rs` | Virtual memory manager — 4-level paging, `map_page` / `unmap_page`, `init` |
| `src/heap.rs` | Kernel heap — linked-list `GlobalAlloc`, backed by the VMM |
| `src/gdt.rs` | GDT — null, kernel code/data, user code/data, TSS descriptor |
| `src/tss.rs` | TSS — RSP0 for syscall/interrupt stack switch |
| `src/idt.rs` | IDT — wires ISR stubs, PIC remapping, `register_irq` |
| `src/exceptions.rs` | Exception handler, page fault diagnostics |
| `src/apic.rs` | Local APIC init, calibrated timer, tick counter, `apic_eoi` |
| `src/task.rs` | Task struct, scheduler (round-robin), context switch, `yield_task`, `task_exit`, `block_and_yield`, `wake_task` |
| `src/serial.rs` | COM1 UART, `SpinLock<T>` (interrupt-safe), `kprint!` / `kprintln!` |
| `src/syscall.rs` | `syscall_entry` (asm), `syscall_dispatch`, `enter_userspace`, LSTAR/STAR/FMASK setup |
| `src/cap.rs` | Capability system — `CapabilityTable`, `cap_grant`, `cap_revoke`, `cap_cascade_revoke` |
| `src/ipc.rs` | IPC endpoints — ring buffer, `send`, `recv`, blocking/waking |
| `src/elf.rs` | ELF64 loader — `exec()`, stack allocation, smoke/lythd/integration ELF blobs |
| `linker.ld` | Memory layout: `.boot` at 0x100000, `.text`, `.rodata`, `.data`, `.bss`, `KERNEL_START/END`, `__bss_start/__bss_end` |

---

## Memory layout

| Region | Address | Description |
|--------|---------|-------------|
| Kernel load | `0x0010_0000` | Physical load address; identity-mapped |
| Identity map | `0x0000_0000`–`0x4000_0000` | 0→1 GiB, 2 MiB huge pages, no NX (kernel executes here) |
| Higher-half kernel | `0xFFFF_8000_0000_0000 + pa` | Kernel image also mapped here with NX (data window) |
| Kernel heap | `0xFFFF_C000_0000_0000` | 64 MiB heap region, `HEAP_INIT_PAGES` pre-mapped |
| IPC kernel window | `0xFFFF_D000_0000_0000` | One 4 KiB page per IPC endpoint, sequential |
| Framebuffer | `0xFFFF_E000_0000_0000` | Linear framebuffer MMIO mapped here (PCD flag set); `src/framebuffer.rs` |
| VMM scratch | `0xFFFF_A000_xxxx_xxxx` | Used in VMM smoke test |
| User code (SMOKE_ELF) | `0x0000_0001_0000_0000` | Above 1 GiB identity map |
| User code (LYTHD_ELF) | `0x0000_0001_0000_0000` | Same VA as SMOKE_ELF; sequential exec, never concurrent |
| User code (IPC_SENDER) | `0x0000_0002_0000_0000` | Step 14 sender |
| User code (IPC_RECEIVER) | `0x0000_0003_0000_0000` | Step 14 receiver |
| User stacks | `0x0000_7FFF_0000_0000`+ | 2050 pages per slot (1 guard + 2048 usable + 1 gap), allocated by `alloc_user_stack` |

---

## Syscall ABI

See `src/syscall.rs`. Entry via `syscall` instruction (LSTAR = `syscall_entry`).

| Register | Role |
|----------|------|
| RAX | syscall number on entry; return value on exit |
| RDI | argument 1 (a1) |
| RSI | argument 2 (a2) |
| RDX | argument 3 (a3) |
| R10 | argument 4 (a4) — RCX is clobbered by `syscall` |
| R8 | argument 5 (a5) |
| R9 | argument 6 (a6) |
| RCX | user RIP (saved by CPU for `sysretq`) |
| R11 | user RFLAGS (saved by CPU for `sysretq`) |

FMASK clears IF on entry. Interrupts are disabled during the syscall handler.

| Nr | Name | Arguments | Returns |
|----|------|-----------|---------|
| 0 | SYS_YIELD | — | 0 |
| 1 | SYS_TASK_EXIT | — | never |
| 2 | SYS_MMAP | a1=virt, a2=(ignored), a3=flags | 0 (kernel picks frame) |
| 3 | SYS_MUNMAP | a1=virt | 0 |
| 4 | SYS_CAP_GRANT | a1=handle, a2=target_task_id, a3=rights_mask | new handle or error |
| 5 | SYS_CAP_REVOKE | a1=handle | 0 or error |
| 6 | SYS_IPC_SEND | a1=cap_handle, a2=msg_ptr, a3=msg_len | 0 |
| 7 | SYS_IPC_RECV | a1=cap_handle, a2=buf_ptr, a3=buf_len | bytes received |
| 8 | SYS_IPC_CREATE | — | new cap handle |
| 9 | SYS_ROLLBACK | — | never (requires Rollback cap) |
| 10 | SYS_EXEC | a1=elf_ptr, a2=elf_len, a3=caps_ptr, a4=caps_len | new TaskId |
| 11 | SYS_LOG | a1=str_ptr, a2=str_len | 0 or EINVAL |
| 12 | SYS_IPC_SEND_CAP | a1=cap_handle, a2=msg_ptr, a3=msg_len, a4=cap_to_send | 0 |
| 13 | SYS_IPC_RECV_CAP | a1=cap_handle, a2=buf_ptr, a3=buf_len, a4=out_handle_ptr | bytes received |
| 14 | SYS_SERIAL_READ | a1=buf_ptr, a2=buf_len | bytes read (blocking) |
| 15 | SYS_TIME | — | ms since boot |
| 16 | SYS_TASK_STATUS | a1=task_id | 0=dead, 1=running/ready, 2=blocked |
| 17 | SYS_TASK_LIST | a1=buf_ptr (*mut TaskInfo), a2=capacity | entries written (24 B each) |
| 18 | SYS_MEM_STAT | — | free frame count |
| 19 | SYS_TASK_KILL | a1=task_id | 0 or EINVAL |
| 20 | SYS_BLK_READ | a1=sector, a2=buf_ptr (512 B) | 0 or error |
| 21 | SYS_BLK_WRITE | a1=sector, a2=buf_ptr (512 B) | 0 or error |
| 22 | SYS_OPEN | a1=path_ptr, a2=path_len | fd (≥ 0) or error |
| 23 | SYS_READ | a1=fd, a2=buf_ptr, a3=len | bytes read or error |
| 24 | SYS_WRITE | a1=fd, a2=buf_ptr, a3=len | bytes written or error |
| 25 | SYS_CLOSE | a1=fd | 0 or error |
| 26 | SYS_STAT | a1=path_ptr, a2=path_len, a3=stat_ptr (48 B) | 0 or error |
| 27 | SYS_READDIR | a1=path_ptr, a2=path_len, a3=buf_ptr, a4=buf_len | entry count or error |
| 28 | SYS_CREATE | a1=path_ptr, a2=path_len | writable fd (≥ 0) or error |
| 29 | SYS_UNLINK | a1=path_ptr, a2=path_len | 0 or error |
| 30 | SYS_SERIAL_AVAIL | — | bytes available in serial RX buffer |
| 31 | SYS_TASK_WAIT | a1=task_id | 0 (blocks until target exits) |
| 32 | SYS_MKDIR | a1=path_ptr, a2=path_len | 0 or error |
| 33 | SYS_NANOSLEEP | a1=nanoseconds | 0 (rounds up to next ms) |
| 34 | SYS_TASK_SETPRIORITY | a1=task_id, a2=priority (0=low,1=normal,2=high) | 0 or EINVAL |
| 35 | SYS_RENAME | a1=old_ptr, a2=old_len, a3=new_ptr, a4=new_len | 0 or error |
| 36 | SYS_SEEK | a1=fd, a2=offset (i64), a3=whence (0=SET,1=CUR,2=END) | new offset or error |
| 37 | SYS_PS | a1=buf_ptr (*mut PsEntry), a2=capacity | entries written (48 B each) |
| 38 | SYS_BRK | a1=new_break (0=query) | new break addr or EINVAL |
| 39 | SYS_IPC_POLL | a1=cap_handle, a2=buf_ptr, a3=buf_len | bytes received or EAGAIN |
| 40 | SYS_IPC_BIND | a1=cap_handle, a2=name_ptr, a3=name_len (≤128) | 0 or ENOSYS if taken |
| 41 | SYS_IPC_LOOKUP | a1=name_ptr, a2=name_len, a3=rights_mask | new CapHandle or ENOENT |
| 42 | SYS_IPC_RECV_TIMEOUT | a1=cap_handle, a2=buf_ptr, a3=buf_len, a4=timeout_ms | bytes or EAGAIN |
| 43 | SYS_IPC_SEND_TIMEOUT | a1=cap_handle, a2=msg_ptr, a3=msg_len, a4=timeout_ms | 0 or EAGAIN |
| 50 | SYS_SOCKET | — | socket fd or ENOSYS |
| 51 | SYS_BIND | a1=socket_fd, a2=port (u16) | 0 or EINVAL |
| 52 | SYS_SENDTO | a1=fd, a2=buf_ptr, a3=len, a4=dst_ip (u32 BE), a5=dst_port (u16 BE) | 0 or error |
| 53 | SYS_RECVFROM | a1=fd, a2=buf_ptr, a3=len, a4=src_ip_out (*mut u32), a5=src_port_out (*mut u16) | bytes received |
| 54 | SYS_NET_CLOSE | a1=fd | 0 or EBADF |

Error codes (returned as large u64 values, equivalent to negative i64):

| Value | Name | Meaning |
|-------|------|---------|
| `(-1i64) as u64` | ENOSYS | Unknown syscall |
| `(-2i64) as u64` | ENOCAP | Invalid or stale capability handle |
| `(-3i64) as u64` | ENOPERM | Insufficient capability rights |
| `(-4i64) as u64` | EINVAL | Invalid argument (bad task ID, self-grant, etc.) |
| `(-5i64) as u64` | ENOENT | Name not found (IPC lookup, file not found) |
| `(-6i64) as u64` | EBADF | Bad file descriptor |
| `(-7i64) as u64` | EAGAIN | Non-blocking op would block (IPC poll, send/recv timeout) |

---

## Capability system

Defined in `src/cap.rs`. Users hold opaque `CapHandle(u64)` values; the kernel maps them to `Capability` entries in a per-task `CapabilityTable`.

**CapKind values:** `Memory`, `Ipc`, `Rollback`

**CapRights bitflags:** `READ = 1`, `WRITE = 2`, `GRANT = 4`, `REVOKE = 8`, `ALL = 15`

**Operations:**
- `cap_grant(from, handle, target_id, to, rights_mask)` — copies cap to another task, rights intersected with mask. Requires `GRANT` right.
- `cap_revoke(table, handle)` — removes from holder's table. Requires `REVOKE` right.
- `cap_cascade_revoke(table, handle, resolver)` — removes from holder and all derived holders.

Passing an invalid/out-of-range handle returns `ENOCAP`. Insufficient rights returns `ENOPERM`.

---

## IPC

Defined in `src/ipc.rs`.

- An IPC endpoint is a 4 KiB physical page shared between kernel and processes.
- Ring buffer layout (within the page): `head: u32` (offset 0), `tail: u32` (offset 4), `data: [u8; 4088]` (offset 8).
- Fixed 64-byte message slots. `RING_CAPACITY = 63`.
- `MSG_SIZE = 64` bytes.
- `send` blocks (suspends task) if ring is full. `recv` blocks if ring is empty.
- A blocked task is woken when the condition changes.
- Kernel maps each endpoint at `0xFFFF_D000_0000_0000 + idx * 4096` for its own access.

---

## Task model

Defined in `src/task.rs`.

- `TaskId` is a `u64`. The bootstrap task (kmain) is task 0.
- States: `Ready`, `Running`, `Blocked`, `Dead`.
- Scheduler is round-robin over the ready queue.
- `yield_task()` — cooperative yield; also called from `SYS_YIELD`.
- `task_exit()` — marks task Dead, switches to next; never returns.
- `block_and_yield()` — marks task Blocked, yields; used by IPC recv/send when ring is empty/full.
- `wake_task(id)` — moves task from Blocked back to Ready.
- `spawn_kernel_task(fn)` — creates a kernel-mode task.
- `spawn_userspace_task(entry, stack, caps, trampoline)` — creates a ring-3 task with inherited caps.

---

## ELF loading

Defined in `src/elf.rs`. `exec(elf_data, caps)`:
1. Parses ELF64 header — must be `ET_EXEC`, `EM_X86_64`, little-endian.
2. Loads all `PT_LOAD` segments: allocates frames, maps at `p_vaddr` with appropriate flags, copies file data, zero-fills BSS padding.
3. Allocates a user stack via `alloc_user_stack()` — one slot from `NEXT_STACK_SLOT`, 2050 pages wide (guard + 2048 usable + gap), starting at `0x0000_7FFF_0000_0000`.
4. Writes initial ABI stack frame (argc=0, NULL argv, NULL envp) below stack top.
5. Inherits `caps` slice into the new task's capability table — handle 0 = caps[0], handle 1 = caps[1], etc.
6. Spawns a kernel task running `exec_trampoline`, which calls `enter_userspace(entry, stack_top)` via `iretq`.

---

## lythd boot protocol

At kernel boot, `main.rs` calls `exec(LYTHD_ELF, &[mem_cap, rollback_cap, boot_cap])`.

The new process receives three capabilities at fixed handles:
- **Handle 0** (`mem_cap`) — `CapKind::Memory`, `CapRights::ALL`, covers all free physical frames at boot.
- **Handle 1** (`rollback_cap`) — `CapKind::Rollback`, `CapRights::ALL`, grants `SYS_ROLLBACK` access.
- **Handle 2** (`boot_cap`) — `CapKind::Ipc`, `CapRights::ALL`, IPC endpoint with one pre-queued `BootInfo` message.

The `BootInfo` message (exactly 64 bytes, `MSG_SIZE`) is pre-queued before exec:

```
offset  0: u64  signature   = 0xB007_1000_B007_1000
offset  8: u64  mem_bytes   = free_frames * 4096
offset 16: u64  free_frames = PMM free frame count at boot
offset 24: [u8; 12] vendor  = CPUID leaf 0 vendor string (EBX+EDX+ECX)
offset 36: [u8; 28] _pad    = zeroed
```

lythd's first action should be `SYS_IPC_RECV` on handle 2 to consume this message, then proceed with spawning `lythdist`.

---

## SpinLock — interrupt safety

`serial::SpinLock<T>` disables interrupts on `lock()` (saves RFLAGS, executes `cli`) and restores them on drop (`popfq`). This prevents APIC timer preemption while the lock is held, which would deadlock if the ISR also tries to acquire it. All `kprintln!` calls are safe to make from any context.

---

## Important invariants

- **BSS must be zeroed before first use.** `boot.s` zeros `__bss_start...__bss_end` with `rep stosl` before building page tables. The Multiboot1 header sets `bss_end_addr = 0` (skip loader zeroing), so the boot stub owns this.
- **U/S bit must be set at every page table level for user-mode accesses.** `vmm::walk_or_create` propagates `entry.0 |= 1 << 2` on all intermediate entries when `flags` includes the USER bit.
- **Do not call `map_page` on addresses in the 0→1 GiB identity range.** Those are huge pages; `walk_or_create` will panic on the PS=1 entry.
- **`NEXT_STACK_SLOT` starts at 0 and increments per `exec` call.** Never call `alloc_user_stack` from concurrent contexts (single-threaded kernel makes this safe currently).

## graphify

This project has a graphify knowledge graph at graphify-out/.

Rules:
- Before answering architecture or codebase questions, read graphify-out/GRAPH_REPORT.md for god nodes and community structure
- If graphify-out/wiki/index.md exists, navigate it instead of reading raw files
- For cross-module "how does X relate to Y" questions, prefer `graphify query "<question>"`, `graphify path "<A>" "<B>"`, or `graphify explain "<concept>"` over grep — these traverse the graph's EXTRACTED + INFERRED edges instead of scanning files
- After modifying code files in this session, run `graphify update .` to keep the graph current (AST-only, no API cost)
