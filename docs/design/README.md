# Lythos — Internal Documentation

Lythos is an x86_64 microkernel written in Rust. This documentation covers
the kernel internals for developers working on or extending Lythos itself.
For writing userspace software that runs *on* Lythos, see the OROS repository.

---

## Documents

| File | Contents |
|------|----------|
| [architecture.md](architecture.md) | High-level design, component map, key invariants |
| [boot.md](boot.md) | Boot sequence from Multiboot handoff to `kmain` |
| [memory.md](memory.md) | Physical memory manager, virtual memory manager, kernel heap |
| [tasks.md](tasks.md) | Task model, scheduler, context switching, user/kernel stacks |
| [syscalls.md](syscalls.md) | Full syscall reference — numbers, arguments, return values, errors |
| [capabilities.md](capabilities.md) | Capability system design and operations |
| [ipc.md](ipc.md) | IPC endpoints — ring buffer layout, send/recv semantics, blocking |
| [elf.md](elf.md) | ELF64 loader — segment loading, stack layout, exec trampoline |
| [drivers.md](drivers.md) | IOAPIC driver, VirtIO block device driver |
| [rfs.md](rfs.md) | RFS filesystem — on-disk format, mkrfs tool, kernel driver |
| [lythd.md](lythd.md) | lythd init process — boot protocol, capability handoff, BootInfo |

---

## Quick reference

**Build:**
```
cargo build                    # debug
cargo build --release          # release
```

**Run:**
```
qemu-system-x86_64 -kernel target/x86_64-lythos/debug/lythos \
    -serial stdio -display none
```

**Debug triple faults:**
```
qemu-system-x86_64 -kernel ... -serial stdio -display none -d int,cpu_reset
```

**Format a disk image:**
```
cd tools/mkrfs && make
./mkrfs disk.img 64M [rootfs/]
```

The kernel boots to `[integration] all checks passed` (14 build steps complete).
