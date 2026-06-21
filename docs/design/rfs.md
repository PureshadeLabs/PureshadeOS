# RFS — Raptor File System

RFS is the native filesystem for Lythos. It is a custom extent-based
filesystem designed for simplicity, correctness, and eventual scalability.
It does not use checksums (by design); it does support Unix permissions,
symbolic links, large files (64-bit sizes), and sparse files.

---

## On-disk layout

### Block size

4096 bytes (8 sectors of 512 bytes each). All offsets and sizes are in
bytes unless noted; all multi-byte integers are little-endian.

### Block map

| Block | Contents |
|-------|----------|
| 0 | Superblock |
| 1 | Block bitmap |
| 2–33 | Inode table (32 blocks) |
| 34+ | Data blocks |

### Superblock (block 0, first 64 bytes used)

| Offset | Size | Field | Value |
|--------|------|-------|-------|
| 0 | 8 | magic | `RFS_V1\0\0` (8 bytes) |
| 8 | 4 | version | 1 |
| 12 | 4 | block_size | 4096 |
| 16 | 4 | total_blocks | image size / 4096 |
| 20 | 4 | free_blocks | at format time |
| 24 | 4 | inode_count | 1024 |
| 28 | 4 | root_inode | 0 |
| 32 | 4 | bitmap_block | 1 |
| 36 | 4 | inode_start | 2 |
| 40 | 4 | inode_blocks | 32 |
| 44 | 4 | data_start | 34 |
| 48 | 16 | _pad | zeroed |

### Block bitmap (block 1)

One bit per block. Bit 0 of byte 0 = block 0. Bit set (1) = block used.
Blocks 0–33 (superblock + bitmap + inode table) are always set used.

4096 bytes × 8 bits = 32768 blocks addressable (128 MiB maximum image size).

### Inode table (blocks 2–33)

32 blocks × 32 inodes per block = **1024 inodes**. Each inode is 128 bytes.
Inode 0 is always the root directory.

---

## Inode format (128 bytes)

| Offset | Size | Field | Description |
|--------|------|-------|-------------|
| 0 | 4 | flags | Inode status and type flags (see below) |
| 4 | 2 | mode | Unix permission bits (low 12 bits of `stat.st_mode`) |
| 6 | 2 | _pad0 | Reserved, zeroed |
| 8 | 4 | uid | Owner user ID |
| 12 | 4 | gid | Owner group ID |
| 16 | 4 | nlink | Hard link count |
| 20 | 8 | size | File size in bytes (64-bit) |
| 28 | 8 | blocks | Number of 4096-byte blocks allocated |
| 36 | 8 | mtime | Modification time (Unix seconds, 64-bit) |
| 44 | 8 | ctime | Change time (Unix seconds, 64-bit) |
| 52 | 4 | ovfl_block | First overflow extent block (0 if none) |
| 56 | 2 | extent_count | Total number of extents (inline + overflow) |
| 58 | 2 | _pad1 | Reserved, zeroed |
| 60 | 64 | extents[4] | Four inline extents (16 bytes each) |
| 124 | 4 | _pad2 | Reserved, zeroed |

### Inode flags

| Bit | Constant | Meaning |
|-----|----------|---------|
| 0 | `INODE_USED` | Inode is allocated |
| 1 | `INODE_DIR` | Regular directory |
| 2 | `INODE_SYMLINK` | Symbolic link |
| 3 | `INODE_FAST_SYM` | Symlink target stored inline (≤64 bytes, in extents[]) |

An inode with `flags == 0` is free. A regular file has `INODE_USED` only.

### Mode field

The `mode` field stores the low 12 bits of a Unix `st_mode` value
(permission bits only, not the file type bits). Typical values:

| Value | Meaning |
|-------|---------|
| `0o755` | `rwxr-xr-x` (executable / directory) |
| `0o644` | `rw-r--r--` (regular file) |
| `0o777` | `rwxrwxrwx` |
| `0o120_777` | Symlink (upper bits set by OS, lower 12 = 0o777) |

---

## Extent format (16 bytes)

An extent describes a contiguous run of blocks in the file.

| Offset | Size | Field | Description |
|--------|------|-------|-------------|
| 0 | 4 | logical | First logical block in this extent (sparse-file offset) |
| 4 | 4 | physical | First physical block number |
| 8 | 4 | count | Number of contiguous blocks |
| 12 | 4 | flags | Reserved, zeroed |

**Sparse files:** a logical block range with no covering extent contains
implicit zeros. Reading it returns zeroes without allocating disk space.

**Lookup:** to find the physical block for logical block L, scan extents in
order (inline first, then overflow chain) and find the extent E where
`E.logical <= L < E.logical + E.count`. The physical block is
`E.physical + (L - E.logical)`.

---

## Overflow extent blocks

When a file has more than 4 extents, additional extents are stored in
overflow blocks linked from `inode.ovfl_block`.

### Overflow block layout

| Offset | Size | Field | Description |
|--------|------|-------|-------------|
| 0 | 4 | next | Next overflow block (0 = end of chain) |
| 4 | 4 | used | Number of extents stored in this block |
| 8 | 8 | _pad | Reserved, zeroed |
| 16 | 4080 | extents[255] | 255 × 16-byte extents |

With 4 inline + 255 per overflow block, a file with a single overflow block
supports up to 259 extents. The chain can grow arbitrarily.

---

## Directory entry format

Directories store variable-length entries, one or more blocks long.

### Entry layout

```
Offset  Size  Field
──────  ────  ──────────────────────────────────────────────
0       4     inode (u32) — inode number of the target file
4       2     rec_len (u16) — total byte length of this entry
                              (includes header + name + padding)
6       1     name_len (u8) — length of the name in bytes (1–255)
7       1     file_type (u8) — 1=regular, 2=directory, 3=symlink
8       N     name — UTF-8 bytes (not null-terminated)
8+N     P     padding — zeroes to align rec_len to 4 bytes
```

- Entries within a block are laid out sequentially.
- The last entry in a block has its `rec_len` extended to fill the
  remaining block space (so the next block starts a fresh entry).
- An entry with `inode == 0` is deleted (hole). Scanning must check this.

### Special entries

Every directory contains at minimum two entries:
- `.` — points to the directory's own inode
- `..` — points to the parent directory's inode (root has `..` pointing
  to itself)

These are the first two entries written, in that order.

---

## Fast symlinks

A symlink with a target of 64 bytes or fewer is stored inline in the inode's
`extents[]` field (bytes 60–123), rather than allocating a data block. The
`INODE_FAST_SYM` flag distinguishes inline from block-stored symlinks.

Inline target: read `inode.size` bytes from `inode.extents` as raw bytes.
Block-stored target: read `inode.size` bytes from the data blocks (using
the normal extent lookup), same as a regular file.

---

## mkrfs tool (`tools/mkrfs/`)

A host-side formatting tool that creates RFS disk images.

### Build

```
cd tools/mkrfs
make           # uses rustc directly (avoids kernel build-std config)
```

### Usage

```
./mkrfs <image> <size[K|M|G]> [<src-dir>]
```

- Creates a new image file, truncates it to `size`.
- If `src-dir` is given, recursively copies the directory tree into the
  image: regular files, subdirectories, and symlinks.
- If `src-dir` is omitted, creates an empty filesystem (root dir only).

### Examples

```
./mkrfs disk.img 64M              # empty 64 MiB image
./mkrfs disk.img 64M rootfs/      # populate from rootfs/
```

### Output

```
mkrfs: creating disk.img (67108864 bytes, 16384 blocks)
mkrfs: done. 16384 total blocks, 16315 free, 42 inodes used.
```

### Limitations

- Maximum image size: 128 MiB (32768-block bitmap).
- Maximum inodes: 1024 (fixed inode table size).
- No hardlinks (each file is assigned a fresh inode).
- Timestamps are taken from the host filesystem (via `stat.st_mtime`).

---

## Kernel RFS driver (`src/rfs.rs`)

Fully implemented. Capabilities:

1. **Mount** — validates superblock magic, records `total_blocks`.
2. **Block I/O** — `read_block` / `write_block` wrap `virtio_blk` (8 sectors per block).
3. **Inode read/write** — `read_inode` / `write_inode` with `serialize_inode`.
4. **Extent traversal** — inline (4) then overflow chain for logical → physical mapping.
5. **File read** — sparse-hole aware; zero-fills unmapped logical blocks.
6. **File write** — `append_to_file` allocates new blocks and extents as needed.
7. **Block/inode allocator** — bitmap-based `alloc_block` / `free_block`,
   scan-based `alloc_inode` / `free_inode`.
8. **Directory scan** — variable-length entry iteration; deleted-entry (ino=0) skipping.
9. **Directory write** — `add_dir_entry` reuses holes or appends/allocates blocks;
   `remove_dir_entry` soft-deletes by zeroing the inode field.
10. **Path resolution** — component walk with symlink following (max 8 hops).
11. **VFS interface** — `open` (read-only fd), `create` (writable fd), `read`, `write`,
    `close`, `stat_path`, `readdir_path`, `unlink`.

Syscall numbers: SYS_OPEN=22, SYS_READ=23, SYS_WRITE=24, SYS_CLOSE=25,
SYS_STAT=26, SYS_READDIR=27, SYS_CREATE=28, SYS_UNLINK=29.
