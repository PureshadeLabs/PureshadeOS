# Design: explicit mount syscall + dedicated `/shade/store` mount

Status: **LANDED, both stages (2026-07-12).** Host tests: `fs/vfs-core` 17/17.
QEMU (boot-tests): `[mount] stage-1 probe passed` + `[mount] stage-2 probe
passed` + lythd mounting `/shade/store` via SYS_MOUNT from ring 3, verified
over 4 consecutive boots to login. Implementation notes at the end (§8).

Original proposal follows. Blast radius spans the capability security
core (`kernel/src/cap.rs`), the kernel↔user ABI (`abi/lythos-abi`), and the
VFS shim (`kernel/src/vfs.rs`). Per `CLAUDE.md`, ABI and capability changes are
flagged before landing. This doc is the flag.

## 0. Why this doc exists — premises vs. reality

The task assumes infrastructure that is not in the tree. Corrected baseline:

| Task premise | Actual state (verified) |
|---|---|
| VFS has a generic FS-backend trait + mount table | `vfs.rs` holds **one** global `Vfs { fs: Rfs2<VirtioDisk, IdentityTransform>, fds }`. No trait, no table, no routing. |
| File syscalls enforce capabilities on the boundary | `SYS_OPEN/READ/WRITE/CREATE/RENAME/...` (`syscall.rs:1014+`) call `vfs::*` with **zero** cap checks. |
| A filesystem capability exists | `CapKind = { Memory, Ipc, Device, Rollback }`. No FS/mount kind. |
| `/shade/store` can get its own backend | One virtio-blk device. A second RFS V2 instance needs its own `BlockDevice`. |

So "add a mount syscall and use the mount table" expands to: build the mount
table + routing, add an FS/mount capability to the security core + ABI, add a
second backing device, then the syscall — plus read-only-after-realize in
stage 2.

## 1. Locked decisions (from review)

1. **Scope:** design doc first (this), then land **stage 1**, verify, then
   stage 2. Do not collapse.
2. **Mount authority:** new `CapKind::Filesystem`. Grown in `cap.rs` +
   `abi/lythos-abi/src/cap.rs`, granted to `lythd` at boot, required by
   `SYS_MOUNT`.
3. **`/shade/store` backing:** RAM-backed `BlockDevice` formatted with
   `rfs2::mkfs` at mount time. Fully separate from root; non-persistent across
   reboot (acceptable — the store is content-addressed and re-realizable).
4. **Verification:** factor a host-testable `vfs-core` crate (mount table,
   path routing, realize-guard) with unit tests like `fs/rfs2`'s 34; kernel
   keeps thin glue verified by boot probes.

## 2. Architecture: `fs/vfs-core` (new, host-testable)

Mirror the `fs/rfs2` factoring: a `no_std + alloc` crate in `default-members`,
host-tested, holding **all logic that is pure over an abstract backend**. The
kernel provides the concrete `Rfs2`-backed backend + cap checks + device glue.

```
fs/vfs-core/           # new crate, no_std+alloc, host tests
  src/lib.rs
  src/backend.rs       # FsBackend trait (object-safe) + Error passthrough
  src/mount.rs         # MountTable: insert, longest-prefix resolve, unmount
  src/realize.rs       # RealizeGuard: seal-on-rename, reject writes to sealed
kernel/src/vfs.rs      # glue: Rfs2 impls FsBackend; owns devices, fd table,
                       #   cap enforcement lives one layer up in syscall.rs
```

### 2.1 `FsBackend` trait (backend.rs)

Object-safe trait covering exactly what `vfs.rs` calls on `Rfs2` today, so a
`Box<dyn FsBackend>` can sit in the table regardless of `<D, T>`:

```rust
pub trait FsBackend {
    fn lookup(&mut self, path: &str) -> Result<u64, Error>;
    fn stat(&mut self, path: &str) -> Result<InodeMeta, Error>;   // slim, Copy
    fn readlink(&mut self, path: &str) -> Result<String, Error>;
    fn read_at(&mut self, ino: u64, off: u64, out: &mut [u8]) -> Result<usize, Error>;
    fn write_at(&mut self, ino: u64, off: u64, data: &[u8]) -> Result<(), Error>;
    fn create(&mut self, path: &str) -> Result<u64, Error>;
    fn mkdir(&mut self, path: &str) -> Result<u64, Error>;
    fn unlink(&mut self, path: &str) -> Result<(), Error>;
    fn rename(&mut self, old: &str, new: &str) -> Result<(), Error>;
    fn readdir(&mut self, path: &str) -> Result<Vec<DirEntryOut>, Error>;
    fn pin(&mut self, ino: u64) -> Result<(), Error>;
    fn unpin(&mut self, ino: u64) -> Result<(), Error>;
    fn commit(&mut self) -> Result<(), Error>;
    fn has_staged_changes(&self) -> bool;
    fn generation(&self) -> u64;
}
```

`Error` is re-exported from `rfs2` (or a `vfs-core` mirror the kernel maps).
`Rfs2<D, T>: FsBackend` is a thin forwarding impl in `vfs.rs`. Host tests use a
`MemBackend` (in-memory tree) implementing the same trait — this is what makes
routing and realize-guard testable off-kernel.

### 2.2 `MountTable` (mount.rs)

```rust
pub struct MountTable { mounts: Vec<Mount> }   // Mount { at: String, backend: Box<dyn FsBackend> }

impl MountTable {
    pub fn mount(&mut self, at: &str, backend: Box<dyn FsBackend>) -> Result<(), MountError>;
    pub fn resolve<'a>(&'a mut self, path: &str)
        -> Result<(&'a mut dyn FsBackend, String), MountError>;   // (backend, backend-relative path)
    pub fn unmount(&mut self, at: &str) -> Result<(), MountError>;
}
```

- **Routing = longest-prefix match** on normalized mount points. `/` (root) is
  the mount installed at boot; `/shade/store` is longer, wins for paths under
  it. Generic — N mounts, no hardcoded path check (constraint honored).
- `resolve` returns the backend plus the **backend-relative** path (mount
  prefix stripped, leading `/` preserved; `/shade/store/x-y-z` → root-of-store
  `/x-y-z`).
- `mount` errors: `AlreadyMounted` (a mount already at `at`), `NoSuchPath`
  (mount point's parent dir does not exist in the covering mount — checked via
  the covering backend's `stat`), `NotDir`.
- Symlink following stays in the VFS glue and runs **per hop against the
  resolved backend**; a symlink target that escapes the mount re-enters
  `resolve` at the VFS layer. Documented cap: `MAX_SYMLINK_HOPS` unchanged.

### 2.3 `RealizeGuard` (realize.rs) — stage 2 only

Pure tracker of sealed store entries, keyed by top-level store name
`<digest>-<name>-<version>`:

```rust
pub struct RealizeGuard { sealed: BTreeSet<String> }
impl RealizeGuard {
    pub fn is_sealed(&self, backend_rel_path: &str) -> bool;   // path under a sealed entry
    pub fn seal(&mut self, store_name: &str);                  // called on atomic rename into place
    pub fn check_write(&self, path: &str) -> Result<(), Error> // Err(ReadOnly) if sealed
}
```

Only attached to the `/shade/store` mount. Write/create/mkdir/unlink/rename
targeting a path whose first component is a sealed store name → `Error::ReadOnly`.
Temp paths (`.tmp-*`, not yet sealed) are writable; the final atomic rename
calls `seal(store_name)`. Re-realize: rename onto an existing sealed name is a
no-op success (already sealed, immutable) — mirrors `shade_store::realize`'s
`already_present` branch.

#### Concurrency: two writers realizing the same digest

Store paths are `<digest>-<name>-<version>`; two writers targeting the same
digest are, by input-addressing, producing byte-identical content. The
model must converge them to **one sealed object, no error, no double-write**:

1. **Temp writes are per-writer.** Each writer stages into its own distinct
   `.tmp-*` name (unique per writer — the realizer picks it, e.g. pid+counter,
   exactly as `shade_store::temp_sibling` does). Pre-seal temp writes never
   collide because the names differ.
2. **The atomic rename onto the final name is the sole commit point.** Sealing
   happens *only* on a successful rename of a temp onto `<store-name>`, never
   on temp writes.
3. **First rename wins and seals.** `rename(tmp_a, <name>)` succeeds atomically
   and `seal(<name>)` records it.
4. **Second rename onto an already-sealed name is a no-op success — not
   `EROFS`.** The guard special-cases *rename whose destination is a sealed
   store entry*: it is the idempotent-realize case (same digest ⇒ same content),
   so it returns `Ok(())` and the caller drops/cleans its now-redundant temp.
   This is distinct from a *write/create into* a sealed path, which is `EROFS`.

So the guard distinguishes two operations on a sealed name:
`rename(temp → sealed <name>)` = idempotent no-op `Ok`; any *mutation of the
contents under* `<name>` = `ReadOnly`. First writer's bytes are authoritative
(immutability); the loser's identical bytes are discarded. No lock is needed —
the kernel is single-threaded and each syscall runs to completion, but the
semantics are defined so the invariant holds regardless of interleave.

## 3. Stage 1 — the mount syscall

### 3.1 ABI (`abi/lythos-abi`)

- `syscall.rs`: add `SYS_MOUNT = 56`; bump `SYSCALL_MAX` 55→56. Args:
  - `a1 = at_ptr`, `a2 = at_len` (mount point path, UTF-8)
  - `a3 = source` (backend selector enum, u64; see §3.2)
  - `a4 = flags` (u64 options bitfield: `MOUNT_RDONLY`, `MOUNT_STORE`, …)
  - `a5 = cap_handle` (Filesystem capability)
  - Returns `0` or negative errno.
- `errno.rs`: add `EMOUNTED = -13` (already mounted / mount point busy),
  `EROFS = -14` (write to read-only / sealed path); update `ERR_MIN`. Update
  the `is_err` floor and the doc table in `docs/spec/syscalls.md`.
- `cap.rs` (ABI doc side): add `CapKind::Filesystem` to the documented enum +
  cross-check note. **Kind values are never register args** (per the file), so
  no numeric ABI — doc only.

> ABI edit checklist (`CLAUDE.md`): update `abi` first, then kernel handler,
> then userspace callers, then `make run`. Struct asserts unaffected (no new
> boundary struct — args are scalars/pointers).

### 3.2 Backend selector

`source` (a3) is a small closed enum, not a device path string, to keep the
first cut tractable:

- `0 = MOUNT_SRC_RFS2_RAM` — fresh RAM-backed RFS V2 (used by `/shade/store`).
- (future) `1 = MOUNT_SRC_BLKDEV` — an existing block device by id.

Keeps the table generic (any backend the kernel can construct) without yet
plumbing device-path parsing through the ABI.

### 3.3 Capability (`kernel/src/cap.rs`)

- Add `CapKind::Filesystem` and `KernelObject::Filesystem` (no payload, like
  `Rollback`).
- Boot: in `main.rs`, alongside the rollback cap, `create_object(
  KernelObject::Filesystem)` → `create_root_cap(&mut lythd, CapKind::Filesystem,
  CapRights::ALL, obj)`. Only lythd holds it; delegable via `SYS_CAP_GRANT` per
  normal rules.
- No change to grant/revoke/cascade logic — new kind rides existing machinery.

### 3.4 Syscall handler (`kernel/src/syscall.rs`)

`SYS_MOUNT` handler, **cap check on the boundary** (pattern from
`SYS_ROLLBACK:703`):

```rust
SYS_MOUNT => {
    let table = current_task_cap_table();
    if !table.has_kind_with_rights(CapKind::Filesystem, CapRights::WRITE) {
        return ENOPERM;                     // no ambient authority
    }
    let at = copy_user_path(a1, a2)?;       // EINVAL on bad ptr/utf8
    match vfs::mount(&at, a3 /*source*/, a4 /*flags*/) {
        Ok(()) => 0,
        Err(code) => code,                  // folded errno (§3.5)
    }
}
```

Enforcement stays in `syscall.rs` (constraint honored); `vfs::mount` never
sees a cap.

### 3.5 errno fold

| Condition | Code |
|---|---|
| caller lacks Filesystem cap | `ENOPERM` |
| stale/invalid cap handle | `ENOCAP` |
| mount point already mounted | `EMOUNTED` |
| mount point parent missing | `ENOENT` |
| mount point not a directory | `ENOTDIR` |
| bad path ptr / len / utf8 / unknown source | `EINVAL` |
| backend format/mount failure | `ENODEV` |

### 3.6 Stage-1 tests (host `vfs-core` + one boot probe)

Host (`fs/vfs-core`, `MemBackend`):
- `mount_registers_backend` — after `mount("/mnt", b)`, `resolve("/mnt/f")`
  returns `b` + `/f`.
- `resolution_crosses_boundary` — `/` and `/mnt` both mounted; `/a` → root,
  `/mnt/a` → mnt backend; longest-prefix correct.
- `double_mount_rejected` — second `mount("/mnt", _)` → `AlreadyMounted`.
- `mount_missing_parent_rejected` → `NoSuchPath`.
- `root_unaffected` — root lookups unchanged after mounting `/mnt`.

Kernel boot probe (behind `boot-tests`):
- mount a RAM RFS2 at `/mnt`, create+read a file through the syscall path,
  assert root FS generation/entries unchanged; assert `SYS_MOUNT` without the
  cap returns `ENOPERM`.

## 4. Stage 2 — dedicated `/shade/store` mount

### 4.1 RAM device + mkfs

`vfs.rs`: `struct RamDisk { blocks: Vec<[u8; 4096]> }` impl `rfs2::BlockDevice`
(flush = no-op, always durable in RAM). On the `MOUNT_SRC_RFS2_RAM` path:
`rfs2::mkfs(&mut dev, &IdentityTransform, &opts)` then `Rfs2::mount(dev, …)`,
box as `dyn FsBackend`, insert at `/shade/store`. Sizing: a fixed RAM budget
(e.g. 64 MiB / 16384 blocks) — noted as tunable.

> `AES-256-GCM via BlockTransform` is available but the store mount uses
> `IdentityTransform` to match root for the first cut; swapping in an encrypted
> transform is a backend-construction change only, no table/API impact.

### 4.2 Read-only-after-realize (mount-boundary enforcement)

The `/shade/store` mount carries a `RealizeGuard`. In `vfs.rs`, store-mount
write paths consult it **before** delegating to the backend:

- `write`/`create`/`mkdir`/`unlink` on a path under a sealed `<store-name>` →
  `EROFS`. Root mount has no guard → unaffected.
- Realize flow (driven by `shade-store` semantics): writes land under a temp
  name (`.tmp-*`, unsealed → allowed); the final `rename(temp, <store-name>)`
  succeeds atomically and then calls `guard.seal(<store-name>)`.
- Re-realize: `rename(temp, <store-name>)` when `<store-name>` already sealed →
  detect and return success without mutating (no-op), and drop the temp. Mirrors
  `already_present`.

Sealing granularity is the **top-level store entry** (first path component under
the mount), matching `/shade/store/<digest>-<name>-<version>` immutability.

### 4.3 Atomicity

`rename` within the store mount is one `Rfs2` staged transaction committed
atomically (existing `vfs::rename` → `fs.rename` + `commit`, doc 03 §4 pointer
flip). Realize depends on this; no change needed beyond routing the call to the
store backend. Cross-mount rename → `EINVAL` (renames stay within one backend).

### 4.4 Stage-2 tests

Host (`vfs-core` `RealizeGuard` + `MemBackend`):
- `realize_temp_then_seal` — write to `.tmp-x`, rename to `x-y-z`, then write to
  `x-y-z/...` → `ReadOnly`.
- `write_to_sealed_rejected`.
- `re_realize_is_noop` — second rename onto sealed name → Ok, contents unchanged.
- `unsealed_temp_writable`.

Kernel boot probe:
- mount store at `/shade/store` (distinct backend: assert different generation
  than root); realize a fake path (temp→rename); write to realized path →
  `EROFS`; `SYS_MOUNT`/store writes without Filesystem cap → `ENOPERM`; root FS
  generation unchanged throughout.

## 5. Constraints check

- **RFS V2's 34 tests**: untouched — `vfs-core` sits above `rfs2`; `rfs2`
  gains only a `FsBackend` impl (in the kernel crate, not `rfs2`), so its tests
  don't move.
- **Atomic-commit guarantees**: unchanged; realize routes to existing
  `rename+commit`.
- **Existing VFS/syscall tests**: root stays mounted at `/` with identical
  behavior; new codes are additive; `SYSCALL_MAX` bump is backward-compatible.
- **Mount table generic**: `Vec<Mount>` + longest-prefix, N backends.
- **Cap enforcement on the syscall boundary**: `SYS_MOUNT` and store writes
  gate in `syscall.rs`; `vfs`/`vfs-core` never inspect caps.

## 6. Open questions / risks

1. **Two errno schemes — pre-existing collision, DEFERRED (do not fold in).**
   `vfs.rs` uses local negative codes (`ENODEV=-1 … ENOSPC=-12`) that shadow
   `abi/errno.rs`'s different meanings (`ENOSYS=-1 …`). This is a real latent
   bug that predates this task. **Reconciling it is out of scope here** and is
   tracked as a follow-up (`docs/plans/followup-code-tasks.md`). This task only
   *adds* `EMOUNTED = -13` and `EROFS = -14` to **both** tables with matching
   values — cleanly, without inheriting or spreading the collision. Adding the
   two codes must not silently entangle with the shadowed range.
2. **RAM store is volatile.** Fine for content-addressed store, but a reboot
   empties `/shade/store`. If persistence is later required, switch backing to a
   disk region/second device — backend-construction change, table/API stable.
3. **`FsBackend` object-safety + `&mut`** across a boxed trait: fine, but the
   global `UnsafeCell<Option<Vfs>>` now holds boxed trait objects; the
   single-threaded-kernel `Sync` justification (vfs.rs module docs) still holds.
4. **Boot-probe reach.** Confirm `make kernel` (build-std) + `make run` (QEMU)
   are runnable in this environment before relying on boot probes; host
   `vfs-core` tests cover the logic regardless.

## 7. Landing order

1. `fs/vfs-core` crate + `MountTable`/`FsBackend`/`MemBackend` + host tests.
2. ABI: `SYS_MOUNT`, errno, `CapKind::Filesystem` (doc). 
3. `cap.rs`: `Filesystem` kind/object; `main.rs` boot grant to lythd.
4. `vfs.rs`: `Rfs2: FsBackend`, table replaces single global, `mount()`;
   `syscall.rs` `SYS_MOUNT` handler. Stage-1 boot probe. **Verify, stop.**
5. Stage 2: `RamDisk`, store mount, `RealizeGuard` wiring, stage-2 tests.
```

## 8. Implementation notes (post-landing, 2026-07-12)

Deviations / additions relative to the proposal above:

- **`MountId`** (stable per-mount u64) was added to `vfs-core`'s table; the
  kernel fd table records it so open fds keep addressing their backend.
- **Stale-fd seal enforcement:** a writable fd staged into a temp and held
  across the realize rename would have bypassed the path-level guard (writes
  are ino-based). Each fd records the top component of its backend-relative
  path; `vfs::rename` retargets those tops to the sealed store name when the
  realize rename commits, and `vfs::write` refuses fds whose top is sealed
  (EROFS). Covered by the stage-2 boot probe.
- **Re-realize cleanup:** on the idempotent no-op rename the kernel returns 0
  and leaves the loser's temp in place — the *caller* drops its redundant
  temp (temps are unsealed, so its unlink works). Matches the vfs-core
  host-test contract.
- **rfs2 `Error::ReadOnly`** now folds to `EROFS` (was `EINVAL` in the V1
  scheme) — strictly more precise; noted here as a deliberate errno change.
- **lythd** mounts the store at boot (`mount_shade_store()`), directly after
  ABI verification; failure is loud but non-fatal.
- **Launcher fixes** (found during verification): `run-limine.sh` was
  re-passing its positional OVMF args via `"$@"` as phantom QEMU disks
  (launch failure whenever the Makefile auto-detected OVMF), and QEMU's
  default 128 MiB is too small for Limine 11 to load the ~8 MB debug kernel
  ("PANIC: High memory allocator: Out of memory") — `QEMU_MEM ?= 512M` now.
- **Open observation:** one boot (first after a disk.img repack, anomalously
  slow) truncated at `[EXCEPTION] vec=` right after the sweep probes, before
  the mount probes. Not reproduced in 4 subsequent boots (all fully green).
  If it recurs, capture the full vector/RIP dump before touching anything —
  it predates no specific mount code path and may be timing-sensitive
  exec-test fallout.

Deferred (unchanged from §6): no unmount syscall; RAM store volatile by
design; RamDisk frames never returned to PMM; encrypted `BlockTransform` for
the store mount is a backend-construction change only.
