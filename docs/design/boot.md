# Boot sequence

## Overview

Lythos boots via Multiboot1 or Multiboot2. QEMU loads the kernel ELF at
physical address `0x100000` and jumps to the 32-bit entry point embedded in
`.boot`. The boot stub switches to 64-bit mode and calls `kmain`.

---

## Stage 1 — 32-bit stub (`boot.s`)

1. **Multiboot headers.** Both MB1 and MB2 headers are present in `.boot`.
   GRUB/QEMU select the appropriate one. The MB1 header sets `bss_end_addr = 0`
   so the loader does not zero BSS — the stub handles that itself.

2. **BSS zeroing.** `rep stosl` clears `__bss_start...__bss_end` before any
   Rust code runs. Without this, static variables start with garbage.

3. **Temporary stack.** A 16 KiB stack in `.bss` is used during boot. It is
   abandoned once `kmain` sets up per-task stacks.

4. **Page table bootstrap.** The stub builds a minimal 4-level page table
   entirely in assembly, sufficient to identity-map the first 1 GiB with 2 MiB
   huge pages (PDE, no NX). This covers the kernel image and BIOS area.

5. **`EFER.LME`, `CR0.PG`.** Long mode is enabled by setting bits in the
   appropriate MSRs and control registers, then a far jump loads a 64-bit code
   segment.

6. **`kmain` call.** The stub pushes the Multiboot magic and info pointer onto
   the stack, then calls `kmain`. `kmain` never returns.

---

## Stage 2 — `kmain` (`main.rs`)

`kmain` runs the following sequence. Each phase is logged to COM1.

| Step | Action |
|------|--------|
| 1 | `serial::init` — UART ready, `kprintln!` works from this point |
| 2 | `gdt::init` — load GDT with kernel/user segments and TSS descriptor |
| 3 | `tss::init` — set RSP0 (used on ring 3 → ring 0 transitions) |
| 4 | `idt::init` — wire ISR stubs to IDT, remap legacy PIC to vectors 32–47, enable interrupts |
| 5 | `pmm::init` — parse Multiboot memory map, mark free frames, reserve kernel + lythd blob + BIOS page |
| 6 | `vmm::init` — install full 4-level page table: identity map, higher-half kernel window, heap pre-map |
| 7 | `heap::init` — activate the linked-list `GlobalAlloc`; `alloc` crate now usable |
| 8 | `apic::init` — disable legacy PIC, enable LAPIC, calibrate timer against PIT |
| 9 | `ioapic::init` — map IOAPIC MMIO, read entry count, mask all GSIs |
| 10 | `task::init` — set up bootstrap task (kmain, task 0), `SCHEDULER` ready |
| 11 | `syscall::init` — set LSTAR, STAR, FMASK; `syscall` instruction routed |
| 12 | `virtio_blk::init` — scan PCI for VirtIO block device, set up virtqueue |
| 13 | Execute lythd ELF (copied from phys `0x400000`), pass `BootInfo` via IPC |
| 14 | `step14_smoke` integration test — verifies caps, IPC, exec, yield, task_exit |

If any step panics, the exception handler prints the vector, error code, and
register state to COM1, then halts.

---

## Multiboot magic values

| Value | Protocol |
|-------|----------|
| `0x2BADB002` | Multiboot1 (GRUB legacy, QEMU a.out kludge) |
| `0x36D76289` | Multiboot2 |

`pmm::init` dispatches to `parse_mb1` or `parse_mb2` based on the magic. An
unknown magic causes a kernel panic immediately.

---

## lythd ELF loading at boot

QEMU loads the lythd ELF binary to physical address `0x400000` before the CPU
starts, using:

```
-device loader,file=lythd,addr=0x400000,force-raw=on
```

This is a raw byte copy, not a Multiboot module. `kmain` reads the ELF from
`0x400000`, copies it to the kernel heap, then calls `elf::exec` to map it
into a new user-mode address space. The PMM reserves frames `0x400000..0x480000`
(512 KiB) at init time so no allocator call overwrites the ELF before `kmain`
has copied it.

See [lythd.md](lythd.md) for the full boot protocol and BootInfo message.
