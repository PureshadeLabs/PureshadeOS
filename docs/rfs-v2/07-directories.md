# RFS V2 — Directories

Directories in RFS V2 are ordinary files whose data blocks contain
**ext2-style variable-length directory entries** (dirents). The directory inode
is a normal inode (`mode` = `S_IFDIR`) with a block map like any file
([`06 §4`](06-inodes.md#4-the-per-file-block-map)); its *contents* are dirents.
All directory mutations obey COW — a changed directory block is rewritten to a
fresh location and rethreaded through the block-map and inode-map spines
([`04`](04-cow-and-commit.md)), never edited in place. This replaces V1's
in-place soft-delete, which is incompatible with COW
([`01 §4.7`](01-overview.md#4-v1-post-mortem-why-rfs-v1-is-unretrofittable)).

Prerequisites: [`06`](06-inodes.md) (inodes, block map),
[`04`](04-cow-and-commit.md) (COW write path).

---

## 1. Directory entry format

Dirents are packed into the directory's 4096-byte data blocks. Each entry is a
fixed 12-byte header followed by the name; `rec_len` chains to the next entry.
Entries never straddle a block boundary. All integers little-endian.

| Offset | Size | Field | Meaning |
|--------|------|-------|---------|
| 0 | 8 | `inode` | Inode number of the target. `0` = unused/empty slot (a hole). |
| 8 | 2 | `rec_len` | Total bytes this record occupies, including header, name, and padding. Advances to the next entry. |
| 10 | 1 | `name_len` | Length of `name` in bytes (≤ 255). |
| 11 | 1 | `file_type` | Cached type ([§2](#2-file-type-values)) so lookup need not read the inode. |
| 12 | `name_len` | `name` | Entry name, UTF-8, **not** NUL-terminated. |
| 12+`name_len` | pad | `padding` | Zero, so `rec_len` is 4-byte aligned. |

This mirrors ext2's dirent, with **one deliberate change**: `inode` is a `u64`,
not a `u32`, because V2 inode numbers are growable and 64-bit
([`06 §3`](06-inodes.md#3-the-inode-map)). The header is therefore 12 bytes
(vs. ext2's 8), preserving 4-byte alignment of the name.

### Record chaining and holes

- `rec_len ≥ 12 + name_len`, rounded up to a multiple of 4.
- The **last** entry in a block has `rec_len` extended to reach the block end,
  so iteration stops exactly at 4096.
- A deleted entry is turned into a **hole** by setting `inode = 0` (the record
  is retained, its `rec_len` optionally merged into the previous record). Space
  is reclaimed within the block; the block itself is rewritten COW on the next
  mutation.
- `.` and `..` are the first two entries of every directory block-0, created at
  `mkdir` time.

---

## 2. File-type values {#2-file-type-values}

`file_type` caches the target's type to avoid an inode read during `readdir`:

| Value | Name | Meaning |
|-------|------|---------|
| 0 | `UNKNOWN` | Unknown / not cached |
| 1 | `REG` | Regular file |
| 2 | `DIR` | Directory |
| 3 | `SYMLINK` | Symbolic link |
| 4 | `CHRDEV` | Character device (`TODO(open)`) |
| 5 | `BLKDEV` | Block device (`TODO(open)`) |
| 6 | `FIFO` | Named pipe (`TODO(open)`) |
| 7 | `SOCK` | Socket (`TODO(open)`) |

The authoritative type is always the target inode's `mode`
([`06 §1`](06-inodes.md#1-inode-format-128-bytes)); `file_type` is an
optimization and must agree with it.

---

## 3. Directory operations

All of these are single COW transactions ([`04 §3`](04-cow-and-commit.md#3-transaction-grouping)):
they rewrite the affected directory data block(s), the directory's block-map
spine, the directory inode (and any target inode), the inode-map spine, and
finally the superblock.

### lookup(dir, name)

Linear scan of the directory's data blocks (logical block 0 upward), comparing
`name_len` + `name` against each non-hole dirent, until a match or EOF. Returns
the target inode number. Cost O(entries); see [§4](#4-large-directory-behavior).

### create / mkdir(dir, name)

1. `lookup` to reject duplicates → `EEXIST` if present
   ([`docs/spec/syscalls.md`](../spec/syscalls.md)).
2. Allocate the target inode ([`06 §5`](06-inodes.md#5-inode-lifecycle)); for
   `mkdir`, initialize its block 0 with `.` (self) and `..` (dir) and set
   `nlink = 2`, and increment the parent's `nlink`.
3. Find a hole in `dir` with `rec_len` large enough for `12 + name_len`
   (rounded up); if none, append a new dirent, growing the directory file by a
   block if the last block is full.
4. Write the new dirent; rewrite the affected directory block COW.
5. Commit.

### unlink / rmdir(dir, name)

1. `lookup`; `ENOENT` if absent. For `rmdir`, verify the target directory
   contains only `.` and `..` (else `ENOTEMPTY`).
2. Turn the dirent into a hole (`inode = 0`), rewrite the directory block COW.
3. Decrement the target inode's `nlink`; free the inode when it reaches 0
   ([`06 §5`](06-inodes.md#5-inode-lifecycle)). For `rmdir`, also decrement the
   parent's `nlink` (the child's `..`).
4. Commit.

### rename(old_dir, old_name, new_dir, new_name) — resolved

All effects — removed source dirent, added/replaced destination dirent,
`..` retarget, `nlink` fixups, and any freed target inode — are staged in
**one transaction** and become visible atomically at the single superblock
commit ([`04 §5`](04-cow-and-commit.md#5-what-a-commit-makes-durable)). No
intermediate state (both names present, neither present) is ever mountable.

Semantics, checked in this order:

1. Source must exist (`ENOENT`). Neither name may be `.` or `..`, and the
   root directory cannot be renamed (`EINVAL`).
2. **Self no-ops:** if source and destination are the same directory entry,
   or the destination exists and refers to the **same inode** as the source
   (hard links to one file), rename succeeds and changes nothing (POSIX).
3. **Loop check:** a directory may not be moved into itself or any of its
   descendants. Walk from the destination directory up via `..` to the root;
   encountering the source → `EINVAL`.
4. **Existing target:**
   - source file, target file → target is replaced; its `nlink` is
     decremented and it is freed at 0 per the lifecycle rules
     ([`06 §5`](06-inodes.md#5-inode-lifecycle)).
   - source dir, target dir → target must contain only `.`/`..`
     (else `ENOTEMPTY`); the empty target is freed and replaced.
   - source file, target dir → `EISDIR`. source dir, target file → `ENOTDIR`.
5. **Directory move across parents:** the moved directory's `..` dirent is
   retargeted to the new parent (a COW rewrite of its block 0); old parent
   `nlink--`, new parent `nlink++`. Replacing an empty target dir also drops
   the `..` it held on the destination parent (`nlink--`).
6. Source `ctime` is bumped; both affected parents' `mtime`/`ctime` are
   bumped.

### link(dir, name → existing inode) — hard links

Creates an additional dirent for an existing **non-directory** inode
(`nlink++`, `ctime` bump). Directories cannot be hard-linked (`EPERM`):
allowing them would create cycles the `..`-based loop check and `nlink`
accounting cannot represent. The new dirent's `file_type` copies the target's
type. `EEXIST` if `name` is taken. Symlinks may be hard-linked (the link
counts the symlink inode itself; no following).

### symlink(target, dir, name) / readlink

Creates an `S_IFLNK` inode (`nlink = 1`, `size` = target length) with the
target stored per [`06 §symlinks`](06-inodes.md#symlinks) (inline ≤ 48 bytes,
else file data). `readlink` returns the raw target bytes; it never resolves.

**Traversal policy (resolved): the filesystem does not follow symlinks.**
Path resolution treats a symlink like any non-directory leaf: as the final
component it resolves to the symlink inode itself; as an intermediate
component it fails with `ENOTDIR`. Following — including loop limits
(ELOOP) and cross-filesystem semantics — is the caller's (VFS/kernel)
responsibility. This keeps the on-disk layer loop-free and policy-free.

---

### Error names per operation (canonical)

| Operation | Errors |
|-----------|--------|
| `lookup` | `ENOENT`, `ENOTDIR` (non-dir or symlink as intermediate component), `EINVAL` (malformed path) |
| `create` / `mkdir` | `EEXIST`, `ENOENT` (parent), `ENOTDIR` (parent), `EINVAL` (name), `ENOSPC`, `EROFS` |
| `unlink` | `ENOENT`, `EISDIR` (target is a directory), `EROFS` |
| `rmdir` | `ENOENT`, `ENOTDIR` (target not a directory), `ENOTEMPTY`, `EINVAL` (root, `.`), `EROFS` |
| `rename` | `ENOENT`, `EISDIR`, `ENOTDIR`, `ENOTEMPTY`, `EINVAL` (loop, root), `ENOSPC`, `EROFS` |
| `link` | `ENOENT`, `EEXIST`, `EPERM` (directory target), `ENOSPC`, `EROFS` |
| `symlink` | `EEXIST`, `ENOENT` (parent), `EINVAL` (empty target), `ENOSPC`, `EROFS` |
| `readlink` | `ENOENT`, `EINVAL` (not a symlink) |

Numeric errno assignment lives in the kernel ABI
([`docs/spec/syscalls.md`](../spec/syscalls.md)); this table fixes the
*names and conditions*. `TODO(open):` `ENOTEMPTY`, `EPERM`, and `EROFS` have
no sentinel values in the syscall ABI yet — assigning them is a cross-cutting
ABI change made at kernel integration, not here.

## 4. Large-directory behavior {#4-large-directory-behavior}

The baseline directory is an **unindexed linear list** of dirent blocks:
lookup, create-collision-check, and unlink are O(entries). This is adequate for
typical directories and matches V1's shape. It degrades for directories with
many thousands of entries.

A future hashed directory index (an on-disk hash tree over names, à la
ext2/3 `dir_index`) would make lookup O(log n). **Its feature mask is
resolved: `feature_ro_compat` bit 1** ([`10 §2`](10-format-and-compat.md#2-feature-flags)).
The deciding constraint: the index must be stored *alongside* the complete
linear dirent list (an acceleration structure, not a replacement), so a
reader without index support can still read every directory correctly by
linear scan — but must not write, since it cannot keep the index coherent.
That is exactly the ro_compat contract. `TODO(open):` the index's on-disk
format itself (hash function, node layout, placement) — not in the V2.0
baseline; the linear format above remains the ground truth.

---

## 5. Invariants

> **DIR-1.** Every directory data block is a valid dirent chain: records are
> 4-byte aligned, `rec_len` values partition the block exactly, and the last
> record reaches the block end. Iteration terminates at 4096 with no straddle.

> **DIR-2.** Every non-hole dirent (`inode != 0`) references a live inode whose
> `mode` type is consistent with the dirent's `file_type`.

> **DIR-3.** A directory mutation is atomic: after a crash the directory is
> observed either fully before or fully after the operation, because it is
> finalized by a single superblock commit
> ([`03 §4`](03-superblock.md#4-commit-the-pointer-flip-protocol)). There is no
> in-place dirent edit that a crash could tear
> ([`01 §4.7`](01-overview.md#4-v1-post-mortem-why-rfs-v1-is-unretrofittable)).
