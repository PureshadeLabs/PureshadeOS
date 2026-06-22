# Follow-up code tasks

Tracked here: kernel changes needed to match the spec contract that were deferred
from the spec-thickening pass. Each item is spec-first (spec defines the contract;
kernel must be updated to match). Each item should be its own commit, verified
green before merging.

---

## 1. ~~SYS_STAT: reorder Stat serialization to canonical layout~~ DONE

**Why deferred:** The canonical Stat layout in `docs/spec/syscalls.md` uses
natural-alignment field ordering (large to small: size, mtime, ctime, flags,
uid, gid, nlink, mode, pad). The current kernel handler in
`kernel/src/syscall.rs` serializes in the old unaligned order
(`size, flags, mode, uid, gid, nlink, mtime, ctime`) which was incorrect.

**What to change:**
- In `kernel/src/syscall.rs`, the `SYS_STAT` handler: rewrite the `buf[..]`
  serialization block to write fields in canonical order:
  ```
  [0..8]   = stat.size
  [8..16]  = stat.mtime
  [16..24] = stat.ctime
  [24..28] = stat.flags
  [28..32] = stat.uid
  [32..36] = stat.gid
  [36..40] = stat.nlink
  [40..42] = stat.mode
  [42..48] = zeroed pad
  ```
- Update the comment above the block to match.
- Validate that the buffer length check (`valid_user_range(frame.a3, 48)`)
  still holds (it does — total remains 48 bytes).

**Audit:** Any existing userspace code that reads `Stat` fields by hardcoded
byte offsets must be updated to the new offsets. Grep for `stat_ptr`, `SYS_STAT`,
and `[0..8]`/`[8..12]`/etc. patterns in userspace crates.

**Commit scope:** `kernel/src/syscall.rs` + any userspace callers.

---

## 2. ~~SYS_TASK_STATUS: return canonical task state encoding~~ DONE

**Why deferred:** The spec defines a canonical 4-value task state encoding:
0=dead, 1=running, 2=ready, 3=blocked. `SYS_TASK_LIST` (17) and `SYS_PS` (37)
already use this encoding. `SYS_TASK_STATUS` (16) currently returns a
non-canonical 3-value form: 0=dead, 1=running-OR-ready (conflated), 2=blocked.

**What to change:**
- In `kernel/src/syscall.rs`, the `SYS_TASK_STATUS` handler: map each task
  state to the canonical value:
  - `TaskState::Dead` (or not found) → 0
  - `TaskState::Running` → 1
  - `TaskState::Ready` → 2
  - `TaskState::Blocked` → 3
- Remove the conflation of Running and Ready.

**Audit:** Every caller of `SYS_TASK_STATUS` in userspace currently treats
return value 2 as "blocked". After this change, blocked is 3 and 2 means ready.
Grep all userspace crates for `SYS_TASK_STATUS` / syscall number `16` and
update comparison logic.

**Commit scope:** `kernel/src/syscall.rs` + all userspace callers.

---

## 3. mtime/ctime: populate timestamps at file creation and modification

**Why deferred:** The Stat layout specifies `mtime` and `ctime` in milliseconds
since kernel boot (same epoch as `SYS_TIME`). The kernel currently always
writes 0 for both fields at inode creation (`mtime: 0, ctime: 0` in `rfs.rs`
`SYS_CREATE` and `SYS_MKDIR` handlers) and never updates them on write.

**What to change:**
- In `kernel/src/rfs.rs`, at every point where an inode is created or last
  modified, set `mtime` and `ctime` using the kernel's `apic::time_ms()` (or
  equivalent millisecond-since-boot source).
- Ensure the values written to disk (`serialize_inode`) and read back
  (`parse_inode`) preserve them correctly (they already do — the field offsets
  in the on-disk format are correct).

**Note:** This does not affect the ABI layout (Stat offsets are fixed). It only
affects whether the fields contain meaningful values.

**Commit scope:** `kernel/src/rfs.rs` (creation and mutation sites).

---

## 4. lythos-syscall: add aarch64 syscall stubs when second arch is built out

**Why deferred:** `lythos-syscall` stubs are x86_64-only, guarded by
`#[cfg(target_arch = "x86_64")]` in `abi/lythos-syscall/src/lib.rs`. On any
other target the crate compiles as an empty module, and any userspace crate
that depends on `lythos-syscall` will fail to link (functions not found).

**What to change:**
- Add `abi/lythos-syscall/src/aarch64.rs` (or equivalent) with the aarch64
  `svc #0` instruction stubs using the same `syscall0`–`syscall6` signatures.
- Gate with `#[cfg(target_arch = "aarch64")]` in `lib.rs`.
- Add the aarch64 target JSON and verify the new stubs compile clean.

**Relevant files:**
- `abi/lythos-syscall/src/lib.rs` — add the `#[cfg(target_arch = "aarch64")]`
  module declaration alongside the existing x86_64 one.
- `abi/lythos-syscall/src/aarch64.rs` — new file, mirrors x86_64.rs structure.

**Commit scope:** `abi/lythos-syscall/` + the aarch64 target JSON.

---

## 5. virtio-net — over-allocation latent behind device probe, NOT fixed

**Status:** heap expansion was a workaround, not a fix. Root cause remains.

`net::init()` over-allocates the kernel heap (oversized RX/TX buffers and/or
kernel task stack), causing a kmain heap-exhaustion panic before lythd spawns.
`net::init()` is gated inside `if virtio_net::init()` — it runs only when a
virtio-net device is present.

**Current workaround:** `HEAP_INIT_PAGES` was expanded from 1024 (4 MiB) to
4096 (16 MiB) in `kernel/src/heap.rs`. This masks the panic but does not fix
the over-allocation. The over-sized buffers are still present; the larger heap
absorbs them.

**Trigger:** booting with a virtio-net device runs `net::init()`. The current
QEMU run target (`make run`) includes `-device virtio-net-pci,netdev=net0`, so
`net::init()` already runs every boot. If `HEAP_INIT_PAGES` is ever reduced
back toward 4 MiB the panic will reappear.

**Fix:** determine actual buffer-count × buffer-size in `net::init()`, right-size
RX/TX buffers and the net task ring, then verify clean init with heap at 4 MiB
to confirm the root cause is resolved (not just masked).

**Location:** `kernel/src/main.rs:210` — `net::init()` call, guarded by
`if virtio_net::init()` at line 204.

---

## 6. xtask: replace Makefile with cargo xtask

**Why deferred:** `Cargo.toml` workspace comment marks `cargo xtask build-kernel` /
`cargo xtask build-userspace` as TBD. The current build system is the top-level `Makefile`.
`make` works but is not idiomatic for a Cargo workspace and forces contributors to have `make`
available.

**What to add:**
- `tools/xtask/` crate added to workspace `members` (but not `default-members`)
- `xtask build-kernel [--release]` — wraps `cargo +nightly build --target
  targets/x86_64-lythos.json -Z build-std=... -p lythos`
- `xtask build-userspace [--release]` — wraps the corresponding oros build + rootfs copy
- `xtask run [--release] [--gui] [--debug-ints]` — invokes QEMU with the correct flags
- `xtask clean` — `cargo clean` + `rm -f disk.img`

**When to do it:** after the kernel and userspace builds are stable. Avoid during active ABI
churn — the Makefile is simpler to patch.

**Relevant files:** `Cargo.toml` (add member), `Makefile` (can be removed or kept as thin
wrapper once xtask is complete).
