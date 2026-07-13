# Shade crates — on-target audit (real ABI vs host-stub)

**Date:** 2026-07-12. **Scope:** `pkg/shade-store`, `pkg/shade-build`,
`pkg/shade-store-db` (the `shade-gc` binary), `pkg/shade-gen`, `fs/vfs-core`.
**Method:** every filesystem, process, time, and store operation traced to its
backing. **No code changed;** workspace still builds green
(`cargo check -p shade-store -p shade-build -p shade-store-db -p shade-gen -p vfs-core -p shadec` → Finished).

## Headline

The four shade workstreams landed **host-seeded against the real `std`**, not
against any abstract backend. **None of them consume `vfs-core::FsBackend`.**
`vfs-core` is a separate island — clean, `no_std+alloc`, wired into the kernel
via `kernel/src/vfs.rs` — but no shade crate touches it. Every store, db, gc,
and generation filesystem operation is a **direct `std::fs` / `std::os::unix` /
`std::process` / `std::time` call**. The one crate with a proper on-target seam
is `shadec` (`EvalIo` trait, `no_std+alloc` core) — and it is out of this
audit's four, pulled in only as a dependency.

So the port is **not** "recompile for `x86_64-oros`". The OROS target builds
`core+alloc` only (`build-std=core,alloc,compiler_builtins`, no `std` sysroot —
`userspace/CLAUDE.md:3`). Every `use std::*` in these crates fails to resolve on
target. The work is to introduce a backend seam (as `shadec`/`vfs-core` already
have) and inject a syscall backend — a rewrite of the I/O layer, not a flag.

## Gap table (crate × concern)

Legend: **ABI-wired** = target-portable today · **host-stub** = direct host
`std`, no seam · **mixed** = seam exists for part, host-stub around it.

| Concern | vfs-core | shade-store | shade-store-db (gc) | shade-build | shade-gen |
|---|---|---|---|---|---|
| **1. FsBackend impls** | ABI-wired (trait + kernel `Rfs2` + test `MemBackend`) | host-stub (no seam; direct `std::fs`) | host-stub | host-stub | host-stub |
| **2. Sandbox / process** | n/a | n/a | n/a | **mixed** (proc behind `BuildSandbox`; fs around it) | host-stub (`std::fs`) |
| **3. Store realization** | ABI-wired guard (`RealizeGuard`, pure) | host-stub I/O; **hashing ABI-wired** | n/a | host-stub scaffolding | n/a |
| **4. DB / roots / gc I/O** | n/a | n/a | host-stub (records, scan, lock, symlink) | n/a (delegates via `DbRegistrar`) | n/a (delegates via `StoreDb`) |
| **5. Generations** | n/a | n/a | n/a | n/a | host-stub (temp+rename, symlink forest, activate) |
| **6. Builds for OROS target** | **yes** (`no_std+alloc`) | no (`std`) | no (`std`) | no (`std`) | no (`std`) |
| **7. Non-portable std surface** | none | `fs`, `path`, `process::id`, `os::unix::symlink` | + `time`, `thread::sleep`, `OpenOptions::create_new` | + `process::Command`, `sync::Mutex/Arc` | `fs`, `path`, `os::unix::symlink`, `time`, `process::id` |

## Per-concern detail

### 1. FsBackend impls — the seam exists only in vfs-core, and nobody uses it

`vfs-core::FsBackend` (`fs/vfs-core/src/backend.rs:75`) is object-safe, exactly
the VFS surface the kernel already calls on `Rfs2`, with its own `FsError` and a
`Copy` `InodeMeta` — clean, `no_std+alloc`, injectable. Concrete impls: the
kernel's `Rfs2`-backed one (`kernel/src/vfs.rs`) and a test `MemBackend`
(`fs/vfs-core/src/testutil.rs`, `#[cfg(test)]` only, not hard-wired anywhere in
production). The `MountTable` (longest-prefix routing) and `RealizeGuard`
(read-only-after-realize) are both pure over path strings and hold no backend.

**But**: `shade-store`, `shade-store-db`, and `shade-gen` do not depend on
`vfs-core` at all (verified in their `Cargo.toml`s), and `shade-build` depends
on it transitively for nothing. They call `std::fs::{rename, create_dir_all,
read_dir, File, remove_dir_all, copy, read, symlink_metadata}` directly. There
is **no trait boundary to inject a syscall backend into** — one must be added.

### 2. Sandbox (shade-build) — process is seamed, filesystem is not

- `BuildSandbox` (`pkg/shade-build/src/executor.rs:86`) is a clean seam for
  *how commands run*. Both impls spawn through it: `PermissiveSandbox`
  (`sh -c`, full host env, `std::process::Command`) and **`LythosSandbox`**
  (`pkg/shade-build/src/sandbox.rs`), which is **real**, not a
  `PermissiveSandbox` alias: it is a two-layer design — a pure `SandboxPlan`
  (mount list in `SYS_MOUNT` terms, `CapKind`+rights grant set, deterministic
  env; answers `check_read/write/network` in ABI errnos) plus a host *vehicle*
  (macOS Seatbelt via `/usr/bin/sandbox-exec`, fail-closed
  `LythosSandbox::new`). The `SandboxPlan` half is already ABI-shaped and fully
  unit-tested; only the vehicle is host.
- **The executor reaches around the sandbox for filesystem.** `build_node` /
  `build_in` (`executor.rs:450`, `:469`) call `fs::remove_dir_all`,
  `fs::create_dir_all(scratch/tmp)`, `fs::create_dir_all(staging)`,
  `fs::create_dir_all(log_root)`, `fs::File::create(log)` directly — the
  scratch/staging/log scaffolding never passes through `BuildSandbox`. So on
  target the seam covers process spawn and the sandbox's own mounts, but the
  executor's directory setup is still host-stub `std::fs`.

### 3. Store realization (shade-store) — writes to a host dir, not a mount

`pkg/shade-store/src/lib.rs` is entirely `std::fs`: `install` stages via
`copy_tree` (`fs::symlink_metadata`, `fs::create_dir_all`, `fs::copy`,
`std::os::unix::fs::symlink`), `fs::rename`s into place, `fsync_dir` via
`fs::File::open(dir).sync_all()`; `ensure_drv` writes the `.drv` with
`fs::File::create` + `sync_all` + `fs::rename`; temp names use
`std::process::id()`. It targets a `store_root: &Path` **host directory** — it
does not write through `FsBackend` to the `/shade/store` mount, and it does not
consult `vfs-core::RealizeGuard` (the guard is the kernel's job; the store crate
enforces immutability itself via the "exists ⇒ no-op" branch).

**Portable already:** the addressing. `Derivation`/`store_paths_at` compute the
digest via `shade_cdf::store_digest` (BLAKE3-160, pinned base32) — `no_std`,
pure, target-independent. `resolved_output_path_does_not_affect_digest` proves
`store_root` never feeds the hash. Only the *I/O* is host-stub.

### 4. DB / roots / gc (shade-store-db) — all direct std::fs + std::time

`pkg/shade-store-db/src/lib.rs`:

- **Records:** `write_atomic` (`fs::File::create` + `sync_all` + `fs::rename` +
  dir fsync), `read_refs`/`read_valid` via `fs::read_to_string`.
- **GC byte-scan:** `scan_tree` walks with `fs::symlink_metadata`,
  `fs::read_dir`, `fs::read_link`, and streams regular files through
  `fs::File::open` + `read_to_end` — direct host I/O, not `FsBackend::readdir` /
  `read_at`.
- **Roots:** `add_root`/`collect_roots` use `std::os::unix::fs::symlink`,
  `fs::read_link`, `fs::remove_file`.
- **Lock:** `acquire_lock` is `fs::OpenOptions::new().create_new(true)` spun on
  `std::thread::sleep(5ms)` against a `std::time::Instant` deadline. The module
  doc already flags this: the OROS VFS exclusive-create primitive is the
  eventual backing (`TODO(open)`, 02 §7.2). `std::thread` does not exist on the
  single-address-space builder task model.
- **Time:** `now_unix` via `std::time::SystemTime`; there is no clock syscall
  seam here.

`shade-build`'s `DbRegistrar` and `shade-gen` both reach the store db only
through `StoreDb`, so fixing this crate fixes their db path in one place.

### 5. Generations (shade-gen) — all direct std::fs + symlink + std::time

`pkg/shade-gen/src/lib.rs`: `GenLine::create` builds in a
`.tmp-gen-<n>-<pid>` sibling (`fs::create_dir_all`, `write_file_synced`,
`fsync_dir`) then `fs::rename`s into place; the profile forest
(`merge_into_profile`) is `fs::read_dir` + `std::os::unix::fs::symlink` with
collision detection via `fs::symlink_metadata`; `activate`/`wire_view` are
`symlink` + `fs::rename` + dir fsync; `boot_activate` reads the pointer via
`fs::read_to_string`; timestamps via `std::time::SystemTime`. Every path is
host-stub. `rfc3339_utc` is pure (portable). Root registration delegates to
`StoreDb::add_root` (concern 4).

### 6. Target build — the exact blocker

`shade-cdf`, `vfs-core`, and `shadec`'s core are `no_std+alloc` and build for
`x86_64-oros`. The four std crates **cannot**, for one reason:

> All programs are `no_std` static ELF64 binaries targeting `x86_64-oros.json`.
> No dynamic linker, no libc from the host toolchain.
> — `userspace/CLAUDE.md:3`

The OROS target is built with `-Z build-std=core,alloc,compiler_builtins`
(`userspace/CLAUDE.md:11`) — **there is no `std` sysroot**. `sysroot-builder`
(`tools/lythos-toolchain/sysroot-builder`) produces the lythos sysroot for
`lythos-libc`/`lythos-unwind`, not a Rust `std`. OROS's stdlib is
**`lythos-libstd`** — a *separate crate* of native wrappers (`fs`, `io`, `net`,
`process`, `sync`, `time` — `userspace/CLAUDE.md:26`), **not** the standard
`std` these crates import. So `use std::fs` does not resolve on target and there
is no drop-in that makes it resolve. The blocker is structural, not a missing
flag.

### 7. Remaining non-portable std surface

Enumerated across the four crates (none of which exist as-is on OROS):

- `std::fs` — all four (the bulk of the work).
- `std::path::{Path, PathBuf}` — all four; assume host path/UTF-8 semantics
  (`to_string_lossy`, `file_name`, `strip_prefix`).
- `std::process::id()` — temp-name uniqueness in every crate.
- `std::process::Command` — `shade-build` phase spawn (both sandboxes).
- `std::process::{exit, ExitCode}` + `std::env::args` — every `bin/`.
- `std::os::unix::fs::symlink` — store, db, gen (all guard `#[cfg(not(unix))]`
  with `Unsupported`).
- `std::time::{SystemTime, Instant, Duration}` — db (timestamps + lock
  deadline), gen (timestamps). No clock-syscall seam.
- `std::thread::sleep` — db lock spin.
- `std::sync::{Mutex, Arc, atomic}` — `Arc` (build eval), `Mutex`
  (`LythosSandbox` plan stash), `AtomicU64` counters.
- `std::env::current_dir` — `shade-gen/src/prism.rs:48`.

## Port plan (ordered by dependency)

The dependency spine is `shade-cdf → shade-store → shade-store-db →
shade-build → shade-gen`, with `vfs-core` as the shared fs abstraction and
`shadec` (via `EvalIo`) as the eval-input seam.

0. **Decide the seam shape (blocks everything).** Two options:
   (a) thread `&mut dyn vfs-core::FsBackend` (+ a small `Clock`/`Spawn`/`Symlink`
   trait) through each crate's public API, mirroring `shadec::EvalIo`; or
   (b) make the crates import `lythos-libstd` under a `std`-shaped facade and
   `#[cfg]`-swap `std` ↔ `lythos-libstd`. (a) is the pattern the codebase
   already commits to (`EvalIo`, `FsBackend`, `BuildSandbox`, `StoreRegistrar`);
   recommended.
1. **shade-store:** replace `install`/`copy_tree`/`ensure_drv`/`fsync_dir`/
   `temp_sibling` with `FsBackend` calls. Addressing already portable — untouched.
2. **shade-store-db:** route `write_atomic`, `read_*`, `scan_tree`, roots, and
   `entry_size` through `FsBackend`; replace `acquire_lock` with the OROS VFS
   exclusive-create primitive (retire `std::thread::sleep`); add a `Clock` seam
   for `now_unix`.
3. **shade-build:** (a) route the executor's scratch/staging/log setup through
   `FsBackend` (close the reach-around from concern 2); (b) add the real OROS
   `BuildSandbox` impl that lowers `SandboxPlan` to `SYS_MOUNT` + a
   capability-restricted builder task (the plan is already ABI-shaped —
   reuse it). `PermissiveSandbox` stays host-only.
4. **shade-gen:** route `create`/`activate`/`wire_view`/`boot_activate`/pointer
   I/O and the symlink forest through `FsBackend` + `Clock`.
5. **shadec OROS `EvalIo`:** implement the `lythos-libstd` VFS `EvalIo` (the
   trait and `HostIo` split already exist — `pkg/shadec/src/io.rs:53`). Blocked
   on argv plumbing through the ABI (`TODO(open)`, and `pkg/shade` stub).
6. **bins:** argv (`std::env::args`) and exit codes are the last mile, shared
   with the deferred OROS `shade` binary; all four host `bin/`s stay as the seed
   vehicle.

## Verdict — port size per crate

- **vfs-core** — *already on-target.* No port; it is the abstraction the others
  should have been built on.
- **shade-store** — *substantial rework of I/O, thin core.* Hashing/addressing
  ports for free; the entire realize/`.drv` write path is direct `std::fs` with
  no seam and must be rebuilt on `FsBackend`.
- **shade-store-db** — *substantial rework.* Records, GC scan, roots, and time
  are all host-stub; the lock additionally needs a new OROS VFS primitive
  (already flagged `TODO(open)`).
- **shade-build** — *mixed: thin on process, substantial on filesystem.* The
  `BuildSandbox` seam and the ABI-shaped `SandboxPlan` are the port's biggest
  head start (process spawn is already abstracted and the mount/cap model is
  written in ABI terms); the executor's own scratch/log filesystem scaffolding
  still reaches around the seam and needs an `FsBackend` port.
- **shade-gen** — *substantial rework.* Every generation/profile/pointer
  operation is direct `std::fs`+symlink+time with no seam.

One-line summary: **`vfs-core` is ready and `shadec` is seamed; the four store
crates are green only against host `std` and need an `FsBackend`/clock seam
introduced and every `std::fs` call rebuilt on it — thin only where addressing
(`shade-cdf`) or the `SandboxPlan`/`BuildSandbox` model already carry the ABI.**
