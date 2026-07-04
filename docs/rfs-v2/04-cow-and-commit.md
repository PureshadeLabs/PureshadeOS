# RFS V2 — Copy-on-Write Write Path and Commit

This document specifies how a modification travels from a userspace write to a
durable commit: the COW rewrite of a path from leaf to root, how modifications
are grouped into a transaction, the order in which blocks reach the device, and
exactly what a commit makes durable.

Prerequisites: [`02`](02-on-disk-layout.md) (blocks, `BlockPtr`),
[`03`](03-superblock.md) (superblock, generations, the commit flip). Space
allocation is [`05`](05-space-management.md); the tree shapes are
[`06`](06-inodes.md) and [`07`](07-directories.md).

---

## 1. The COW rule

> **COW-1.** No block that is reachable from a valid superblock is ever
> overwritten. A modification writes fresh blocks and is made visible only by
> a superblock commit ([`03 §4`](03-superblock.md#4-commit-the-pointer-flip-protocol)).

The one exception is the superblock slot itself, written in place and made
safe by the dual-slot + generation mechanism ([`03`](03-superblock.md)). Every
other block — inode-map node, inode leaf, block-map node, directory block, data
block — is immutable once committed.

A corollary: **every commit rewrites a full root-to-leaf path** for each thing
it changes. Changing one data block of one file rewrites that data block, every
block-map node above it, the inode, the inode-map leaf holding that inode,
every inode-map index node above that leaf, and finally the superblock. Blocks
*not* on any changed path are shared unchanged between generations `K` and
`K+1` — their `BlockPtr`s (including `gen` and `tag`) are simply copied into
the new parent.

---

## 2. The COW write path (single modification)

Example: application writes to logical block `L` of the file with inode number
`I`. Current generation is `K`; the commit in progress is `G = K+1`.

1. **Materialize the new data block.** Take the file's current bytes for
   logical block `L` (read + decrypt the old block if this is a partial-block
   write; skip the read for a full-block overwrite). Apply the modification in
   memory.
2. **Allocate + write the data block.** Get a free block `b_d` from the
   allocator ([`05`](05-space-management.md)). Encrypt with nonce `b_d ‖ G`,
   write, obtain tag `t_d`. New pointer `P_d = {b_d, G, t_d}`.
3. **Rewrite the block-map spine.** Walk the file's block-map tree
   ([`06 §4`](06-inodes.md#4-the-per-file-block-map)) from the leaf that holds
   logical index `L` up to its root. For each level, allocate a fresh index
   node, copy the sibling `BlockPtr`s unchanged, splice in the updated child
   pointer (`P_d` at the bottom, then each freshly-written index node's pointer
   at the next level up), encrypt, write, capture its tag. This yields a new
   block-map root pointer `P_bm`.
4. **Rewrite the inode.** Copy inode `I` into a new in-memory image, set its
   block-map root to `P_bm`, update `size`, `mtime`, `blocks`, and set the
   inode's `gen = G`. The inode is 128 bytes and lives packed 32-per-block in
   an inode-map *leaf*; see the next step.
5. **Rewrite the inode-map spine.** Locate the inode-map leaf block holding
   inode `I` ([`06 §3`](06-inodes.md#3-the-inode-map)). Allocate a fresh leaf,
   copy the other 31 inodes unchanged, write the updated inode `I` into its
   slot, encrypt, write, capture the tag. Then walk up the inode-map index
   nodes exactly as in step 3, allocating fresh nodes and splicing updated
   child pointers, to produce a new inode-map root pointer `P_im`.
6. **Stage the superblock.** Record `P_im` (and any updated counters:
   `inode_count`, `next_inode`, `block_count`) as the pending superblock
   content for generation `G`. Do **not** write the superblock yet if more
   modifications will join this transaction ([§3](#3-transaction-grouping)).

Steps 2–5 are pure allocation-and-write of fresh blocks; nothing live is
touched. Until the superblock is written, generation `K`'s tree is entirely
intact and remains the mountable state.

### Overwrite vs. hole vs. append

- **Overwrite** an existing logical block: as above; the old data block and old
  spine nodes become unreferenced by `G` (but may still be referenced by `K` —
  see [§6](#6-freeing-and-the-two-live-trees)).
- **Append / grow into a new logical block:** the block-map may need a new leaf,
  and if the file outgrows the current tree height, a new root level is added
  ([`06 §4`](06-inodes.md#4-the-per-file-block-map)). Same rewrite discipline.
- **Sparse hole:** a logical block with no data is a null `BlockPtr`
  ([`02 §4`](02-on-disk-layout.md#blockptr)) in the block-map; reading it
  yields zeros. Writing it allocates as above.

---

## 3. Transaction grouping

A **transaction** is a set of modifications that share one commit. Because a
commit rewrites the entire root-to-leaf spine up to and including the
superblock, batching amortizes that cost: many changed files in one commit
share the upper inode-map index nodes and a single superblock write.

Grouping policy (baseline):

- Modifications accumulate in an **in-memory dirty set** — a working copy of
  the inode-map root and the sub-trees touched since the last commit. Within a
  transaction, a block modified twice is rewritten only once at commit time
  (the working copy coalesces repeated edits before any block is emitted).
- A commit is triggered by any of: an explicit `sync`/`fsync`; the dirty set
  exceeding a size threshold; a periodic commit timer; or unmount.
- All modifications in the dirty set at trigger time share generation `G` and
  one superblock write.

**`fsync` semantics (resolved).** The commit — the whole dirty set under one
generation — is the durability unit. `fsync(F)` performs a full commit:

- **Guarantee:** when `fsync(F)` returns, *every* modification staged before
  the call (to `F` and to every other file) is durable as generation `G`; any
  later crash mounts to `G` or newer. This is strictly stronger than POSIX
  requires for `F` — over-delivery, never under-delivery.
- **Non-guarantee:** there is no way to persist `F` alone. `fsync(A)` also
  commits unrelated dirty file `B`.
- **Why:** the design has one global generation and one superblock root.
  Per-file isolation would require multiple pending generations with partially
  merged inode-map roots — a second commit machinery whose failure modes
  (which root is the fallback?) would erode the single-flip crash argument
  ([`03 §5`](03-superblock.md#5-crash-consistency-argument)). The baseline
  buys simplicity of the consistency proof with occasionally-larger fsyncs.

`TODO(open):` concurrent writers during a commit. The baseline treats commit
as a barrier: writes that arrive mid-commit either join the in-flight
transaction (if before the superblock is staged) or start the next one. The
locking model is an implementation detail not fixed here.

---

## 4. Commit ordering {#4-commit-ordering}

The ordering that makes crash-consistency hold ([`03 §5`](03-superblock.md#5-crash-consistency-argument),
[`09`](09-consistency.md)):

> **COW-2 (children before parents).** A block is written to the device only
> after every block it points to has been written. Equivalently: emit the tree
> bottom-up, so each `BlockPtr` a parent carries already references a durable,
> correctly-tagged child.

> **COW-3 (superblock last, after a barrier).** All non-superblock blocks of
> the transaction are made durable (device cache flushed) *before* the new
> superblock is written; the superblock write is then itself flushed. See
> [`03 §4`](03-superblock.md#4-commit-the-pointer-flip-protocol) steps 2–5.

COW-2 guarantees that at the instant the superblock becomes durable, following
any pointer from it reaches a fully-written subtree — there is no dangling
`BlockPtr` into a not-yet-written block. COW-3 guarantees the superblock (the
sole thing that makes the new tree reachable) is the *last* durable write, so a
crash before it leaves generation `K` current and the new blocks as
unreferenced garbage.

The tag flow forces COW-2 naturally: a parent cannot compute its own
correctly-formed `BlockPtr` for a child until the child's ciphertext (and thus
its tag) exists. You physically cannot write a valid parent before its child.

---

## 5. What a commit makes durable {#5-what-a-commit-makes-durable}

When commit `G` completes (its superblock is durable in the inactive slot):

- Every modification in the transaction's dirty set is durable and atomically
  visible: a subsequent mount reads generation `G`.
- Nothing outside the dirty set changed on disk; unchanged subtrees are shared
  by reference with generation `K`.
- Partial visibility is impossible: there is no mount state in which some but
  not all of transaction `G`'s changes appear. The generation flip is atomic
  ([`03 §4`](03-superblock.md#4-commit-the-pointer-flip-protocol)).

Before commit `G` completes, none of its changes are durable; a crash reverts
to generation `K` in full.

**Dependence on honest barriers.** COW-3 assumes the device actually persists
data on flush and does not reorder the superblock write ahead of the data
writes. A device that lies about flushes can violate durability ordering; this
is out of the filesystem's control and noted in [`09`](09-consistency.md).

---

## 6. Freeing and the two live trees {#6-freeing-and-the-two-live-trees}

COW does not free anything at write time — the old blocks are still referenced
by generation `K`, which remains valid in its slot until the *next* commit
overwrites it ([`03 §1`](03-superblock.md#1-slots)). Therefore, immediately
after commit `G = K+1`:

- **Two trees are live:** generation `K+1` (current) and generation `K`
  (previous, still in the other slot as the crash-recovery fallback).
- A block is genuinely free only when it is referenced by **neither** tree.
- The blocks superseded during commit `G` (old data + old spine) are unreferenced
  by `K+1` but may still be referenced by `K`; they become free only after
  commit `G+1` overwrites `K`'s slot and abandons `K`'s tree.

This "keep the previous generation until the next commit" rule is why the
allocator must consider both valid superblocks live, and why free space is
recovered by mark-and-sweep from *both* roots at mount rather than by eager
per-commit freeing. The allocation and reclamation rules are specified in
[`05 §space-management`](05-space-management.md). The key invariant:

> **COW-4.** No block reachable from either currently-valid superblock is ever
> allocated for a new write. A superseded block is returned to the free set
> only once no valid superblock references it (i.e., after the older generation
> is overwritten).
