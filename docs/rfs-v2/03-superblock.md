# RFS V2 — Superblock, Generations, and the Commit Flip

The superblock is the single root of the live filesystem tree and the anchor
of the crash-consistency guarantee. V2 keeps **two** superblock slots at fixed
addresses; the valid slot with the higher generation is *current*. A commit is
finalized by writing a new superblock — one generation higher — into the
*other* slot. There is no separate "active-slot" pointer to update: **the
generation number is the pointer.**

Prerequisites: [`02 §BlockPtr`](02-on-disk-layout.md#blockptr),
[`02 §region-map`](02-on-disk-layout.md#2-device-region-map). Encryption of
the superblock is detailed in
[`08 §superblock`](08-encryption.md#3-block-encryption).

---

## 1. Slots

Two fixed device locations, from the static header
([`02 §3`](02-on-disk-layout.md#3-static-header-block-0)):

- **Slot A** = block `sb_slot_a` (= 1)
- **Slot B** = block `sb_slot_b` (= 2)

At any instant each slot independently holds one of:

- a **valid** superblock (decrypts, authenticates, passes structural checks), or
- an **invalid** slot (blank at mkfs, a torn write, or tampered) — rejected.

The slots are the **only** blocks in the filesystem written in place
([`02 §2`](02-on-disk-layout.md#2-device-region-map)). Every commit overwrites
whichever slot is *not* current, so the two slots hold the two most recent
generations `K` (current) and `K-1` (previous, still valid as a fallback until
the next commit overwrites it).

---

## 2. Superblock structure

A superblock occupies a full 4096-byte slot block. Unlike every other block,
its authentication tag cannot live in a parent pointer (it has no parent), so
the slot is laid out as **4072 bytes of AEAD-protected payload + a 24-byte
plaintext trailer** holding a copy of the generation and the tag. The payload
is AES-256-GCM encrypted under the DEK with nonce = `slot_block ‖ gen`
([`08 §3`](08-encryption.md#3-block-encryption)).

The plaintext `gen_copy` exists because the nonce and AAD both incorporate
`gen`, which a reader must therefore know *before* it can decrypt the payload
— but the payload's own `gen` field is ciphertext. The trailer copy breaks
that cycle. It needs no separate integrity protection: `gen_copy` feeds the
nonce and AAD, so a tampered copy makes authentication fail exactly as a
tampered payload would. (This resolves the former gen-before-decrypt gap.)

Payload layout (bytes 0–4071 of the slot):

| Offset | Size | Field | Meaning |
|--------|------|-------|---------|
| 0 | 8 | `sb_magic` | `"RFSSB\0\0\0"` — superblock magic, distinct from the static-header magic |
| 8 | 8 | `gen` | This superblock's generation. Strictly increasing across commits ([§3](#3-generation-numbers)). |
| 16 | 8 | `total_blocks` | Echo of static-header `total_blocks`; mount rejects a mismatch |
| 24 | 32 | `inode_map_root` | `BlockPtr` to the root of the inode-map tree ([`06 §3`](06-inodes.md#3-the-inode-map)) |
| 56 | 8 | `inode_map_height` | Number of index levels in the inode map (0 = root is a leaf) |
| 64 | 8 | `next_inode` | Lowest inode number never yet allocated (high-water mark) ([`06`](06-inodes.md)) |
| 72 | 8 | `inode_count` | Number of live inodes |
| 80 | 8 | `block_count` | Count of blocks reachable via `inode_map_root` traversal, **excluding** the fixed blocks 0–2. Advisory — see below and [`05 §2`](05-space-management.md#2-mount-time-mark-and-sweep) |
| 88 | 8 | `commit_time` | Wall-clock time of this commit (informational) |
| 96 | 16 | `uuid` | Echo of static-header `uuid`; mount rejects a mismatch |
| 112 | 3960 | `reserved` | Zero |

Trailer (plaintext):

| Offset | Size | Field |
|--------|------|-------|
| 4072 | 8 | `gen_copy` — plaintext copy of the payload's `gen`; read first to form the nonce/AAD |
| 4080 | 16 | `sb_tag` — GCM tag over the 4072-byte payload |

The GCM AAD for a superblock is `sb_magic ‖ gen ‖ slot_block ‖ uuid`, binding
the tag to both the generation and the physical slot so a superblock cannot be
authenticated in the wrong slot or at the wrong generation
([`08 §3`](08-encryption.md#3-block-encryption)).

**Slot validity** now requires all of: the payload decrypts and authenticates
under nonce `slot_block ‖ gen_copy` against `sb_tag`; the decrypted payload's
`gen` equals `gen_copy`; `gen_copy ≥ 1` (`0` = blank slot); and the structural
echoes (`sb_magic`, `total_blocks`, `uuid`) match the static header.

**`block_count` is deliberately advisory, not authoritative.** The mark-sweep
traversal is cryptographically self-validating end-to-end; a counter cannot
add integrity, only detect implementation bugs. Failing the mount on a
mismatch would convert a benign accounting bug into data unavailability, so a
mismatch is surfaced (logged/flagged) and the traversal result governs.

---

## 3. Generation numbers

`gen` is the global monotonic commit counter from
[`01 §5`](01-overview.md#5-glossary):

- **Monotonic:** each commit writes `gen = current_gen + 1`. It never resets,
  never repeats, never decreases.
- **Global:** the same counter versions the superblock, stamps every block
  written in that commit (via `BlockPtr.gen`), and forms the high half of
  every block's encryption nonce
  ([`08 §4`](08-encryption.md#4-nonce-construction)). This coupling is what
  makes each `(block, gen)` nonce unique — see the uniqueness argument in
  [`08 §4`](08-encryption.md#4-nonce-construction).
- **Width:** `u64`. At one commit per millisecond it does not wrap for ~584
  million years. Wrap is nevertheless defined, not left undefined: let
  `GEN_LIMIT = 2⁶⁴ − 256`. Once the current generation reaches `GEN_LIMIT`,
  the volume **freezes to read-only** — every mutating operation and every
  commit is refused (the implementation surfaces a dedicated
  generation-exhausted error). `gen` is never reset, rolled, or rewrapped in
  place: under the same DEK that would reuse `(block, gen)` nonces and break
  GCM ([`08 §CRYPTO-1`](08-encryption.md#4-nonce-construction)). The remedy is
  an offline copy to a freshly formatted volume (fresh DEK, fresh nonce
  space). The 256-generation margin keeps the last usable generations clear of
  any off-by-one at the boundary.

**Validity ordering.** Given the two slots, *current* is the slot that (a)
decrypts and authenticates, (b) passes structural checks (`sb_magic`,
`total_blocks`, `uuid`), and (c) has the higher `gen`. If only one slot is
valid, it is current. If neither is valid, the volume does not mount
([§5](#5-crash-consistency-argument)).

---

## 4. Commit: the pointer-flip protocol

Let the current superblock be generation `K`, in (say) slot A. Slot B holds
generation `K-1` (or is blank on a freshly-formatted volume). To commit a batch
of COW changes ([`04`](04-cow-and-commit.md)):

1. **Write all new tree blocks.** Every new/modified inode-map node, inode,
   block-map node, directory block, and data block for the transaction is
   encrypted (nonce `block ‖ (K+1)`) and written to freshly-allocated blocks
   in the dynamic region. Children before parents, so every `BlockPtr` a
   parent stores already points at a durable, tagged child
   ([`04 §commit-ordering`](04-cow-and-commit.md#4-commit-ordering)).
2. **Barrier.** Flush the device write cache so that all blocks from step 1
   are durable on the medium before the superblock is written
   ([`04 §durability`](04-cow-and-commit.md#5-what-a-commit-makes-durable),
   [`09`](09-consistency.md)). This ordering is load-bearing.
3. **Build the new superblock** with `gen = K+1` and `inode_map_root` pointing
   at the new inode-map root produced in step 1. Encrypt it with nonce
   `slot_B ‖ (K+1)`; place `gen_copy = K+1` and the tag in the plaintext
   trailer.
4. **Write the new superblock into the inactive slot (B).** This is the single
   commit point.
5. **Barrier.** Flush so the superblock write is durable.

After step 4 completes durably, slot B holds a valid generation `K+1 > K`, so
by the validity ordering ([§3](#3-generation-numbers)) **B is now current**.
The "flip" is not a mutation of any shared pointer — it is the mere existence
of a higher valid generation in the other slot. The next commit will target
slot A (now the stale one, holding `K`), and so on, alternating.

### Why the flip is atomic

The transition from "K current" to "K+1 current" hinges entirely on the single
block write in step 4. The GCM tag in the slot trailer is a whole-block
checksum: the slot is valid **iff** its payload authenticates against its tag.
A write that lands completely yields a valid `K+1`; a write interrupted partway
(torn) yields a payload/tag mismatch → the slot is invalid → the reader falls
back to slot A's still-intact `K`. There is no third outcome. The commit is
therefore atomic with respect to power loss: observed either as `K` or as
`K+1`, never as a blend.

---

## 5. Crash-consistency argument {#5-crash-consistency-argument}

A crash can occur at any point. Enumerate by phase (full end-to-end treatment
in [`09`](09-consistency.md)):

- **Before step 4 (during steps 1–3).** No slot changed. Both slots still hold
  `{K, K-1}`; `K` remains current. The blocks written in step 1 are
  unreferenced by any valid superblock — harmless garbage, reclaimed as free
  by mark-and-sweep at next mount ([`05`](05-space-management.md)). The
  filesystem mounts to state `K`. **No data loss beyond the uncommitted
  transaction; no corruption.**
- **During step 4 (torn superblock write).** Slot B's payload and tag
  disagree → B is invalid. Slot A's `K` is untouched and valid. Mount selects
  `K`. Identical outcome to the previous case. **Atomic.**
- **After step 4 durable (crash in/after step 5).** Slot B holds a valid
  `K+1 > K`. Mount selects `K+1`. Every block `K+1` references was made durable
  in step 2 before B was written, so the tree is complete. **The transaction
  is committed.**

The two slots guarantee that a torn write to the *new* superblock can never
destroy the *old* one, so there is always at least one valid superblock to
mount from — provided the two writes are never in flight simultaneously, which
the alternating protocol enforces (a commit only ever writes the non-current
slot).

**Double-fault caveat.** If the medium corrupts the *current* slot
independently of the commit (bit-rot, a lie by the device about an earlier
flush), mount may find only the older slot valid and silently roll back one
commit, or find neither valid and refuse to mount. This is the same exposure
any dual-superblock design has; V2 detects it (tag mismatch) rather than
returning wrong data. `TODO(open):` optional N>2 superblock ring for extra
redundancy — not in the baseline.

---

## 6. Invariants

> **SB-1.** At least one slot is valid at all times during normal operation;
> a commit never writes the currently-current slot.

> **SB-2.** `gen` is strictly increasing per commit and never reused. The
> current superblock has the maximum `gen` of any valid on-disk structure.

> **SB-3.** All blocks reachable from a valid superblock of generation `G` were
> written in some commit `≤ G` and are durable before that superblock became
> durable.

> **SB-4.** A superblock authenticates only in the slot and at the generation
> recorded in its AAD (`slot_block`, `gen`); it cannot be relocated or
> gen-rolled without detection ([`08`](08-encryption.md)).
