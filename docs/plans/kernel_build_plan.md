# Lythos (Capability-Aware System Kernel) — 14-Step Build Plan

**Target**: x86_64 (primary), aarch64 (secondary)
**Language**: Rust
**Revision**: 2.0

---

## Scope

Lythos (Capability-Aware System Kernel) is a **microkernel**. This repo contains only kernel code. All drivers, daemons, and userspace programs (`lythd`, `lythdist`, `lythmsg`, etc.) live in the **OROS** repo and run in ring 3.

The kernel exposes exactly four syscall categories: memory management, IPC primitives, capability operations, and scheduling. Nothing else belongs in this repo.

---

## Step 1 — Bare-metal boot target

**Goal**: Produce a bootable binary that transfers control to Rust code.

**Implementation**:

- Create a `no_std`, `no_main` Rust workspace with a custom target spec: `x86_64-lythos.json`.
- Target fields: `"os": "none"`, `"panic-strategy": "abort"`, `"disable-redzone": true`, `"features": "-mmx,-sse,+soft-float"` (SSE must be disabled before stack setup).
- Write a 16→32→64-bit assembly stub (`boot.asm`) using NASM or inline `global_asm!`. The stub sets up a minimal GDT, switches to 64-bit long mode, sets up a temporary stack pointer, then calls `kmain()`.
- Use a linker script (`linker.ld`) to place `.boot` section at `0x100000`, define `KERNEL_START` and `KERNEL_END` symbols for later use by the physical memory allocator.
- Boot via GRUB2 Multiboot2 header for now; replace with a UEFI stub in Step 2 or later.
- Verify with QEMU: `qemu-system-x86_64 -kernel kernel.elf -serial stdio`.

**aarch64 note**: Add a second target spec `aarch64-lythos.json`. The entry stub uses `bl kmain` after setting `SP` to a static stack region. QEMU target: `qemu-system-aarch64 -M virt -cpu cortex-a57`.

---

## Step 2 — GDT, IDT, and exception handling

**Goal**: Install a valid Global Descriptor Table, Interrupt Descriptor Table, and handle CPU exceptions without triple-faulting.

**Implementation**:

- Define GDT in Rust as a `static` array of `u64` descriptors. Entries needed: null, kernel code (ring 0), kernel data (ring 0). Load with `lgdt`.
- Define IDT as a `static` array of 256 `IdtEntry` structs. Each entry holds a handler pointer, selector, and flags.
- Write ISR stubs in `global_asm!` for vectors 0–31 (CPU exceptions). Each stub pushes a fake error code if the CPU doesn't, pushes the vector number, then jumps to a common `exception_handler` written in Rust.
- `exception_handler` receives a `*const ExceptionFrame` (saved registers + vector + error code). For now, print the vector to the serial port and halt.
- Install the `#PF` (14) handler with a real page fault dispatcher stub — it will be wired to the VMM in Step 5.
- Load IDT with `lidt`.
- Test by intentionally triggering a divide-by-zero (`int $0x0`); confirm the handler fires and the system doesn't triple-fault.

**aarch64 note**: Replace GDT/IDT with exception vector table at a 2KB-aligned address. Set `VBAR_EL1`. Handle synchronous exceptions (`ESR_EL1` decoding) and IRQs via the same Rust dispatcher pattern.

---

## Step 3 — Serial output and early logging

**Goal**: Establish a serial port logger usable before any memory allocator exists.

**Implementation**:

- Implement a `SerialPort` struct wrapping x86 I/O port `0x3F8` (COM1). Methods: `init()`, `write_byte(u8)`, `write_str(&str)`.
- Use `x86_64::instructions::port::Port` or raw `in`/`out` instructions via `global_asm!`.
- Implement `core::fmt::Write` for `SerialPort`. This enables `write!` macros.
- Define a `kprint!` / `kprintln!` macro that calls into a global `SerialPort` wrapped in a spin lock (a raw `AtomicBool` spinlock is sufficient here — no heap needed).
- All subsequent steps use `kprintln!` for debugging.

**aarch64 note**: Use PL011 UART at MMIO base `0x09000000` (QEMU virt board). Same `Write` trait implementation, different register offsets.

---

## Step 4 — Physical memory manager

**Goal**: Track all usable physical memory as page frames; support alloc and free of 4KB pages.

**Implementation**:

- Parse the Multiboot2 memory map tag at boot to enumerate usable RAM regions.
- Implement a bitmap allocator: one bit per 4KB frame. Store the bitmap at a fixed physical address (`KERNEL_END` aligned up to 4KB). Mark kernel + bitmap region as used.
- API: `fn alloc_frame() -> Option<PhysAddr>`, `fn free_frame(PhysAddr)`.
- `PhysAddr` is a newtype over `u64` with alignment assertions.
- Keep a `free_frames: usize` counter for diagnostics.
- Test: alloc 1000 frames, free them, alloc again — confirm same addresses returned and no double-alloc.

**Note**: The bitmap is a temporary early allocator. A buddy allocator can replace it post-Step 8 if needed, but the bitmap is sufficient for the kernel bootstrap.

---

## Step 5 — Virtual memory and paging

**Goal**: Install 4-level paging (PML4→PDPT→PD→PT), map the kernel, and support dynamic page mappings.

**Implementation**:

- Define `PageTable`, `PageTableEntry` structs. `PageTableEntry` wraps `u64` with bit-field accessors: present, writable, user, NX, address.
- Allocate page tables from the physical allocator (Step 4). Root PML4 lives at a known physical address.
- Identity-map the kernel region (`0x100000`–`KERNEL_END`) + the serial MMIO range.
- Map a higher-half kernel window at `0xFFFF_8000_0000_0000` for kernel virtual addresses (KASLR deferred).
- Load `CR3` with the PML4 physical address.
- Implement `fn map_page(virt: VirtAddr, phys: PhysAddr, flags: PageFlags)` and `fn unmap_page(virt: VirtAddr)`.
- Wire the `#PF` handler (Step 2) to call a `page_fault_handler(addr, error_code)` that logs the faulting address and halts.
- TLB: `invlpg` on unmap and remap.

**aarch64 note**: Use 4-level page tables via `TTBR0_EL1` (user) and `TTBR1_EL1` (kernel). Page table descriptor format differs — use a separate `AArch64PageTableEntry` type.

---

## Step 6 — Heap allocator (kernel slab/linked-list) ✅


**Goal**: Enable `alloc::` crate usage inside the kernel — `Box`, `Vec`, `Arc`, etc.

**Implementation**:

- Carve a kernel heap region from virtual memory (e.g. `0xFFFF_C000_0000_0000`, 64MB initially). Back it with physical frames on demand via the page fault handler or by pre-mapping a fixed range.
- Implement a linked-list allocator as a `GlobalAlloc` impl. Each free block stores a header with size and a pointer to the next free block. `alloc` walks the list for a fit; `dealloc` reinserts and optionally coalesces.
- Register via `#[global_allocator]`.
- Set `#[alloc_error_handler]` to `kprintln!` + halt.
- Test: allocate `Box::new(42u64)`, `Vec::<u8>::with_capacity(1024)`, confirm no panics.

**Note**: A slab allocator per fixed size class (16, 32, 64, 128, 256 bytes) significantly reduces fragmentation and is worth adding before capability token allocation (Step 9), since tokens will be small, frequently allocated objects.

---

## Step 7 — Scheduling: kernel tasks and context switching ✅

**Goal**: Define a task abstraction, implement cooperative context switching, add task exit and blocking, then wire preemptive scheduling to the APIC timer (Step 8).

**Implementation**:

- Define `Task` struct: `id: TaskId`, `state: TaskState` (Running/Ready/Blocked/Dead), `kernel_stack: VirtAddr`, `page_table: PhysAddr`, `context: TaskContext`.
- `TaskContext` holds callee-saved registers: `rsp`, `rbp`, `rbx`, `r12–r15` (x86_64).
- Implement `fn switch_context(from: &mut TaskContext, to: &TaskContext)` in `global_asm!`.
- Implement a simple round-robin ready queue.
- `task_exit()`: mark current task `Dead`, drop it from the queue, schedule next. Must never return.
- `block_task(id)` / `wake_task(id)`: transition a task between `Blocked` and `Ready`. Used by IPC `park`/`unpark` in Step 11.
- Cooperative first: `yield_task()` calls `schedule()` directly.
- Preemptive: wire the APIC timer (Step 8) to call `schedule()` from the timer ISR.
- Kernel tasks only at this stage — user-mode (ring 3) added in Step 10.

**aarch64 note**: `TaskContext` holds `x19–x28`, `fp`, `lr`, `sp`. `switch_context` uses `stp`/`ldp` pairs.

---

## Step 8 — Interrupt controller: APIC and timer ✅

**Goal**: Replace PIC with APIC, configure a preemption timer, and support hardware IRQ routing.

**Implementation**:

- Detect and disable the legacy PIC (mask all IRQs, send EOI sequence).
- Map the Local APIC MMIO registers. Base address from `IA32_APIC_BASE` MSR. Map into kernel virtual space.
- Enable the APIC (`APIC_SVR` register, bit 8).
- Configure the APIC timer: divide by 16, one-shot or periodic mode. Calibrate against the PIT or HPET to determine ticks-per-ms. Set the initial count for a 1ms or 10ms tick.
- Install a timer ISR at IDT vector 32. The ISR: increments a global tick counter, sends EOI to APIC, calls `schedule()`.
- Install a spurious IRQ handler at IDT vector 255.
- For I/O APIC: parse ACPI MADT table to find I/O APIC base, configure redirection table entries for IRQ routing. Device drivers (keyboard, disk, etc.) live in OROS userspace — the kernel only routes IRQs to the appropriate capability-holding process.

**aarch64 note**: Use the ARM Generic Timer (`CNTP_TVAL_EL0`, `CNTP_CTL_EL0`) and GICv2/GICv3 (distributor + CPU interface MMIO). QEMU virt exposes GICv2 at `0x08000000`.

---

## Step 9 — Capability system ✅

**Goal**: Implement the core capability model: unforgeable tokens that grant access to kernel objects and IPC memory regions.

**Note**: This is the kernel's side of what `lythdist` (a OROS daemon) sits on top of. The kernel provides the primitive operations; `lythdist` is the userspace policy layer that distributes them at boot.

**Implementation**:

- Define `Capability` as a struct: `id: CapId` (monotonically incrementing `u64`), `kind: CapKind` (Memory, IPC, Device, etc.), `rights: CapRights` (Read, Write, Grant, Revoke as bitflags), `object: KernelObjectRef`.
- `KernelObjectRef` is a typed reference into a kernel object table (arena-allocated, indexed by a generation-tagged handle to prevent use-after-free).
- Capability tokens are never exposed directly to userspace — processes hold opaque `CapHandle` integers. The kernel maintains a per-process `CapabilityTable` mapping `CapHandle → Capability`.
- Implement `cap_grant(from_task, handle, to_task, rights_mask)`: copies the capability into the recipient's table with rights intersected by `rights_mask`. This is the only way capabilities propagate.
- Implement `cap_revoke(handle)`: removes from the holder's table. If the capability has the Revoke right, it can revoke all derived copies (tracked via a child list on the parent capability).
- No capability can be forged from userspace — all `CapHandle` values are indices into a kernel-managed table. Passing an out-of-range or generation-mismatched handle returns `ENOCAP`.

---

## Step 10 — Userspace: ring 3 tasks and syscall interface ✅

**Goal**: Run unprivileged code, establish the syscall boundary, and implement the minimal syscall surface.

**Implementation**:

- Add user-mode GDT entries (ring 3 code + data) and a TSS with a kernel stack pointer (RSP0) for the privilege switch on syscall entry.
- Set up a user-mode page table layout: kernel mapped at high addresses (not accessible from ring 3 via SMEP/SMAP), user stack and code mapped at low addresses.
- Implement `enter_userspace(entry: VirtAddr, stack: VirtAddr)` using `iretq` (or `sysretq`) to drop to ring 3.
- Install a `syscall`/`sysret` handler via `IA32_LSTAR` MSR. The entry stub swaps to the kernel stack via TSS RSP0, saves user registers, calls a Rust `syscall_dispatch(nr, args)`.
- Implement the four syscall categories:
  - **Memory**: `mmap(len, flags, cap_handle)` — validates capability, maps physical frames into caller's address space.
  - **IPC**: `ipc_send(endpoint_cap, msg_ptr, len)`, `ipc_recv(endpoint_cap, buf_ptr, len)` — backed by shared memory regions (Step 11). Parks the caller via `block_task` if the endpoint buffer is full/empty.
  - **Capability ops**: `cap_grant(handle, target_pid, rights)`, `cap_revoke(handle)`.
  - **Scheduling**: `yield()`, `task_exit(code)` — `task_exit` calls the kernel-level exit added in Step 7.
- Return values: `isize` in `rax`. Errors are negative errno-style codes.

**SMEP/SMAP**: Set CR4 bits at boot to prevent the kernel executing or reading/writing user pages accidentally.

---

## Step 11 — IPC: shared memory regions ✅

**Goal**: Implement the shared-memory async IPC primitive that `lythmsg` (OROS) will run on.

**Implementation**:

- An IPC endpoint is a kernel object: a circular ring buffer in a shared physical page, plus a wait queue.
- `lythdist` (OROS) creates endpoints at boot: it allocates a physical page via `mmap`, maps it into both sender and receiver address spaces, and hands each party a `CapHandle` for the endpoint. The kernel provides the mapping primitive; the policy is `lythdist`'s job.
- The ring buffer layout (in the shared page): `head: AtomicU32`, `tail: AtomicU32`, `data: [u8; N]`. Sender writes to `tail`; receiver reads from `head`. No kernel involvement for the data path — both sides access the shared page directly.
- The kernel is involved only for blocking/waking: `ipc_send` does a fast-path check (is there space?) and calls `block_task` if the buffer is full. `ipc_recv` similarly blocks if empty. The kernel wakes the blocked task via `wake_task` when the condition changes.
- Message framing: fixed 64-byte message slots (or variable with a 4-byte length prefix). Document the format in a shared header consumed by both lythos and OROS.

---

## Step 12 — ELF loader and exec ✅

**Goal**: Load and execute ELF64 binaries into isolated address spaces.

**Implementation**:

- Implement an ELF64 parser: validate magic, check `ET_EXEC` or `ET_DYN`, iterate `PT_LOAD` segments.
- For each `PT_LOAD` segment: allocate physical frames, map into a new page table at `p_vaddr`, copy `p_filesz` bytes, zero-fill `p_memsz - p_filesz`.
- Allocate a user stack (default 8MB, guard page at the bottom — mapped present but with no permissions to catch stack overflow).
- Set up the initial stack frame: `argc`, `argv`, `envp`, and an auxiliary vector (`AT_ENTRY`, `AT_PHDR`, `AT_PHNUM`, `AT_PAGESZ`).
- `exec(elf_data: &[u8], caps: &[CapHandle]) -> TaskId`: creates a new `Task`, loads the ELF, assigns the provided capabilities to the new task's capability table, enqueues the task in the scheduler.
- Static executables only for now (musl statically linked). Dynamic linking deferred.
- Test: compile a minimal Rust binary targeting `x86_64-unknown-linux-musl` with a raw `write` syscall (no `std`), exec it from the kernel, confirm serial output.

---

## Step 13 — First userspace process bootstrap ✅

**Goal**: Exec the first userspace process (`lythd`) from the kernel and hand it its initial capabilities.

**Note**: `lythd`, `lythdist`, and `lythmsg` are **OROS** processes. The kernel's job here is to load and launch `lythd` from an embedded blob or initrd and grant it the root capability set. Everything after that is `lythd`'s responsibility.

**Implementation**:

- At the end of `kmain`, locate the `lythd` ELF binary (embedded in the kernel image or at a known physical address from the bootloader).
- Allocate the initial capability set: a root memory capability covering all free physical frames, and a boot-info IPC endpoint capability containing the hardware topology (parsed from ACPI/CPUID).
- Call `exec(lythd_elf, &[mem_cap, boot_info_cap])` — this is the only process the kernel spawns directly. All subsequent processes are spawned by `lythd` via the `exec` syscall.
- Define a privileged `rollback` syscall gated by a dedicated capability type. Only the process that holds a `CapKind::Rollback` capability (granted exclusively to `lythd` at boot) can invoke it.
- Enter the scheduler. The kernel's boot work is done; `lythd` takes over.

---

## Step 14 — Integration: boot sequence and smoke test ✅

**Goal**: Wire all kernel components together and verify the full boot sequence up to a live userspace.

**Note**: The full boot sequence (lythd → lythdist → lythmsg → services) is defined in the OROS design document and implemented there. The kernel smoke test ends at "lythd is running and can make syscalls."

**Implementation**:

- Verify the kernel boots cleanly through all 13 prior steps with no triple fault.
- Confirm `lythd` receives its initial capabilities and can make all four syscall categories.
- Confirm IPC send/recv between two test userspace tasks succeeds end-to-end.
- Confirm `task_exit` cleans up correctly and the scheduler continues with remaining tasks.
- Confirm a capability-less process cannot access a capability-gated resource (`ENOCAP` returned).
- Run under QEMU with `-d int,cpu_reset` to confirm no unexpected exceptions at any stage.

**Smoke test checklist**:

- [x] Kernel boots through all steps without triple fault.
- [x] `lythd` ELF is loaded and enters ring 3 successfully.
- [x] `lythd` receives and can exercise its initial capabilities.
- [x] IPC shared-memory send/recv works between two userspace tasks.
- [x] `task_exit` removes a task cleanly; scheduler continues.
- [x] Unauthorized capability access returns `ENOCAP`.
- [ ] QEMU `-d int,cpu_reset` shows no unexpected faults. *(manual — run under QEMU to confirm)*

---

## Architecture notes

| Concern         | x86_64                           | aarch64                        |
| --------------- | -------------------------------- | ------------------------------ |
| Boot stub       | NASM, Multiboot2 / UEFI          | `bl kmain` from reset vector   |
| Exception table | IDT, `lidt`                      | `VBAR_EL1` vector table        |
| Syscall entry   | `syscall`/`sysret`, `IA32_LSTAR` | `svc #0`, `ELR_EL1`/`SPSR_EL1` |
| Timer           | APIC timer, calibrated via PIT   | ARM Generic Timer              |
| IRQ controller  | I/O APIC, ACPI MADT              | GICv2/GICv3                    |
| Page tables     | 4-level, `CR3`                   | 4-level, `TTBR0/1_EL1`         |
| Context switch  | `rsp`/callee-saved regs          | `sp`/`x19–x28`, `lr`           |
