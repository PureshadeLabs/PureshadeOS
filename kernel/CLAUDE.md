# Lythos kernel — development reference

`no_std` bare-metal Rust. Build-std required for `core`, `alloc`, `compiler_builtins`. Most failure-prone area of the repo — mistakes here produce triple faults, silent corruption, or panics that only manifest under QEMU, not at `cargo check`.

---

## Build

```bash
make kernel          # debug build via Makefile (correct flags + build-std)
make kernel-release
make run             # debug build + QEMU launch
make debug           # QEMU with -d int,cpu_reset
```

Never use bare `cargo build` at the workspace root for the kernel — it will compile host tools instead. The kernel's per-crate `kernel/.cargo/config.toml` sets `[build] target = "targets/x86_64-lythos.json"` and the required build-std flags; these apply when building inside the `kernel/` subtree or via `make kernel`.

---

## Authoritative hardware references

- **Intel SDM** (Software Developer's Manual): privilege levels, exception handling, 4-level paging, MSRs (`LSTAR`/`STAR`/`FMASK` for `syscall`/`sysretq`), APIC, IOAPIC
- **ARM Architecture Reference Manual**: all aarch64 specifics

Do not state x86_64 or aarch64 behavior from memory. Verify against the SDM or ARM ARM, or say you're uncertain. x86_64 and aarch64 code paths must stay **strictly separate** — no shared asm, no blurred assumptions between architectures.

---

## Load-bearing files — silent failures if orphaned

| File | Risk |
|------|------|
| `targets/x86_64-lythos.json` | kernel target spec — removing or corrupting breaks build entirely |
| `tools/lythos-toolchain/target-specs/x86_64-lythos-sysroot.json` | sysroot target — breaks toolchain build |
| `kernel/boot/linker/x86_64.ld` | defines memory layout, section placement, `KERNEL_START/END`, `__bss_start/__bss_end` — any edit risks boot failure |
| `kernel/.cargo/config.toml` | default target + linker flags for the kernel crate; wrong value compiles to host |
| `kernel/boot/` asm files | ISR stubs, 32→64 boot stub, BSS zeroing, page table bootstrap — all are pre-allocator, pre-Rust runtime |

---

## Key source files

| File | Role |
|------|------|
| `src/main.rs` | `kmain` — boot sequence, smoke tests, `lythd` exec |
| `src/cap.rs` | **capability system — security core** |
| `src/syscall.rs` | `syscall_entry` (asm), dispatcher, ABI number → handler mapping |
| `src/ipc.rs` | IPC endpoints, ring buffer, blocking/waking |
| `src/task.rs` | task struct, round-robin scheduler, context switch, `block_and_yield`, `wake_task` |
| `src/vmm.rs` | 4-level paging, `map_page`/`unmap_page` |
| `src/pmm.rs` | physical memory manager, bitmap allocator |
| `src/heap.rs` | kernel heap (coalescing free list; `HEAP_INIT_PAGES = 512`, 2 MiB) |
| `src/elf.rs` | ELF64 loader, `exec()`, user stack allocation |
| `src/pci.rs` | PCI enumeration + device registry (claimable devices for userspace drivers) |
| `src/device.rs` | userspace device-driver framework: Device-cap gate, MMIO map, DMA alloc, IRQ delivery |

---

## Capability system — high risk

`src/cap.rs` is the security core. A bug here is a **capability forge** — it lets userspace obtain or amplify authority it was never granted. Treat every change as high risk:

- Prefer a **failing test that demonstrates a broken invariant** over a quick in-place fix
- Every `cap_grant` path must verify the caller holds `GRANT` right before copying
- `cap_cascade_revoke` must walk **all live tasks** to remove derived handles — missing a task leaves dangling authority

Syscall numbers used by cap operations live in `abi/lythos-abi/src/syscall.rs`, not in this crate.

---

## Syscall dispatch

`src/syscall.rs` maps ABI syscall numbers to handler implementations. The canonical numbers come from `abi/lythos-abi/src/syscall.rs`. When adding or changing a syscall:

1. Update `abi/lythos-abi` first (number, error code, any new struct + assert)
2. Add or update the handler in `src/syscall.rs`
3. Update all userspace callers
4. Verify with `make run` — compilation alone is insufficient

---

## Architecture separation

x86_64-specific code lives in `src/arch/x86_64/`. aarch64 support is stubbed/incomplete. Keep them strictly isolated — do not share asm or assume behavioral equivalence. See deferred work below for the aarch64 status.

---

## Known deferred kernel work

See `docs/plans/followup-code-tasks.md` — read it before declaring something a bug to fix. Items to avoid rediscovering:

**Heap sizing (resolved 2026-07-02):** the old 16 MiB `HEAP_INIT_PAGES` masked free-list fragmentation, not a virtio-net heap over-allocation (the driver is PMM-backed). `dealloc` now coalesces; `HEAP_INIT_PAGES = 512` (2 MiB). Post-mortem: `docs/plans/followup-code-tasks.md` item 5.

**`sweep_dead` stack leak (stale — verified fixed):** `sweep_dead` frees the kernel stack Vec, restores the guard page, and frees the user page table. The `[sweep]`/`[sweep-user]` boot probes in `src/main.rs` measure 0 B heap / 0 frames PMM leaked per spawn/reap and exec/reap cycle; they are feature-gated behind `boot-tests` (build with `make kernel-tests`) since 2026-07-03 — run them before scheduler/PMM/heap changes.

**Networking is userspace now:** the in-kernel virtio-net driver and TCP/IP stack were torn down. Networking is a `netd` driver process that owns the virtio-net device via a `CapKind::Device` capability over the userspace device-driver framework (`src/device.rs`, syscalls 61–66). virtio-blk stays in-kernel and untouched. UDP socket syscalls 50–54 are retired (return `ENOSYS`).

**Device-driver framework follow-ups:** (1) DMA buffers are zeroed on alloc but there is no `SYS_DEV_DMA_FREE` — buffers are reclaimed at task teardown (leaf frames without the `MMIO_NOFREE` PTE bit are returned to the PMM); add wipe-on-free when a free path is needed. (2) On a shared PCI INTx line (device IRQ sharing virtio-blk's GSI), masking-on-fire degrades virtio-blk to polling until `SYS_DEV_IRQ_ACK`; move device drivers to MSI/MSI-X for isolated, edge-triggered vectors. (3) No IOMMU — a Device-cap holder is trusted-for-DMA; program VT-d/virtio-iommu inside `SYS_DEV_DMA_ALLOC` when required.

**aarch64 syscall stubs:** `abi/lythos-syscall` is x86_64 only — `#[cfg(target_arch = "x86_64")]` guard; aarch64 compiles as an empty module, breaking any userspace link targeting aarch64.

**mtime/ctime timestamps:** `src/rfs.rs` always writes 0 for both fields at inode creation and never updates them on write. Tracked in `docs/plans/followup-code-tasks.md` item 3.
