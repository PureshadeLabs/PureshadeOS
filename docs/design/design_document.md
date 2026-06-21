# OROS — Open Runtime Operating System
**System Design Document** — Revision 2.0

---

## Overview

OROS (Open Runtime Operating System) is a capability-secure microkernel operating system built on the Lythos (Capability-Aware System Kernel) kernel, written in Rust. It uses a Btrfs subvolume-based filesystem for atomic updates and rollbacks, musl libc for broad POSIX compatibility, and a unified tooling ecosystem split between system daemons (`lyth-` prefix) and user-facing tools (`rp-` prefix).

---

## 1. The Core: Lythos Microkernel

Lythos (Capability-Aware System Kernel) is a minimalist, high-performance microkernel. All drivers and services run in userspace. The kernel exposes a minimal syscall surface across four categories: memory management, IPC primitives, capability operations, and scheduling.

### Architecture

| Property | Value |
|---|---|
| Type | Microkernel — drivers and services run in userspace |
| Security Model | Capability-based — unforgeable tokens grant access to hardware and IPC regions |
| Language | Rust — memory safety enforced at the kernel level |
| IPC Primitive | Shared memory regions with async message passing, allocated by `lythdist` |

### Core Daemons

Three daemons form the critical set. A failure in any of these during the 30-second stability window triggers an automatic rollback.

- **`lythd`** — PID 1. Supervises all services, manages the boot sequence and stability timer, and coordinates rollbacks on critical failure.
- **`lythdist`** — Capability distributor. Reads hardware topology at boot, allocates unforgeable capability tokens, and grants access to shared memory regions used by `lythmsg`.
- **`lythmsg`** — IPC bus. Async, shared-memory message passing for all inter-process communication. Built on top of `lythdist`-granted memory regions.

---

## 2. Boot Sequence

The boot sequence is strictly ordered. `lythmsg` does not become available until `lythdist` has distributed capabilities, and non-critical services do not start until the core is healthy.

| Step | Actor | Action |
|---|---|---|
| 1 | Lythos kernel | Initializes, mounts `/lth/system` (immutable, read-only) |
| 2 | `lythd` | Starts as PID 1 |
| 3 | `lythd` → `lythdist` | `lythd` spawns `lythdist`; `lythdist` reads hardware topology and allocates capability tokens |
| 4 | `lythdist` → `lythd` | `lythdist` grants `lythd` its capabilities and opens for service requests |
| 5 | `lythd` → `lythmsg` | `lythd` spawns `lythmsg` using its granted shared memory region |
| 6 | `lythd` | Checks for pending rollback flag set by `rpkg` on last update. If present, starts 30-second stability timer |
| 7 | `lythd` | Mounts remaining subvolumes: `/lth/store`, `/cfg`, `/user` |
| 8 | `lythd` | Reads service definitions, spawns non-critical services in dependency order |
| 9 | `lythd` | If stability timer is active and a critical daemon fails: atomically reverts `/lth/system` and `/cfg` snapshots, reboots |
| 10 | `lythd` | If timer expires cleanly: marks snapshot stable, clears rollback flag |
| 11 | — | System is live. User sessions spawn `lysh` on demand |

---

## 3. Filesystem: Btrfs Subvolume Layout

OROS uses a Btrfs subvolume-based structure. The `/lth/` namespace is system-owned and managed exclusively by `rpkg` and the kernel. Root-level paths are mutable and user-adjacent. Symlinks in `/lth/bin` are managed by `rpkg` and point to active versions in `/lth/store`.

| Path | Subvolume | Writable | Description |
|---|---|---|---|
| `/lth/system` | `@core` | No | Immutable. Lythos kernel and `lythd`. |
| `/lth/store` | `@store` | No* | Read-only store for compiled binaries. Functions like `/nix/store` — content-addressed, never mutated in place. (*`rpkg` mounts rw transiently during installs.) |
| `/lth/bin` | N/A | No | Symlinks managed by `rpkg` pointing to active versions in `/lth/store`. |
| `/cfg` | `@cfg` | Yes | System configuration. Snapshotted atomically with `/lth/system` before every `rpkg` update. |
| `/user` | `@home` | Yes | Persistent user data. |

---

## 4. Tooling Stack

Tools are split into two tiers by function. The `lyth-` prefix denotes kernel-adjacent daemons. The `rp-` prefix denotes user-facing OS tools.

### System Daemons (lyth-)

- **`lythd`** — Init process and service supervisor. PID 1. Manages boot, stability timer, and rollback coordination.
- **`lythdist`** — Hardware capability distributor. Allocates unforgeable tokens at boot, grants access to IPC memory regions.
- **`lythmsg`** — IPC bus. Async shared-memory message passing. Transport is shared memory; discovery is token-based via `lythdist`; if `lythmsg` itself crashes, it is in the critical set and triggers rollback.

### User-Facing Tools (rp-)

- **`rpkg`** — Source-based package manager wrapping Cargo. Handles atomic installs, binary caching, config snapshots, and rollback flag management.
- **`rpbreak`** — Chaos engineering tool. Manually triggers service failures to test system recovery.
- **`rpview`** — TUI displaying the live service hierarchy and `lythmsg` bus health.

### Shell

- **`lysh`** — Native system shell. POSIX-compliant. Launched as a user session process; not part of the boot sequence.

### Service Definitions

`lythd` reads service definitions at boot. Each service is defined in TOML. The `critical` field marks a service as part of the rollback-triggering set.

```toml
[service]
name     = "lynet"
bin      = "/lth/bin/lynet"
critical = false
deps     = ["lythmsg"]
```

---

## 5. Package Manager: rpkg

`rpkg` is source-first. It always builds from Cargo. The local binary cache skips recompilation when the same content hash already exists in `/lth/store`. Btrfs reflinks deduplicate identical build outputs without copying.

### Install Flow

| Step | Action |
|---|---|
| 1 | Snapshot `/lth/system` and `/cfg` atomically as a pair |
| 2 | Set rollback flag (triggers 30-second stability timer on next boot) |
| 3 | Resolve dependencies via Cargo. For C deps without a `-sys` crate, process `[system-deps]` block first: build, install to `/lth/store`, inject paths into Cargo environment |
| 4 | Check `/lth/store` for a matching content hash. If found, reflink — skip build |
| 5 | If no cache hit: build via Cargo, install result to `/lth/store` |
| 6 | Update `/lth/bin` symlinks atomically to point to new store entries |
| 7 | On successful boot past stability window: mark snapshot stable, clear rollback flag |

### Dependency Resolution

`rpkg` resolves dependencies in two tiers, reached in order:

1. Cargo handles everything natively via `build.rs` and `-sys` crates. Covers the majority of cases.
2. If a C dependency has no `-sys` crate, the package definition declares a `[system-deps]` block. `rpkg` builds those dependencies first, installs them to `/lth/store`, and injects their paths into Cargo's environment (`PKG_CONFIG_PATH`, `LIBRARY_PATH`, etc.) before proceeding with the main build.

---

## 6. Standard Library & Runtime

OROS uses a hybrid ABI model. Native OROS programs talk directly to the Lythos syscall interface. An optional compatibility server (`lythos-linux-compat`) provides Linux syscall translation for ported software, without any Linux-specific concerns entering the kernel.

### Native ABI (default)

Native OROS programs link against `lythos-std`, a minimal runtime that calls Lythos syscalls directly. No translation layer, no Linux assumptions.

| Property | Value |
|---|---|
| Runtime | `lythos-std` — thin layer over native Lythos syscalls |
| Rust std | Custom `std` implementation targeting the native Lythos ABI |
| Syscall interface | Lythos native — four categories: memory, IPC, capability ops, scheduling |
| Linking | Static by default. `rpkg` reflinks deduplicate identical copies in `/lth/store` |

### Linux Compatibility Layer (optional)

`lythos-linux-compat` is an optional userspace server that translates Linux syscall numbers and semantics to their Lythos equivalents. It uses musl libc internally as its POSIX implementation. Software that requires it declares a dependency on `lythos-linux-compat` in its service definition; native OROS software has no dependency on it and pays no overhead.

| Property | Value |
|---|---|
| Server | `lythos-linux-compat` — userspace, optional, OROS only |
| libc | musl — statically linked inside `lythos-linux-compat` only |
| Rust std (ported) | `std` via `x86_64-unknown-linux-musl`, routed through the compat server |
| Linking | Static. Ported binaries link musl; native binaries do not |

---

## 7. Resilience & Observability

### Fault Isolation

Drivers and non-critical services run as isolated userspace processes. A crash in `lynet`, `lygpu`, or any non-critical service does not affect the kernel or critical daemons. `lythd` detects the crash via its supervision loop and restarts the service.

### Automatic Rollback

`rpkg` snapshots `/lth/system` and `/cfg` as an atomic pair before every update and sets a rollback flag. On the first boot after an update, `lythd` starts a 30-second stability timer. If any critical daemon (`lythd`, `lythdist`, `lythmsg`) fails before the timer expires, `lythd` triggers an atomic Btrfs rollback of both snapshots and reboots. If the timer expires cleanly, the snapshot is marked stable and the flag is cleared. Config always rolls back with the system — they are never out of sync.

### Observability Tools

- **`rpview`** — Live TUI showing the service dependency graph and `lythmsg` bus health. Read-only.
- **`rpbreak`** — Chaos engineering tool. Manually kills services to test supervision, restart behavior, and rollback triggers.

---

## Summary

OROS (Open Runtime Operating System) is a Lythos (Capability-Aware System Kernel) microkernel OS with a Btrfs subvolume filesystem, musl libc, and a Rust-native toolchain. The critical triad of `lythd`, `lythdist`, and `lythmsg` provides a minimal, fault-isolated foundation. `rpkg` manages the full software lifecycle — source builds, binary caching, atomic installs, and coordinated rollbacks. The `/lth/` namespace is system-owned; `/cfg` and `/user` are user-adjacent and mutable. All system tooling is split cleanly between `lyth-` daemons and `rp-` user tools, with `lysh` as the POSIX-compliant system shell.
