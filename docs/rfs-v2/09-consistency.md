# RFS V2 — Crash Consistency and Recovery (End to End)

This document assembles the guarantees from COW ([`04`](04-cow-and-commit.md)),
the dual superblock ([`03`](03-superblock.md)), mark-and-sweep
([`05`](05-space-management.md)), and authenticated encryption
([`08`](08-encryption.md)) into a single crash-consistency and recovery model:
what survives a crash at each stage, how mount recovers, and how the mechanisms
interact. RFS V2 has **no journal and no `fsck` repair pass** — recovery is
"mount the last committed generation," which is always well-defined.

---

## 1. The guarantee

> **CONSIST-1.** After any crash, RFS V2 mounts to the state of the last
> superblock commit that became fully durable. No intermediate, partial, or
> torn state is ever observed as live. There is no repair step; the last-good
> generation *is* the recovered filesystem.

This holds because the only way a change becomes visible is a superblock
commit, and a superblock commit is atomic ([§3](#3-stage-by-stage)).

---

## 2. The three mechanisms and what each contributes

| Mechanism | Contribution to consistency |
|-----------|-----------------------------|
| **COW** ([`04`](04-cow-and-commit.md)) | Live blocks are never overwritten, so an in-progress transaction cannot damage the committed tree. A crash mid-transaction leaves committed data pristine and the new blocks merely unreferenced. |
| **Dual superblock + generation flip** ([`03`](03-superblock.md)) | Provides the single atomic commit point. A torn superblock write invalidates one slot but never the other, so a valid prior generation always remains. |
| **Ordering + barriers** ([`04 §4`](04-cow-and-commit.md#4-commit-ordering)) | Children durable before parents; superblock durable last. Guarantees the committed tree is fully present the instant it becomes reachable. |
| **Authenticated encryption** ([`08`](08-encryption.md)) | Turns *torn* or *tampered* blocks from silent wrong-data into detected faults: a partial write fails its GCM tag. Makes "valid superblock" and "intact block" cryptographically checkable, not merely plausible. |
| **Mark-and-sweep** ([`05`](05-space-management.md)) | Reconstructs free space from the recovered tree, so there is no persistent allocation state to be left inconsistent by the crash. |

No single mechanism suffices; consistency is their conjunction.

---

## 3. Stage-by-stage: crash at each point of a commit {#3-stage-by-stage}

Consider committing generation `G = K+1` over current generation `K`. Stages
follow [`03 §4`](03-superblock.md#4-commit-the-pointer-flip-protocol) /
[`04 §4`](04-cow-and-commit.md#4-commit-ordering).

### Stage 0 — before any write

Nothing on disk changed. Mount → `K`. The uncommitted transaction is lost in
full. **No corruption.**

### Stage 1 — writing new tree blocks (data, block-map, inodes, inode-map)

New blocks are landing in *free* locations (never live blocks, per COW-1). No
superblock has changed; both slots still hold `{K, K-1}`.

- Crash here: mount → `K` (highest valid generation). The partially-written
  new blocks are unreferenced by any valid superblock → invisible.
- Some of those new blocks may themselves be **torn** (partial writes). That is
  irrelevant: nothing points to them with a matching tag, so they are never
  read; mark-and-sweep simply leaves them in the free set
  ([`05 §2`](05-space-management.md#2-mount-time-mark-and-sweep)). **No
  corruption; no leak beyond transient free-space until remount.**

### Stage 2 — barrier (flush of all stage-1 blocks)

The device is told to persist stage-1 blocks. Until this completes, the
superblock is not written (COW-3). Crash here is identical to Stage 1: mount →
`K`.

### Stage 3 — writing the new superblock into the inactive slot

The single commit point. The inactive slot (holding `K-1`) is overwritten with
`G`'s superblock.

- **Torn superblock write:** the slot's 4072-B payload and its plaintext
  trailer (`gen_copy` + tag) disagree → GCM authentication fails → the slot is
  **invalid**
  ([`03 §2`](03-superblock.md#2-superblock-structure)). The other slot's `K` is
  untouched. Mount → `K`. **Atomic: the commit either fully takes or fully does
  not.**
- **Complete superblock write, then crash before Stage 4's flush:** whether `G`
  is durable depends on the device. If the write reached the medium, mount →
  `G` (all its referenced blocks were made durable in Stage 2). If it did not,
  mount → `K`. Either way a *fully valid* generation is mounted — never a
  blend.

### Stage 4 — barrier (flush of the superblock)

After this completes, `G` is guaranteed durable. Crash here or later: mount →
`G`. **The transaction is committed.**

**Summary:** at every stage the mountable state is exactly one of `{K, G}`,
transitioning atomically at the moment `G`'s superblock becomes durable in
Stage 3.

---

## 4. Mount recovery procedure

1. **Read the static header** (block 0, plaintext). Validate `magic`,
   `format_version`, feature flags ([`10`](10-format-and-compat.md)).
2. **Derive the KEK** from the passphrase + stored Argon2id params; **unwrap
   the DEK** ([`08 §6`](08-encryption.md#6-key-hierarchy)). Failure → wrong
   passphrase or tampered header → abort.
3. **Read both superblock slots** (1, 2). For each: take `gen` from the
   plaintext trailer `gen_copy`, decrypt-and-verify under nonce `slot ‖ gen`
   ([`08 §3`](08-encryption.md#3-block-encryption)), and require the decrypted
   payload's `gen` to equal `gen_copy`. Discard any slot that fails
   authentication or the structural checks (`sb_magic`, `total_blocks`,
   `uuid`).
4. **Select the current superblock:** the valid slot with the higher `gen`
   ([`03 §3`](03-superblock.md#3-generation-numbers)). If neither is valid →
   mount fails (see [§6](#6-failure-modes)). Retain the other valid slot (if
   any) as the previous generation.
5. **Mark-and-sweep** from the current (and retained-previous) superblock(s) to
   build the in-memory free set
   ([`05 §2`](05-space-management.md#2-mount-time-mark-and-sweep)). A tag
   mismatch during the live traversal is a corruption fault, not an orphan →
   surface it ([§6](#6-failure-modes)).
6. **Reclaim orphaned inodes** (read-write mounts only). An inode with
   `USED = 1` and `nlink = 0` is an orphan: it was unlinked while pinned by an
   open handle and the pin did not outlive the session
   ([`06 §5`](06-inodes.md#5-inode-lifecycle)). Free all orphans (drop block
   maps, clear slots) in one transaction, committed immediately. Zero orphans
   → no transaction, no generation burned.
7. **Mounted.** No journal replay, no repair. The filesystem is the mounted
   generation's tree; free space is its complement.

There is deliberately no `fsck`: the on-disk invariants are either satisfied
(mount succeeds on the last committed generation) or a live block fails
authentication (a genuine media/tamper fault, which no structural repair could
safely paper over).

---

## 5. Interaction: COW + dual superblock + mark-sweep

The three interlock via the **two-live-trees** rule
([`04 §6`](04-cow-and-commit.md#6-freeing-and-the-two-live-trees)):

- COW writes `G`'s new blocks into free space **without freeing** `K-1`'s or
  `K`'s blocks, because `K` (and, until overwritten, `K-1`) must stay mountable
  as the crash fallback.
- The dual superblock is what *needs* the previous generation kept intact: it
  is the fallback that a torn `G` write reverts to.
- Mark-and-sweep therefore marks from **both** valid superblocks, so the
  allocator never reuses a block still needed by the fallback
  ([`05 §SPACE-1`](05-space-management.md#1-principle)). Only after `G+1`
  overwrites `K`'s slot do `K`'s uniquely-owned blocks become free.

This is why freeing is deferred and derived rather than eager and recorded:
eager freeing at commit time would race the very fallback the dual superblock
relies on; a persistent free structure would reintroduce the corruptible state
V2 exists to eliminate.

---

## 6. Failure modes and limits {#6-failure-modes}

- **Neither superblock valid.** Both slots fail authentication (double media
  fault, or a lying device that dropped both writes, or tampering). Mount
  fails; V2 does not fabricate a tree. `TODO(open):` an offline salvage tool
  that scans for the newest self-consistent inode-map root — recovery aid only,
  not part of normal mount.

- **A live block fails authentication after a good superblock is selected.**
  The committed tree references a block that no longer authenticates
  (bit-rot, media failure, tamper). That subtree is unreadable; operations
  touching it return an I/O error ([`08 §CRYPTO-2`](08-encryption.md#7-what-is-and-isnt-authenticated)).
  V2 **detects** rather than **repairs** — there is no redundancy in the
  baseline to reconstruct the lost block. `TODO(open):` optional metadata
  replication / data redundancy (feature flag) to make selected subtrees
  recoverable.

- **Device lies about flush/ordering.** COW-3's durability ordering
  ([`04 §5`](04-cow-and-commit.md#5-what-a-commit-makes-durable)) assumes the
  device persists on flush and does not reorder the superblock ahead of the
  data. A device that violates this can make `G`'s superblock durable while
  some stage-1 block is not — then a pointer in `G` references a block whose
  tag will not verify, detected as the previous bullet (an unreadable subtree),
  **not** as silent wrong data. So a lying device degrades to detected
  I/O errors, never to undetected corruption.

- **Whole-device rollback** across power cycles: not detectable without an
  external monotonic counter ([`08 §7`](08-encryption.md#7-what-is-and-isnt-authenticated),
  [`01 §3`](01-overview.md#3-threat-model)).

> **CONSIST-2.** Every consistency failure RFS V2 cannot prevent, it *detects*
> (authentication failure) rather than silently returning wrong data. The
> spectrum of outcomes is: correct data, or a reported error — never plausible
> wrong data.
