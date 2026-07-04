# RFS V2 — Inodes, the Inode Map, and the Block Map

This document defines the 128-byte inode, the COW radix tree that maps inode
numbers to inodes (the **inode map**, rooted in the superblock), and the
per-file COW radix tree that maps a file's logical block index to a data block
(the **block map**, rooted in each inode). Both trees are built from the same
128-fanout index node over `BlockPtr`s
([`02 §4`](02-on-disk-layout.md#blockptr)).

Prerequisites: [`02`](02-on-disk-layout.md), [`03`](03-superblock.md),
[`04`](04-cow-and-commit.md). Directory *contents* are [`07`](07-directories.md).

---

## 1. Inode format (128 bytes)

Inodes are 128 bytes, packed 32 per 4096-byte inode-map leaf block
([§3](#3-the-inode-map)). All integers little-endian.

| Offset | Size | Field | Meaning |
|--------|------|-------|---------|
| 0 | 2 | `mode` | POSIX type + permission bits (`S_IFREG`, `S_IFDIR`, `S_IFLNK`, rwx…) |
| 2 | 2 | `flags` | RFS inode flags ([§2](#2-inode-flags)) |
| 4 | 4 | `uid` | Owner user id |
| 8 | 4 | `gid` | Owner group id |
| 12 | 4 | `nlink` | Hard-link count: number of dirents referencing this inode (files start at 1 and grow via `link` — [`07 §3`](07-directories.md#3-directory-operations); dirs hold `2 + subdirs`) |
| 16 | 8 | `size` | Logical file size in bytes (for dirs: total dirent-block bytes) |
| 24 | 8 | `blocks` | Count of 4096-B data blocks allocated to this file |
| 32 | 8 | `mtime` | Last data-modification time (ns since epoch) |
| 40 | 8 | `ctime` | Last inode-change time |
| 48 | 8 | `atime` | Last access time |
| 56 | 8 | `btime` | Birth (creation) time |
| 64 | 8 | `inode_gen` | Commit generation in which this inode image was last written ([`03 §3`](03-superblock.md#3-generation-numbers)) |
| 72 | 1 | `bmap_height` | Block-map tree height ([§4](#4-the-per-file-block-map)): 0 = root points at a data block; ≥1 = root points at an index node |
| 73 | 7 | `reserved` | Zero |
| 80 | 8 | `rdev` | Device id for device-special files (else 0) — `TODO(open)` device nodes |
| 88 | 8 | `reserved` | Zero |
| 96 | 32 | `bmap_root` | `BlockPtr` — root of this file's block map ([§4](#4-the-per-file-block-map)) |

Total: **128 bytes**.

### Symlinks {#symlinks}

Resolved. Symlink target storage is gated by the `FAST_SYMLINK` flag
([§2](#2-inode-flags)); `size` is always the target length in bytes:

- **Fast symlink** (`FAST_SYMLINK = 1`, target ≤ **48 bytes**): the target is
  stored inline in the inode, occupying the full byte span at offsets
  **80–127** (the `rdev` + reserved span, 16 bytes, plus the `bmap_root`
  span, 32 bytes). A fast symlink has **no block map**: the bytes at 96–127
  are target data, not a `BlockPtr`, and must never be dereferenced or
  traversed (mark-and-sweep skips the block map of any `FAST_SYMLINK` inode).
  `rdev` is meaningless for symlinks, so no information is lost.
- **Slow symlink** (`FAST_SYMLINK = 0`, target > 48 bytes): the target is
  ordinary file data reached through `bmap_root`, like a regular file's
  contents.

`readlink` returns exactly `size` bytes from the inline span or the data
blocks. Path-resolution policy (symlinks are **not** followed by the
filesystem layer) is specified in [`07 §3`](07-directories.md#3-directory-operations).

---

## 2. Inode flags {#2-inode-flags}

`flags` (offset 2, `u16`) bits — the authoritative type discriminator is
`mode`; `flags` carries RFS-specific hints:

| Bit | Name | Meaning |
|-----|------|---------|
| 0 | `USED` | Inode slot is live (an unused slot is all-zero, `USED=0`) |
| 1 | `FAST_SYMLINK` | Target stored inline (see [§1](#symlinks)) |
| 2–15 | reserved | Zero |

An all-zero 128 bytes is a canonical **free inode slot** (`USED=0`). The inode
map may contain such slots inside otherwise-live leaf blocks.

---

## 3. The inode map {#3-the-inode-map}

The inode map translates an inode number (`u64`) to the 128 bytes of that
inode. It is a **COW radix tree** rooted at the superblock's `inode_map_root`
([`03 §2`](03-superblock.md#2-superblock-structure)); its height is the
superblock's `inode_map_height`.

### Node types

- **Leaf block** (4096 B): 32 inodes packed back-to-back (32 × 128 = 4096).
  Holds inode numbers `[base, base+32)` for some 32-aligned `base`.
- **Index block** (4096 B): 128 `BlockPtr`s (128 × 32 = 4096), each pointing to
  a child node one level down.

### Number decomposition

An inode number is split from the low bits up:

- **bits 0–4** (× 32): index of the inode *within* its leaf block.
- **bits 5–11** (× 128): index within the level-1 index block.
- **bits 12–18**, **19–25**, … : successive index levels, 7 bits (fanout 128)
  each.

Height `h` (index levels above the leaf) addresses `32 × 128^h` inodes:

| Height | Addressable inodes | Max inode number |
|--------|--------------------|------------------|
| 0 (root is a leaf) | 32 | 31 |
| 1 | 4 096 | 4 095 |
| 2 | 524 288 | 524 287 |
| 3 | 67 108 864 | 67 108 863 |
| 4 | 8 589 934 592 | ~8.6 G |

### Reserved inode numbers

| Inode | Use |
|-------|-----|
| 0 | Null / "no inode" (a dirent with `inode == 0` is empty — [`07`](07-directories.md)) |
| 1 | Root directory `/` |
| 2–9 | Permanently reserved for future fixed roles. Never handed out by the general allocator, even while unassigned; a role granted later must not collide with an existing volume's inodes. |
| 10+ | General allocation |

`next_inode` in the superblock is the general allocator's high-water mark and
is written as **10** by mkfs ([`10 §3`](10-format-and-compat.md#3-mkfs-parameters)):
mkfs itself places only the fixed root (inode 1), and the reserved band is
excluded from the counter's range by construction, so mkfs and the allocator
agree without a special case.

### Growth

`next_inode` in the superblock ([`03 §2`](03-superblock.md#2-superblock-structure))
is the high-water mark. Allocating an inode:

1. **No slot reuse (resolved).** V2.0 allocation is bump-only: every
   allocation takes `next_inode` and increments it; freed slots become
   all-zero holes and are never re-issued. Inode numbers are therefore unique
   over the volume's lifetime — a deliberate property (no ABA for handle
   caches, dirent `inode` fields can never dangle onto a recycled identity).
   The cost is tree-node occupancy for sparse leaves; a future offline
   compaction tool may rewrite a volume densely, but no in-place reuse is
   permitted.
2. If `next_inode` exceeds the current height's capacity, **grow the tree**:
   allocate a new root index block, put the old root as its child 0, increment
   `inode_map_height`. This is a COW write like any other and is finalized by
   the enclosing commit ([`04`](04-cow-and-commit.md)). Growth is O(1) blocks
   per level and happens rarely (only at power-of-128 boundaries).

There is **no fixed inode table and no inode bitmap** — the contrast with V1's
hardcoded 1024-inode table ([`01 §4.4`](01-overview.md#4-v1-post-mortem-why-rfs-v1-is-unretrofittable)).
Inode capacity is bounded only by device space for tree nodes.

### COW updates

Modifying inode `I` rewrites its leaf block and every index node up to the
root, then the superblock — the inode-map spine rewrite of
[`04 §2`](04-cow-and-commit.md#2-the-cow-write-path-single-modification) step 5.
Unchanged sibling inodes in the leaf and unchanged sibling pointers in index
nodes are copied by value into the fresh blocks.

---

## 4. The per-file block map {#4-the-per-file-block-map}

Each inode's `bmap_root` roots a **COW radix tree** mapping a file's logical
block index (`size`-derived, 0-based) to a data block. Same node shapes as the
inode map's index level: an index block is 128 `BlockPtr`s.

### Height encoding

`bmap_height` (inode offset 72) gives the number of index levels:

| `bmap_height` | `bmap_root` points at | Max logical blocks | Max file size |
|---------------|-----------------------|--------------------|---------------|
| 0 | a data block directly | 1 | 4 KiB |
| 1 | a 128-entry index block | 128 | 512 KiB |
| 2 | index → index → data | 16 384 | 64 MiB |
| 3 | three index levels | 2 097 152 | 8 GiB |
| 4 | four index levels | 268 435 456 | 1 TiB |
| 5 | five index levels | 34 359 738 368 | 128 TiB |

A null `bmap_root` (`block == 0`) with `bmap_height = 0` denotes an empty file
(`size == 0`).

### Logical-block lookup

To read logical block `L` of a file of height `h`:

- If `h == 0`: the datum is `bmap_root` (valid only for `L == 0`).
- Else split `L` into `h` groups of 7 bits (fanout 128), most-significant group
  first, and walk index → index → … → data, dereferencing each `BlockPtr`
  ([`02 §4`](02-on-disk-layout.md#blockptr)) with authentication.
- A null `BlockPtr` at any level is a **sparse hole**: logical block `L` reads
  as zeros; no block is allocated ([`04 §2`](04-cow-and-commit.md#overwrite-vs-hole-vs-append)).

### Growth

When a write extends the file past the current height's capacity, add a level:
allocate a new root index block, store the old root as its child 0, increment
`bmap_height`. As with the inode map, this is a COW write folded into the
commit.

### The last block

`size` is exact in bytes; the final data block is zero-padded to 4096 before
encryption (encryption is always whole-block —
[`02 §1`](02-on-disk-layout.md#1-block-model)). Readers honor `size` and ignore
padding.

---

## 5. Inode lifecycle

- **Create** ([`07`](07-directories.md) `create`/`mkdir`): allocate an inode
  number ([§3](#3-growth)), write a fresh inode image (`USED=1`, `mode`, times
  = now, `nlink` as appropriate, `bmap_root` null, `size` 0), splice it into
  the inode-map spine, add a dirent in the parent directory, commit. All within
  one transaction.
- **Modify:** any metadata or data change rewrites the inode via the COW spine
  ([`04 §2`](04-cow-and-commit.md#2-the-cow-write-path-single-modification)),
  bumping `inode_gen`, `ctime`, and `mtime`/times as applicable.
- **Truncate/grow:** adjust `size`, allocate or drop logical blocks, and
  restructure `bmap_height` as needed; dropped blocks become free per the
  reclamation rules ([`05 §5`](05-space-management.md#5-reclamation-freeing)).
- **Unlink / delete:** decrement `nlink` (multiple dirents may share the
  inode — hard links, [`07 §3`](07-directories.md#3-directory-operations)).
  The inode and its blocks are freed only when **both** conditions hold:
  `nlink == 0` and no open handle pins the inode. Freeing clears the slot to
  all-zero (`USED=0`) in a fresh leaf and drops the block map (blocks become
  free after the next generation is retired —
  [`05 §5`](05-space-management.md#5-reclamation-freeing)).

  **Deleted-but-pinned (resolved):** if `nlink` reaches 0 while an open handle
  exists, the inode is written back with `nlink = 0`, `USED = 1` — an
  **orphan**. It stays fully readable through the (in-memory) open table; it
  is unreachable from any directory. The last handle release frees it as a
  normal staged operation. If the session ends first (crash, or unmount with
  pins held), the orphan persists on disk; the next **read-write mount**
  reclaims all orphans in one immediately-committed transaction
  ([`09 §4`](09-consistency.md#4-mount-recovery-procedure)). Read-only mounts
  leave orphans in place.

- **atime (resolved — noatime):** reads never modify on-disk state. Under COW
  an atime bump would rewrite the inode's whole spine and burn a commit per
  read, so V2 does not maintain access times: `atime` is set at creation and
  by explicit metadata operations (future utimens-style call), never by
  `read`/`readdir`/`lookup`. Read paths are guaranteed commit-free.

> **INODE-1.** Every live inode is reachable at exactly one inode-map leaf slot
> determined by its inode number; the inode-map spine and the superblock make
> that slot's bytes authenticated end-to-end.

> **INODE-2.** A file's data at logical block `L` is either a single
> authenticated data block reached through `bmap_root`, or a sparse hole (null
> pointer) reading as zeros; there is no third state.
