# PureshadeOS — monorepo reference

Lythos microkernel + OROS userspace = PureshadeOS: a capability-based OS for x86_64 (aarch64 stubbed/incomplete). All authority flows through unforgeable kernel-managed capability handles; the kernel is `no_std` bare-metal Rust; userspace (OROS) runs natively on the Lythos ABI. Brand: **PureshadeOS**. Microkernel: **Lythos**. Userspace layer: **OROS**.

---

## This is NOT seL4 / Redox / Fuchsia

The capability model, IPC design, and lythos-compat (planned) are **original** — do not import assumptions from other microkernels:

- No seL4 Cnode/CSpace hierarchy, no formal proof model, no static derivation tree
- No Redox scheme-based VFS at the IPC layer
- No Fuchsia FIDL, VMO, or VMAR primitives

When unsure about any behavior: **read `docs/spec/` or ask**. Do not pattern-match from other systems.

---

## ABI — single source of truth

`abi/lythos-abi` is the verified kernel↔userspace contract. The following live here and nowhere else:

- Syscall numbers + `SYSCALL_MAX = 55` (`syscall.rs`)
- 12 error sentinels (`errno.rs`)
- `CapRights`, `CapKind`, `CapHandle` types (`cap.rs`)
- Boundary structs with size+offset asserts: `TaskInfo`, `PsEntry`, `Stat`, `DirEntry`, `BootInfo` (`structs.rs`)
- IPC ring constants: `MSG_SIZE = 64`, `RING_CAPACITY = 63` (`ipc.rs`)

`abi/lythos-syscall` — userspace `syscall`-instruction stubs (x86_64 only; aarch64 compiles as an empty module — see deferred work).

**ABI changes are cross-cutting.** A single edit can break kernel handlers, userspace callers, and struct parsers simultaneously. Flag before touching; don't make casually.

---

## Spec — ground truth

Read the relevant file before editing a subsystem:

| Spec | When to read |
|------|-------------|
| `docs/spec/syscalls.md` | syscall numbers, register ABI, error codes, all boundary struct byte layouts |
| `docs/spec/capabilities.md` | CapKind, CapRights bit values, grant/revoke/cascade semantics |
| `docs/spec/ipc.md` | ring layout, blocking model, capability transfer protocol, BootInfo format |
| `docs/spec/fhs.md` | filesystem hierarchy — canonical install paths, subvolume layout |

---

## Build — three domains, never one `cargo build`

```bash
make               # full build: userspace → rootfs/lth/bin/, kernel + disk.img
make oros          # userspace only (→ rootfs/lth/bin/)
make kernel        # kernel debug
make kernel-release
make run           # debug kernel under QEMU
make run-release
make debug         # QEMU with -d int,cpu_reset
make run-gui       # QEMU with graphical display
cargo build        # host tools only (orox-pack, sysroot-builder)
```

Bare `cargo build` at workspace root builds **host tools only** — the kernel and all userspace crates are excluded from `default-members`. The kernel requires `--target targets/x86_64-lythos.json` + `-Z build-std`; userspace requires `--target targets/x86_64-oros.json` + `-Z build-std`. The root `.cargo/config.toml` sets per-target rustflags only — there is **no global default build target**. Each domain's `per-subdir .cargo/config.toml` configures its own.

No xtask yet — the Cargo.toml comment `(xtask TBD)` is aspirational. Use `make`.

---

## Workspace layout

```
abi/lythos-abi/          — kernel↔user ABI (no logic; pure types and constants)
abi/lythos-syscall/      — userspace syscall instruction stubs (x86_64 only)
kernel/                  — Lythos microkernel (no_std bare metal)
userspace/
  init/lythd/            — PID-1 root server + service supervisor
  daemons/lythdist/      — capability distributor daemon
  daemons/lythmsg/       — IPC bus daemon
  shell/lysh/            — system shell
  apps/rkilo,rutils/     — editor, utilities
  lib/lythos-rt/         — userspace runtime + linker script (userspace.ld)
  lib/lythos-libstd/     — native stdlib wrappers
  webwm/                 — WebWM window manager (bridge/ excluded from workspace)
tools/
  orox-pack/             — package packer (host tool, in default-members)
  mkrfs/                 — disk image builder (excluded from workspace; run by kernel/build.rs)
  lythos-toolchain/      — sysroot builder, lythos-libc, lythos-unwind
targets/
  x86_64-lythos.json     — kernel custom target spec
  x86_64-oros.json       — userspace custom target spec
docs/spec/               — canonical ground truth (do not duplicate here)
docs/plans/              — deferred work tracking
```

---

## Key invariants

- **Capabilities are a kernel subsystem** (`kernel/src/cap.rs`). Enforcement never leaks to userspace. Userspace holds opaque handles; the kernel validates rights on every syscall.
- **lythd is PID-1 and the root server.** Critical daemons (`lythdist`, `lythmsg`) are spawned from it. It must not crash; keep it lean.
- **System binaries live in `/lth/bin/`** (per `docs/spec/fhs.md`). `/bin/` and `/sbin/` are POSIX compat symlinks populated by `lythd` at boot. Never use `/bin/` as a canonical install target.
- **Boundary struct layout changes need the size+offset asserts to pass** in `abi/lythos-abi/src/structs.rs`. A field change that satisfies the asserts but misaligns kernel serialization vs. userspace deserialization is a silent data corruption bug — both sides must change together.

---

## Load-bearing files — silent failures if orphaned

| File | What breaks |
|------|------------|
| `targets/x86_64-lythos.json` | kernel build entirely |
| `targets/x86_64-oros.json` | all userspace builds |
| `tools/lythos-toolchain/target-specs/x86_64-lythos-sysroot.json` | sysroot build |
| `kernel/boot/linker/x86_64.ld` | boot: wrong load address or missing sections |
| `userspace/lib/lythos-rt/userspace.ld` | all userspace binaries link incorrectly |
| `kernel/.cargo/config.toml` | kernel uses wrong build target or linker flags |
| `userspace/.cargo/config.toml` | userspace uses wrong build target |

These are not caught by `cargo check`. Moving or editing them produces errors that look like unrelated linker failures or wrong-format ELF output.

---

## Verification discipline

**Build green ≠ run correct.** Syscall number changes, struct field reordering, and error code value changes compile cleanly but run wrong. After any ABI, handler, or struct layout change:

1. `make run` — boot under QEMU
2. Inspect raw syscall return values, not just whether the binary ran
3. For struct layout changes, compare byte offsets against the layout tables in `docs/spec/syscalls.md`

---

## Deferred work

`docs/plans/followup-code-tasks.md` tracks all deferred items. **Critical landmine to know now:**

**Heap / `net::init` over-allocation:** `HEAP_INIT_PAGES = 4096` (16 MiB) in `kernel/src/heap.rs` is a workaround that masks a heap-exhaustion panic from `net::init()` over-allocating RX/TX buffers. The root cause is in `kernel/src/virtio_net.rs`. The current QEMU target always attaches a virtio-net device (`-device virtio-net-pci,netdev=net0`), so `net::init()` runs on every boot. **Do not reduce `HEAP_INIT_PAGES` without first fixing the over-allocation in `virtio_net.rs`.** Reducing it back toward 4 MiB will reproduce a kmain panic.
