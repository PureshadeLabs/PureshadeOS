# Architecture

## Design philosophy

Lythos is a capability-based microkernel. The kernel is minimal: it provides
memory management, scheduling, inter-process communication, and a capability
system. All device drivers, filesystems, and services run as userspace
processes (`lythd` and its children). The kernel enforces isolation between
processes through hardware paging and the capability system — a process can
only access a resource if it holds a capability that grants it.

The kernel is single-processor at present. Multi-AP support is tracked in
`TODO.md` and requires a per-CPU scheduler and APIC startup sequence.

---

## Component map

```
┌─────────────────────────────────────────────────────────┐
│                     Userspace (ring 3)                  │
│  lythd (PID 1)  ──  lythdist  ──  lysh  ──  other tasks │
│                        ↕ syscall                        │
├─────────────────────────────────────────────────────────┤
│                      Kernel (ring 0)                    │
│                                                         │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐  ┌────────┐   │
│  │  syscall │  │   task   │  │   cap    │  │  ipc   │   │
│  │ dispatch │  │scheduler │  │  table   │  │  rings │   │
│  └──────────┘  └──────────┘  └──────────┘  └────────┘   │
│                                                         │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐               │
│  │   PMM    │  │   VMM    │  │   heap   │               │
│  │ (bitmap) │  │ (4-level)│  │(ll-alloc)│               │
│  └──────────┘  └──────────┘  └──────────┘               │
│                                                         │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐               │
│  │  IOAPIC  │  │ virtio   │  │   ELF    │               │
│  │  driver  │  │  -blk    │  │  loader  │               │
│  └──────────┘  └──────────┘  └──────────┘               │
└─────────────────────────────────────────────────────────┘
                    ↕ MMIO / port I/O
┌─────────────────────────────────────────────────────────┐
│                       Hardware                          │
│    QEMU: x86_64, VirtIO block, LAPIC, IOAPIC            │
└─────────────────────────────────────────────────────────┘
```

---

## Key source files

| File                          | Role                                                                 |
| ----------------------------- | -------------------------------------------------------------------- |
| `src/main.rs`                 | `kmain`, boot sequence, smoke tests, `step14_smoke` integration test |
| `src/arch/x86_64/boot.s`      | 32→64-bit stub, Multiboot1/2 headers, BSS zero, page table bootstrap |
| `src/arch/x86_64/isr_stubs.s` | ISR stubs for vectors 0–31, `exception_common`, `isr_stub_table`     |
| `src/pmm.rs`                  | Physical memory manager                                              |
| `src/vmm.rs`                  | Virtual memory manager                                               |
| `src/heap.rs`                 | Kernel heap (`GlobalAlloc`)                                          |
| `src/gdt.rs`                  | GDT: null, kernel code/data, user code/data, TSS descriptor          |
| `src/tss.rs`                  | TSS: RSP0 for syscall/interrupt stack switch                         |
| `src/idt.rs`                  | IDT: ISR wiring, PIC remapping, `register_irq`                       |
| `src/exceptions.rs`           | Exception handler, page fault diagnostics                            |
| `src/apic.rs`                 | Local APIC init, calibrated timer, tick counter, `apic_eoi`          |
| `src/task.rs`                 | Task struct, round-robin scheduler, context switch                   |
| `src/serial.rs`               | COM1 UART, `SpinLock<T>`, `kprint!` / `kprintln!`                    |
| `src/syscall.rs`              | `syscall_entry` (asm), `syscall_dispatch`, `enter_userspace`         |
| `src/cap.rs`                  | Capability system                                                    |
| `src/ipc.rs`                  | IPC endpoints — ring buffer, blocking send/recv                      |
| `src/elf.rs`                  | ELF64 loader                                                         |
| `src/ioapic.rs`               | IOAPIC driver                                                        |
| `src/pci.rs`                  | PCI config space scanner                                             |
| `src/virtio_blk.rs`           | VirtIO legacy block device driver                                    |
| `linker.ld`                   | Memory layout: `.boot` at 0x100000, kernel sections, symbols         |

---

## Memory map

| Region                    | Address                       | Notes                          |
| ------------------------- | ----------------------------- | ------------------------------ |
| Kernel load               | `0x0010_0000`                 | Physical load; identity-mapped |
| Identity map              | `0x0000_0000` – `0x4000_0000` | 1 GiB, 2 MiB huge pages, no NX |
| Higher-half (data window) | `0xFFFF_8000_0000_0000 + pa`  | Same physical, NX set          |
| Kernel heap               | `0xFFFF_C000_0000_0000`       | 64 MiB, pre-mapped at boot     |
| IPC kernel window         | `0xFFFF_D000_0000_0000`       | One 4 KiB page per endpoint    |
| VMM scratch               | `0xFFFF_A000_xxxx_xxxx`       | Used in VMM smoke test         |
| User code                 | `0x0000_0001_0000_0000`+      | Above 1 GiB identity map       |
| User stacks               | `0x0000_7FFF_0000_0000`+      | 2050 pages per slot            |
| lythd ELF blob            | `0x0040_0000` (phys)          | Loaded by QEMU device loader   |

---

## Critical invariants

**BSS zeroing.** `boot.s` zeros `__bss_start...__bss_end` before building page
tables. The Multiboot1 header sets `bss_end_addr = 0` to skip loader zeroing;
the stub owns it entirely.

**U/S bit propagation.** Every page table level must have the U/S bit set for
user-mode accesses. `vmm::walk_or_create` propagates `entry |= 1 << 2` on all
intermediate tables when the U/S flag is present in `flags`.

**No `map_page` in 0–1 GiB.** That range uses 2 MiB huge pages. Calling
`map_page` on those addresses will trip the PS=1 guard in `walk_or_create`.

**Single-threaded allocators.** `alloc_frame`, `alloc_frames_contiguous`, and
`NEXT_STACK_SLOT` are not locked. This is safe while the kernel is
single-CPU with cooperative scheduling. A spinlock wrapper is needed before
SMP.

**SpinLock disables interrupts.** `serial::SpinLock::lock` saves RFLAGS and
executes `cli`; the guard restores via `popfq` on drop. Any code that holds a
SpinLock must not call `yield_task` or any blocking operation.

---

## Rust notes

- **Edition:** 2024 (nightly).
- **`#![no_std]`** throughout the kernel. Heap uses `alloc` crate via
  `GlobalAlloc`.
- **`static mut` references** are forbidden in Rust 2024. Mutable statics use
  `UnsafeCell<Option<T>>` + `unsafe impl Sync`, accessed through raw pointer
  helpers (`dev_mut` / `dev_ref` pattern in `virtio_blk.rs`).
- **`addr_of!` / `addr_of_mut!`** replace `&raw` references to mutable statics
  in `pmm.rs`.
- **`core::hint::black_box`** is used in smoke tests to prevent the optimizer
  from eliding side-effect-free calls.
