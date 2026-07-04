# RFS V2 — Space Management (Mark-and-Sweep, No Persistent Bitmap)

RFS V2 keeps **no on-disk free-space structure**. Free space is reconstructed
at mount by a mark-and-sweep traversal of the live tree(s); allocation and
reclamation then operate on an in-memory free set. This removes the single
worst corruption surface from V1 — a persistent bitmap that a crash can
desynchronize from reality with no way to detect it
([`01 §4.3`](01-overview.md#4-v1-post-mortem-why-rfs-v1-is-unretrofittable)).

Prerequisites: [`02`](02-on-disk-layout.md) (region map, `BlockPtr`),
[`03`](03-superblock.md) (two valid superblocks), [`04 §6`](04-cow-and-commit.md#6-freeing-and-the-two-live-trees)
(the two-live-trees rule).

---

## 1. Principle

There is nothing on disk that says "this block is free." A block is free **iff
it is not reachable from any valid superblock.** Reachability is the ground
truth; a bitmap would only be a cache of it — and a cache that can lie. V2
therefore derives the free set from reachability at mount and maintains it in
memory thereafter.

> **SPACE-1.** The set of allocated blocks = the set of blocks reachable by
> `BlockPtr` traversal from the current superblock, unioned with those
> reachable from the previous superblock (the fallback slot), plus the three
> fixed blocks 0, 1, 2. Everything else is free.

The previous generation is included because it must stay intact until the next
commit overwrites its slot ([`04 §6`](04-cow-and-commit.md#6-freeing-and-the-two-live-trees));
allocating a block it still references would corrupt the crash-recovery
fallback.

---

## 2. Mount-time mark-and-sweep

At mount, after the DEK is available and the current + previous superblocks are
identified ([`03 §3`](03-superblock.md#3-generation-numbers)):

**Mark.** Starting from each valid superblock, traverse every `BlockPtr`,
marking each visited physical block in an in-memory bitmap
(`total_blocks` bits, 1 bit/block = 32 KiB per GiB of device). Roots and edges:

- Superblock → `inode_map_root`.
- Inode-map index nodes → 128 child `BlockPtr`s each
  ([`06 §3`](06-inodes.md#3-the-inode-map)).
- Inode-map leaf nodes → the 32 inodes they hold; each live inode →
  its block-map root ([`06 §4`](06-inodes.md#4-the-per-file-block-map)).
- Block-map index nodes → 128 child `BlockPtr`s each → down to data blocks.
- Directory inodes' data blocks are file data like any other (their *contents*
  are dirents, but the blocks are reached through the directory inode's
  block-map, so no special traversal is needed for marking).
- Fixed blocks 0, 1, 2 are marked unconditionally.

Every dereference is authenticated ([`02 §4`](02-on-disk-layout.md#blockptr)):
a tag mismatch during marking means the live tree is corrupt and mount fails
loudly rather than marking a wrong block set.

**Sweep.** No separate pass is required to *find* free blocks — the complement
of the mark bitmap *is* the free set. Optionally, a single linear scan builds
an allocation-friendly structure (free-extent list / next-fit cursor) from the
bitmap.

**Cross-check.** Compare the marked block count of the current tree against
the superblock's `block_count` — defined as the number of blocks reachable via
`inode_map_root`, excluding fixed blocks 0–2
([`03 §2`](03-superblock.md#2-superblock-structure)). A mismatch is surfaced
(logged/flagged) but not fatal. The counter is deliberately advisory: the
traversal is authenticated end-to-end and is the ground truth; a counter can
only detect implementation bugs, and refusing to mount over one would turn an
accounting bug into data unavailability.

---

## 3. Cost

- **Time:** O(number of live blocks). Each live block is read and
  authenticated once. This is proportional to used space, not device size —
  an empty large device mounts almost instantly; a full one pays one full read
  pass. Marking is dominated by block I/O + AES-GCM verification (AES-NI).
- **Memory:** the mark bitmap is `total_blocks` bits (32 KiB per GiB;
  32 MiB for a 1 TiB device). This is the main scaling cost and is
  acceptable for the target device sizes.
- **Comparison:** V1 paid nothing at mount but risked a silently-corrupt
  bitmap forever after. V2 trades a bounded mount-time scan for the permanent
  absence of an on-disk allocation structure that can be wrong.

`TODO(open):` very large devices where a full mark pass at every mount is
undesirable. Possible mitigations (a persisted *hint* bitmap that is verified,
not trusted; background/lazy marking) are out of the baseline — the baseline
always reconstructs from scratch.

---

## 4. Allocation

Allocation is an in-memory operation against the free set; it touches no disk
metadata (there is none to touch).

- **Request:** the COW write path ([`04`](04-cow-and-commit.md)) asks for one
  free block.
- **Selection:** pick a block that is free per SPACE-1. Baseline policy is
  next-fit from a rotating cursor to spread writes; allocation policy does not
  affect correctness, only layout. `TODO(open):` allocation heuristic
  (locality for sequential files, wear-leveling) — unspecified, free to tune.
- **Reservation:** the chosen block is marked allocated in the in-memory
  bitmap immediately, so it is not handed out twice within the transaction.
- **No nonce reuse:** the block is written with nonce `block ‖ G` where `G` is
  the current commit generation. Because `G` is strictly greater than any
  generation in which this physical block was previously written
  ([`03 §3`](03-superblock.md#3-generation-numbers)), the `(block, gen)` nonce
  is fresh — see the uniqueness argument in
  [`08 §4`](08-encryption.md#4-nonce-construction).

**Out-of-space.** If no block satisfies SPACE-1, allocation fails and the
transaction returns `ENOSPC` ([`docs/spec/syscalls.md`](../spec/syscalls.md));
the in-progress commit is abandoned and generation `K` remains current
([`04 §2`](04-cow-and-commit.md#2-the-cow-write-path-single-modification)).

---

## 5. Reclamation (freeing)

There is no explicit "free block" operation on disk. A block becomes free when
it stops being reachable, which happens implicitly at commit boundaries:

1. During commit `G = K+1`, superseded blocks (old data + old spine) become
   unreferenced by the new tree but remain referenced by generation `K` in the
   fallback slot ([`04 §6`](04-cow-and-commit.md#6-freeing-and-the-two-live-trees)).
   They are **not** freed yet.
2. When commit `G+1` overwrites `K`'s slot, generation `K`'s tree is abandoned.
   Blocks that were referenced *only* by `K` (not by `K+1` or `K+2`) are now
   unreachable and become free.

The baseline maintains this incrementally in memory: as each commit is
prepared, the allocator tracks which blocks the outgoing generation uniquely
owned and returns them to the free set once that generation's slot is
overwritten. Correctness does not depend on the incremental bookkeeping being
perfect, because:

> **SPACE-2 (self-healing).** The on-disk free state is *nothing*; the free set
> is always re-derivable exactly by mark-and-sweep. A bookkeeping bug can only
> leak blocks (temporarily under-counting free space) or, at worst, be caught
> by the reachability check before a block is reused — it cannot hand out a
> live block, because allocation validates against SPACE-1. A remount
> reconstructs the exact free set from the tree regardless of prior in-memory
> state.

This is the core payoff of having no persistent allocation structure: the free
map cannot be *corrupt*, only *stale in memory*, and staleness is erased by the
next mount.

---

## 6. Correctness summary

> **SPACE-3.** A block returned by the allocator is not reachable from either
> valid superblock at allocation time (never aliases live data).

> **SPACE-4.** After any crash, the free set reconstructed at mount equals the
> exact complement of the live set of the mounted generation(s); no repair pass
> and no on-disk allocation metadata are consulted, because none exists.

> **SPACE-5.** The two-live-trees rule ([`04 §6`](04-cow-and-commit.md#6-freeing-and-the-two-live-trees))
> is upheld: blocks uniquely owned by the previous generation are treated as
> allocated until that generation's superblock slot is overwritten.
