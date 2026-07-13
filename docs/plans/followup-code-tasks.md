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

## 5. virtio-net — over-allocation. RESOLVED (2026-07-02)

**Status:** fixed, but the original diagnosis was wrong on both counts.

The virtio-net driver never allocated from the kernel heap — all its buffers
come from the PMM (~296 KiB: 64 RX buffers × one full 4 KiB frame each for a
1524-byte need, 2 × 4 ring pages, 2 TX pages). The heap-exhaustion panic that
prompted the 16 MiB `HEAP_INIT_PAGES` workaround was **free-list
fragmentation**: `dealloc` inserted at the list head with no coalescing, so
task-stack and syscall-buffer churn shattered the heap (measured: 507 free
blocks at idle) until large allocations failed despite ample free bytes.

**Fixes applied:**
- `kernel/src/heap.rs` — address-ordered free list with coalescing in
  `dealloc`; idle block count now 3–8. `HEAP_INIT_PAGES` reduced 4096 → 512
  (16 MiB → 2 MiB); idle heap use is ~250 KB.
- `kernel/src/virtio_net.rs` — RX pool packed at 2 KiB stride in one
  contiguous run (32 bufs × 2 KiB = 16 pages, was 64 × 4 KiB = 64 pages); TX
  header+payload share one page; ring sizes now honor the device's
  `REG_QUEUE_NUM`. The old code hardcoded queue size 64 while QEMU reports
  256 — avail-ring writes used `% 64`, the device reads `% 256`, so RX broke
  silently after the first 64 packets. Ring index math now uses the device
  size everywhere. (Sustained->64-packet RX not yet exercised under load —
  verify when a userspace net workload exists.)
- `kernel/src/elf.rs` — the actual big consumer: `USER_STACK_PAGES` was 2048
  (8 MiB **eagerly allocated per exec**, measured 2061 frames/exec). Reduced
  to 64 (256 KiB); exec now costs 73 frames.

**Measured:** idle RAM ~53 MiB (est., full service set) → ~4.4 MiB.
`[ram-idle]`/`[heap-stat]` diagnostics print periodically from the kmain idle
loop; `[sweep]`/`[sweep-user]` probes confirm 0 B heap and 0 frames PMM
leaked across spawn/reap and exec/reap cycles (the "sweep_dead stack leak"
note was stale — `sweep_dead` frees stack Vec, guard page, and user page
table since the repo merge).

**Follow-up round (same date) — userspace arena:** the remaining large
consumer was `lythos-rt/src/allocator.rs`: a **4 MiB static BSS arena in
every binary**, eagerly frame-backed by the ELF loader. Replaced with a
64 KiB static bootstrap arena + on-demand brk growth (256 KiB chunks) and
tail shrink back to the kernel (512 KiB threshold / 256 KiB slack
hysteresis). Kernel `SYS_BRK` now really frees pages on shrink
(`vmm::unmap_page_in` returns the frame; spec updated in
`docs/spec/syscalls.md`). SYS_BRK requires a Memory capability, so
`lysh.svc` now grants `cap=memory` and lysh forwards it to spawned apps via
`rutils::cmd_exec_with_caps` — a task without the cap is limited to the
64 KiB static arena. Measured per-task exec cost: lythd 1127 → 117 frames;
lythdist spawn Δ 95, lythmsg Δ 112 frames. Not yet exercised: rkilo editing
a file large enough to trigger repeated grow/shrink cycles.

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

---

## 7. RFS v1 write path: kernel #PF on multi-block file writes. RESOLVED (2026-07-03)

**Found and fixed same day** while verifying per-exec Memory-cap scoping.
Reproducer: `cp /lth/bin/rkilo /rkilo2` (a ~100 KiB copy through repeated
4 KiB `SYS_WRITE_FD` chunks) page-faulted the kernel partway through:

```
[#PF] task 35 (user)  faulting_va=0xffffc00007c07000  error=0x2  not-present write kernel
rip → compiler_builtins memset (512-byte fill, rsi=0x200)
```

**Root cause: kernel stack overflow into the guard page**, not a wild block
pointer. `rfs::read_block` returned `[u8; 4096]` by value; in debug builds
each call site cost up to three 4 KiB stack copies (return slot, `Option`
temporary, destination local). The write chain
`append_to_file → resolve_block → add_extent → read_block` stacks several
such frames; once a file grew past its 4 inline extents (16 KiB) the
overflow-extent path added enough depth to cross the 64 KiB kernel stack
into the unmapped guard page. The 512-byte memset was `read_block`'s
`sector` local landing on the guard page — which is also why small writes
(passwd → /etc/shadow) never hit it and why the fault appeared partway
through a 100 KiB copy.

**Fix:** `read_block` → `read_block_into(blk, &mut [u8; BLOCK_SIZE])` — all
16 call sites pass the buffer by reference; sectors are read directly into
the destination slice (no 512-byte temp either). Worst-case write-path stack
is now roughly one 4 KiB buffer per frame.

**Verified:** cp of the 100 KiB binary completes, and the copy execs as a
valid ELF (multi-block write correctness, not just fault-freeness).

---

## 8. Scheduler: flaky boot panic — index OOB in find_next_ready. RESOLVED (2026-07-03)

**Found and fixed same day**, seen once across ~6 boots (debug build,
Limine/OVMF):

```
[PANIC] task 0 (kmain)  panicked at kernel/src/task.rs:207:27:
index out of bounds: the len is 3 but the index is 3
```

**Root cause:** `yield_task` ran `find_next_ready` with interrupts enabled —
`sweep_dead` restores the caller's IF on exit, and the old `cli` came only
just before `switch_cr3`. A timer ISR in that window re-enters `yield_task`;
its `sweep_dead` removes reaped tasks from `sched.tasks` mid-scan, so the
outer scan indexes past the shrunken Vec (and its `n`/`current` snapshots go
stale). `task_exit` had the sibling bug: it marked the current task Dead
*before* disabling interrupts, so a timer ISR could reap the running task —
freeing the kernel stack the ISR itself was on.

**Fix (kernel/src/task.rs):** `irq_save_disable`/`irq_restore` helpers;
`yield_task` holds IF=0 from entry through `switch_context` (restoring on
early returns); `task_exit` does `cli` before setting `state = Dead`. The
read-only scanners callable with IF=1 (`for_each_task`, `for_each_task_ps`,
`task_exists`, `task_status_raw` — kmain idle-loop diagnostics and smoke
tests) hold interrupts off across their iteration too.

**Verified:** 14 consecutive boots with zero panics (prior rate ~1-in-6
would predict ~2), plus the full cap/brk/rkilo/cp functional pass which
churns exec/exit/wait scheduler paths.

---

## 9. Boot time: 61 s → 6.5 s (2026-07-03)

Measured under QEMU TCG (debug kernel, Limine/OVMF). Three changes:

- **virtio-blk multi-sector requests + spin-then-halt polling**
  (`kernel/src/virtio_blk.rs`): `submit()` transferred one 512-byte sector
  per request and immediately `sti;hlt`-waited per poll iteration — ~7 ms per
  sector, and a 100 KiB binary took 200+ interrupt waits. Now one request
  moves up to 8 sectors (rfs reads/writes whole 4 KiB blocks in one request)
  and completion is spin-polled (500k pause budget) before falling back to
  hlt. Measured 62 µs/block after the change.
- **Boot test suite feature-gated** (`boot-tests` cargo feature, off by
  default; `make kernel-tests` enables): userspace-entry/ELF/integration/
  sweep probes cost seconds per boot. Cheap init smoke tests
  (pmm/vmm/heap/sched/apic/cap/ipc) remain unconditional.
- Remaining ~6.5 s ≈ OVMF/USB firmware (~4 s) + kernel init + service spawn.
  The firmware share is QEMU-specific (usb-storage ESP enumeration); real
  hardware differs. Debug-build TCG emulation overstates the compute phases.

Run the gated suite (`make kernel-tests && make run`) before touching the
scheduler, PMM, heap, ELF loader, or cap system.

---

## 9. RFS V2 kernel wiring. LANDED (2026-07-06)

The `fs/rfs2` crate (COW filesystem, `docs/rfs-v2/`) is now the live backing
store for the FS syscalls, replacing the V1 driver (`kernel/src/rfs.rs`, kept
in-tree but no longer wired). Integration only — no on-disk format or FS logic
changed.

**What landed:**
- **virtio-blk `BlockDevice`** (`kernel/src/vfs.rs::VirtioDisk`): 4096-B blocks
  over the existing 8-sector virtio-blk requests. Driver gained
  `VIRTIO_BLK_F_FLUSH` negotiation + `VIRTIO_BLK_T_FLUSH` (`virtio_blk::flush`);
  boot confirms `flush=T_FLUSH` on QEMU 10.x. Without F_FLUSH the device is
  write-through and `flush()` is a correct no-op.
- **Mount at boot** (`vfs::init`, called from `main.rs`): `Rfs2::mount` runs
  slot selection + mark-and-sweep free-space reconstruction + orphan reclaim on
  the real volume. Boot log: `[vfs] rfs2 mounted: gen=N slot=A blocks=X/Y`.
- **Syscall switchover**: `SYS_OPEN/READ/WRITE/CLOSE/STAT/READDIR/CREATE/`
  `UNLINK/MKDIR/RENAME/SEEK` and exec's `load_file` route through `vfs.rs` to
  V2. Syscall surface unchanged; errno mapping preserves the V1 ABI values.
  lythd's `abi-verify` self-test passes against the mounted volume.
- **Image builder** `tools/mkrfs2` links `fs/rfs2` and formats via the
  reference `mkfs`; `kernel/build.rs` now calls it. `--verify` walks/reads the
  whole tree; `--crash-test` is the acceptance harness.
- **Crash-consistency gate PASS** (`mkrfs2 --crash-test disk.img`): a torn
  superblock write (only first sector lands) is rejected by `read_slot`
  (payload gen ≠ trailer gen_copy) and remount recovers to last-good gen K; a
  clean commit advances to K+1; a recovered volume stays writable. Pointer-flip
  atomicity holds on the real commit path.

**Flush/barrier verification (how):** the driver reads device feature bits,
accepts only F_FLUSH, and issues a 2-descriptor T_FLUSH request that blocks on
the device status byte — same completion path as read/write, so the barrier
provably reaches the device before `flush()` returns. Confirmed on boot via the
`flush=T_FLUSH` diagnostic. On a device that does not offer F_FLUSH the spec
guarantees write-through durability, so the no-op path is correct.

**TODO(open) surfaced during wiring:**
- **create/mkdir ownership**: `Rfs2::create`/`mkdir` (doc 07 §3, doc 06 §1)
  take no uid/gid and stamp 0/0 mode 0644/0755. The V1 driver recorded the
  caller's uid/gid. `vfs::create`/`mkdir` accept `uid`/`gid` for ABI
  compatibility but ignore them. Needs a spec-defined set-ownership operation
  (chown-equivalent) before multi-user DAC on V2 files is meaningful.
- ~~**ENOTEMPTY**~~ RESOLVED 2026-07-13 with item 10: sentinel `-16` in both
  errno tables; `vfs::errno_fs` no longer folds `NotEmpty` into `EINVAL`.
- ~~**EIO sentinel**~~ RESOLVED 2026-07-13 with item 10: sentinel `-17` in
  both errno tables; device/integrity faults (`Io`, `Auth`, `Corrupt`,
  `BadHeader`, `NoSuperblock`, `Unsupported`) now report `EIO`, distinct
  from `ENOMNT` ("no device / not mounted").
- **V1 driver retirement**: `kernel/src/rfs.rs` is dead once V2 is trusted;
  left in place this pass to keep the diff integration-only. Remove in a
  follow-up (also drops the mkrfs V1 tool and its build.rs references).
- **Crypto layer**: out of scope by design — `IdentityTransform` (no-op) is the
  active seam. Torn-write detection under identity rests on the plaintext
  `gen_copy` trailer check, not GCM auth; the real `GcmTransform` is a separate
  KAT-gated workstream (doc 08, `fs/rfs2/src/transform.rs`).

## 10. errno scheme collision: vfs-local codes shadow abi/errno.rs. RESOLVED (2026-07-13)

**Resolution:** `kernel/src/vfs.rs` now returns the canonical
`abi/lythos-abi/src/errno.rs` values. Three new sentinels added to both
tables and `docs/spec/syscalls.md`: `EISDIR=-15` (was vfs-local -7, which
aliased `EAGAIN`), `ENOTEMPTY=-16` (was folded into `EINVAL` — closes item
9's ENOTEMPTY TODO), `EIO=-17` for device/integrity faults (was vfs-local
`ENODEV=-1`, which aliased `ENOSYS` — closes item 9's EIO TODO; distinct
from `ENOMNT` "not mounted"). `ERR_MIN` moved to `EIO`. Userspace:
`lythos-rt::SysError` gained `Mounted/RoFs/IsDir/NotEmpty/Io` variants,
`from_raw`/`is_err_raw` now derive from `lythos_abi::errno` (the old
hardcoded `is_err_raw` boundary at -12 silently treated `EMOUNTED`/`EROFS`
as success values). Audit found no userspace callers decoding the old -1/-7
numerics. The retired V1 driver `kernel/src/rfs.rs` keeps its local table
(unwired; removed with item 9's V1-retirement task). Boot probe: the
exclusive-create probe (`make kernel-tests`) asserts `open(dir) == -15` live.
Original report follows.

`kernel/src/vfs.rs` defines local negative error codes carried over from the
retired V1 `rfs.rs` ABI: `ENODEV=-1, EINVAL=-4, ENOENT=-5, EBADF=-6, EISDIR=-7,
ENOTDIR=-8, ENOMNT=-9, EMFILE=-10, EEXIST=-11, ENOSPC=-12`. These **shadow**
`abi/lythos-abi/src/errno.rs`, where the same numeric values mean different
things (`ENOSYS=-1, ENOCAP=-2, ENOPERM=-3, EINVAL=-4, ENOENT=-5, EBADF=-6,
EAGAIN=-7, ENOTDIR=-8, ENOMNT=-9, EMFILE=-10, EEXIST=-11, ENOSPC=-12`). Where
the two tables agree (-4..-6, -8..-12) it is coincidence, not design; -1/-7
actively disagree (`ENODEV`/`EISDIR` vs `ENOSYS`/`EAGAIN`).

A userspace caller that interprets an FS syscall return through
`abi::errno` gets the wrong meaning for -1 and -7. Latent because current
callers mostly test `< 0` rather than decode the specific code.

**Deferred deliberately** (surfaced by the mount-syscall task,
`docs/plans/mount-syscall-shade-store.md` §6.1 — that task adds `EMOUNTED=-13`
and `EROFS=-14` to both tables cleanly but does **not** touch the collision).

**Fix (when scheduled):** make `vfs.rs` return the canonical `abi::errno`
values (fold `ENODEV`→a real `EIO` sentinel — see item 9's EIO note — and
`EISDIR`→its own sentinel), update `docs/spec/syscalls.md`'s error table, and
audit userspace FS callers for hardcoded numeric assumptions. Cross-cutting ABI
change; do with the EIO/ENOTEMPTY sentinel work above in one errno pass.

## 11. Kernel boot-loops with guest RAM > 1 GiB

Found 2026-07-12 during mount-hardening verification. `-m 512M` boots fully
green; `-m 1536M` enters the kernel and boot-loops (repeated
`lythos kernel initializing` banners = crash+reset before serial diagnostics);
`-m 2G` wedges even earlier, inside Limine/OVMF (`Loading executable` repaint
loop — kernel never entered, possibly an EDK2/Limine-11-on-nix-QEMU quirk).

The crash at 1536M happens in EARLY boot, long before VFS/RamDisk/mount code
runs — likely something (Limine response structures, memory-map walk, early
page-table bootstrap) touching physical addresses above the 1 GiB window that
`vmm::IDENTITY_MAP_LIMIT` covers, i.e. the same unmapped-frame bug class the
mount hardening fixed for RamDisk, but in the boot path. `pmm::MAX_FRAMES`
spans 4 GiB, so PMM init itself is a suspect (bitmap marking is address-only —
fine — but anything that *touches* high usable regions is not).

Not caused by the 2026-07-12 hardening: the vmm identity-map loop is
byte-identical in behavior (512 × 2 MiB, now expressed via
`IDENTITY_MAP_LIMIT` with a const assert), and the RamDisk validation code is
unreachable at the observed crash point.

**Fix (when scheduled):** boot under `-d int,cpu_reset` (`make debug`) with
`-m 1536M`, find the faulting access, and either extend the identity map /
HHDM alias beyond 1 GiB or gate the offending walk. Until then QEMU_MEM must
stay ≤ 1 GiB (default 512M).
