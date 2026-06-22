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
| `src/heap.rs` | kernel heap (`HEAP_INIT_PAGES = 4096`, 16 MiB — see below) |
| `src/elf.rs` | ELF64 loader, `exec()`, user stack allocation |
| `src/virtio_net.rs` | virtio-net driver — **root cause of heap landmine** |

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

**`net::init` over-allocation (active landmine):** `HEAP_INIT_PAGES = 4096` (16 MiB) in `src/heap.rs` masks a kmain heap-exhaustion panic. Root cause: `net::init()` (called at `src/main.rs:210`, guarded by `if virtio_net::init()`) over-allocates RX/TX buffers. The QEMU target always attaches a virtio-net device, so `net::init()` runs every boot. Shrinking `HEAP_INIT_PAGES` back toward 4 MiB reproduces the panic. **Do not reduce it without fixing the buffer sizing in `src/virtio_net.rs`.**

**`sweep_dead` stack leak:** task stacks are not freed when a task exits. A dedicated stack allocator is needed before this is safe.

**aarch64 syscall stubs:** `abi/lythos-syscall` is x86_64 only — `#[cfg(target_arch = "x86_64")]` guard; aarch64 compiles as an empty module, breaking any userspace link targeting aarch64.

**mtime/ctime timestamps:** `src/rfs.rs` always writes 0 for both fields at inode creation and never updates them on write. Tracked in `docs/plans/followup-code-tasks.md` item 3.
