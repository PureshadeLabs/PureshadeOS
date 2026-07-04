# RFS V2 — Overview, Goals, Threat Model, V1 Post-Mortem

RFS V2 is the second-generation native filesystem for PureshadeOS (Lythos
microkernel). It is a **copy-on-write (COW)**, **authenticated-encrypted**
filesystem designed from scratch after RFS V1 was found unretrofittable
(see [§4](#4-v1-post-mortem)).

This document set is the design specification. It is written to be
implementation-ready: on-disk structures are given as byte-offset tables,
invariants are stated explicitly, and every shared structure is defined once
and cross-referenced thereafter.

| Doc | Subject |
|-----|---------|
| `01-overview.md` (this) | goals, non-goals, threat model, V1 post-mortem, glossary |
| [`02-on-disk-layout.md`](02-on-disk-layout.md) | device layout, static header, `BlockPtr`, region map |
| [`03-superblock.md`](03-superblock.md) | dual superblock, generations, commit pointer-flip |
| [`04-cow-and-commit.md`](04-cow-and-commit.md) | COW write path, transactions, commit ordering |
| [`05-space-management.md`](05-space-management.md) | mount-time mark-and-sweep, allocation |
| [`06-inodes.md`](06-inodes.md) | 128-byte inode, block-map tree, inode-map radix tree |
| [`07-directories.md`](07-directories.md) | ext2-style dirents, directory operations |
| [`08-encryption.md`](08-encryption.md) | AES-256-GCM, self-validating pointers, KDF, key hierarchy |
| [`09-consistency.md`](09-consistency.md) | end-to-end crash-consistency and recovery |
| [`10-format-and-compat.md`](10-format-and-compat.md) | magic, version, feature flags, mkfs, compat policy |

---

## 1. Goals

1. **Crash consistency without a journal.** After any crash — including a
   torn write mid-commit — the filesystem mounts to the last fully-committed
   state. No `fsck` repair pass, no journal replay. Guaranteed by COW plus an
   atomic superblock generation flip ([`03`](03-superblock.md),
   [`09`](09-consistency.md)).

2. **Copy-on-write throughout.** No in-place mutation of any block that is
   live in the committed tree. Every modification allocates fresh blocks and
   is made visible only by a single atomic superblock commit
   ([`04`](04-cow-and-commit.md)).

3. **Authenticated encryption at rest.** All data and metadata blocks are
   encrypted and authenticated with AES-256-GCM using hardware acceleration
   (AES-NI). Confidentiality and tamper-detection are properties of the
   normal read path, not an optional layer ([`08`](08-encryption.md)).

4. **Self-validating structure.** Every block pointer carries the identity
   and authentication tag of its target ([`02 §BlockPtr`](02-on-disk-layout.md#blockptr)).
   Following a pointer both locates and verifies the target block; a
   substituted, replayed, or rolled-back block is detected on read.

5. **Growable metadata.** Inode count is not fixed. The inode map is a COW
   radix tree; inodes are added by growing the tree, bounded only by device
   space ([`06`](06-inodes.md)). Contrast V1's fixed 1024-inode table.

6. **No persistent free-space bitmap.** Free space is reconstructed at mount
   by mark-and-sweep from the live tree ([`05`](05-space-management.md)).
   There is no on-disk allocation structure to keep consistent, so there is
   no allocation structure to corrupt.

7. **Familiar, adequate shapes where they cost nothing.** The 128-byte inode
   and ext2-style directory entries are carried forward from V1 because they
   are well understood and sufficient. Everything structural around them is
   new.

## 2. Non-goals

- **No formal proof of correctness.** This is not seL4. The crash-consistency
  argument is a written argument ([`09`](09-consistency.md)), not a machine
  proof.
- **No hardware root of trust.** Key derivation is from a passphrase via
  Argon2id. There is **no TPM** and no sealed-key storage. A consequence is
  limited anti-rollback protection (see [§3](#3-threat-model)).
- **No multi-device / RAID / volume management.** One filesystem occupies one
  block device. Subvolumes ([`docs/spec/fhs.md`](../spec/fhs.md)) are a
  logical layer above the on-disk format; V2 defines the single-device
  format only. `TODO(open):` subvolume/snapshot on-disk representation is not
  specified in this document set.
- **No compression.** Reserved by a feature flag ([`10`](10-format-and-compat.md))
  but not specified here.
- ~~No hard links in the initial format.~~ **Resolved: hard links are part of
  the baseline** ([`07 §3`](07-directories.md#3-directory-operations),
  [`06 §5`](06-inodes.md#5-inode-lifecycle)); volumes advertise
  `feature_ro_compat.HARDLINKS` ([`10 §2`](10-format-and-compat.md#2-feature-flags)).
  Directories cannot be hard-linked.
- **Not seL4 / Redox / Fuchsia.** Per project convention, do not import
  primitives (CSpace, schemes, VMO/VMAR/FIDL) from those systems.

## 3. Threat model

**Adversary.** An attacker with full offline read/write access to the block
device (a lost/stolen disk, a compromised storage backend), but **without**
the passphrase.

**In scope — defended:**

- **Confidentiality of data at rest.** Every data and metadata block is
  AES-256-GCM encrypted under a volume key derived from the passphrase.
  Without the passphrase the attacker learns only device geometry and the
  KDF parameters (the plaintext static header, [`02`](02-on-disk-layout.md)),
  and coarse allocation size.
- **Integrity / tamper-detection.** Any modification to a block's ciphertext
  is detected: the block's GCM tag will not match the tag recorded in the
  parent `BlockPtr` ([`08`](08-encryption.md)). Modification propagates —
  the parent's tag would have to change too, and so on up to the superblock,
  whose own tag is bound to its generation.
- **Block relocation / replay.** The GCM nonce is `block ‖ gen`
  ([`08 §nonce`](08-encryption.md#nonce)). A block moved to a different
  physical location decrypts under a different nonce and fails
  authentication. A stale version of a block (same location, older `gen`)
  fails because the parent pointer records the expected `gen`.
- **Wrong passphrase.** The volume data-encryption key is stored wrapped
  under a passphrase-derived key using GCM; unwrapping with the wrong
  passphrase fails authentication and mount is denied
  ([`08 §key-hierarchy`](08-encryption.md#key-hierarchy)).

**In scope — partially mitigated:**

- **Whole-device rollback.** An attacker who snapshots the entire device and
  later restores it presents a fully self-consistent, correctly-authenticated
  *older* filesystem. Because there is **no TPM** or external monotonic
  counter, V2 cannot cryptographically prove the on-disk generation is the
  newest that ever existed. Within a mounted session the generation only
  moves forward; across power cycles a full-device rollback is not detectable
  by the filesystem alone. `TODO(open):` optional external anti-rollback
  counter (out of scope for the no-TPM baseline).

**Out of scope — not defended:**

- **Online attacker with the key / a running privileged process.** Once
  mounted, normal capability enforcement ([`docs/spec/capabilities.md`](../spec/capabilities.md))
  governs access; the FS crypto does not defend against an authorized caller.
- **Traffic / access-pattern analysis.** Block-level access patterns, file
  sizes rounded to blocks, and directory-tree shape are observable.
- **Denial of service by corruption.** Tampering is *detected*, not
  *repaired*. A corrupted live block makes its subtree unreadable; V2
  reports the error rather than silently returning wrong data.
- **Side channels** (timing, power) in the AES implementation.

## 4. V1 post-mortem — why RFS V1 is unretrofittable

RFS V1 (`kernel/src/rfs.rs`, [`docs/design/rfs.md`](../design/rfs.md)) is a
correct, simple, in-place, unencrypted extent filesystem. Every property RFS
V2 requires contradicts a load-bearing V1 assumption. The changes are not
additive; they invert the write path and replace every on-disk structure.

1. **In-place mutation is pervasive and structural.** `write_inode`
   read-modify-writes the inode's containing block; `append_to_file` writes
   data blocks in place; `add_dir_entry` / `remove_dir_entry` mutate
   directory blocks in place; `alloc_block` / `free_block` mutate the bitmap
   in place. COW's defining rule — *never mutate a live block* — cannot be
   layered on; it requires rewriting every mutating path to allocate-and-
   redirect. There is no intermediate design.

2. **No atomic commit primitive exists.** V1 has a single superblock
   (block 0) with no generation number. There is no point at which a set of
   changes becomes durable atomically; a crash between any two `write_block`
   calls can leave a partially-updated structure. V2's guarantee is built on
   dual superblocks + generation flip, which V1 has no representation for.

3. **The persistent bitmap is a corruption surface, not a feature.** V1's
   in-place bitmap (block 1) can be torn by a crash, desynchronizing free
   space from reality with no way to detect it. V2 deletes the concept: no
   persistent bitmap, free space reconstructed by mark-and-sweep
   ([`05`](05-space-management.md)). This is a removal, not a change to an
   existing field.

4. **The inode table is fixed and small.** V1 hardcodes 1024 inodes in a
   32-block table (`INODE_COUNT = 1024`), and the bitmap caps the device at
   128 MiB (32768 blocks). Both are baked into the block map and the
   allocator's scan loops. Growable inodes require an inode *map* (a tree),
   not a table — a different on-disk object with a different allocator.

5. **Pointers have no room for authentication.** V1 extents store a raw
   `u32` physical block number (16-byte positional extent, no tag). Self-
   validating pointers are `{block:u64, gen:u64, tag:[u8;16]}` = 32 bytes
   ([`02 §BlockPtr`](02-on-disk-layout.md#blockptr)). Every structure that
   holds a pointer — inodes, extent lists, directory maps, the superblock —
   changes shape and size. The extent format itself is replaced by a COW
   block-map tree ([`06`](06-inodes.md)).

6. **"No checksums by design" is the opposite of the V2 requirement.** V1's
   design doc explicitly forgoes checksums. V2 requires authenticated
   encryption on every block. This touches the read path (decrypt+verify on
   every `read_block`), the write path (encrypt+tag on every `write_block`),
   and every pointer (carry the tag). It is not a module that can be inserted
   beneath V1's block I/O without changing the callers, because the tag must
   flow into the parent pointer at write time.

7. **Directory soft-delete is incompatible with COW.** V1 deletes entries by
   zeroing the inode field in place (`remove_dir_entry`) and reuses holes in
   place. Under COW a directory mutation must rewrite the affected directory
   block(s) into fresh locations and re-thread them through the block-map
   tree and a new commit ([`07`](07-directories.md)).

**Conclusion.** Retrofitting COW, dual-superblock commit, authenticated
encryption, self-validating pointers, and a growable inode map onto V1 would
replace the superblock, the allocator, the pointer format, the extent
mechanism, the inode I/O path, and the directory mutation path — i.e. every
on-disk structure and every write path. Only the 128-byte inode footprint
and the ext2-style dirent shape survive, and those are deliberately re-adopted
in V2. A clean design is cheaper and clearer than an incremental one, so V1 is
retired rather than migrated. `TODO(open):` V1→V2 offline migration tool
(read V1, write a fresh V2 image); no in-place upgrade is possible.

## 5. Glossary

- **Block** — the 4096-byte unit of allocation and I/O (8 × 512-byte
  sectors). All addresses are block numbers unless stated.
- **COW (copy-on-write)** — modifying data by writing a new copy to a free
  block and redirecting the pointer, never overwriting the live block.
- **Generation (`gen`)** — a global monotonically-increasing 64-bit counter,
  incremented once per commit. Every block written during commit *K* records
  `gen = K`. Used as the superblock version and as part of every block's
  encryption nonce ([`03`](03-superblock.md), [`08`](08-encryption.md)).
- **Commit** — the act of making a batch of COW changes durable and visible,
  finalized by writing a new superblock with `gen+1` into the inactive slot
  ([`04`](04-cow-and-commit.md)).
- **Superblock slot** — one of two fixed device locations holding a
  superblock. The valid slot with the higher generation is *current*
  ([`03`](03-superblock.md)).
- **Static header** — the plaintext, immutable block 0: geometry, KDF
  parameters, and the wrapped volume key. Read before any key is available
  ([`02`](02-on-disk-layout.md)).
- **`BlockPtr`** — a self-validating pointer `{block:u64, gen:u64,
  tag:[u8;16]}` (32 bytes). Locates and authenticates a target block
  ([`02 §BlockPtr`](02-on-disk-layout.md#blockptr)).
- **Tag** — the 128-bit AES-GCM authentication tag over a block's ciphertext,
  stored in the pointing `BlockPtr`.
- **Nonce** — the AES-GCM initialization value, `block ‖ gen`
  ([`08 §nonce`](08-encryption.md#nonce)).
- **DEK / KEK** — volume Data-Encryption Key (encrypts all blocks) and
  passphrase-derived Key-Encryption Key (wraps the DEK)
  ([`08`](08-encryption.md)).
- **KDF** — Argon2id, deriving the KEK from the passphrase
  ([`08 §kdf`](08-encryption.md#kdf)).
- **Block-map tree** — per-inode COW radix tree mapping a file's logical
  block index to a `BlockPtr` ([`06`](06-inodes.md)).
- **Inode map** — the COW radix tree mapping inode number to its on-disk
  location, rooted in the superblock ([`06`](06-inodes.md)).
- **Mark-and-sweep** — the mount-time traversal of the live tree that
  reconstructs the in-memory free-block set ([`05`](05-space-management.md)).
- **Torn write** — a write interrupted by power loss such that only some of a
  block's sectors reached the medium. Detected via GCM tag mismatch
  ([`09`](09-consistency.md)).
