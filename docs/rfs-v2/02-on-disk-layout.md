# RFS V2 — On-Disk Layout

This document defines the physical device layout: the block model, the fixed
static region (static header + two superblock slots), the dynamic COW region,
and the `BlockPtr` — the single self-validating pointer type used by every
tree in the filesystem. Structures defined here are referenced, not
redefined, by later documents.

See [`01-overview.md`](01-overview.md) for goals and glossary,
[`03-superblock.md`](03-superblock.md) for the superblock contents,
[`08-encryption.md`](08-encryption.md) for how `BlockPtr.tag` is computed.

---

## 1. Block model

- **Block size:** 4096 bytes, fixed in V2. (`block_size` is recorded in the
  static header for forward-compat but no other value is defined —
  [`10`](10-format-and-compat.md).)
- **Sector size:** 512 bytes. **8 sectors = 1 block.**
- **Addressing:** all on-disk references are *block numbers* (`u64`), never
  byte or sector addresses, unless explicitly stated. Byte offset of block
  `N` on the device is `N * 4096`.
- **Block 0** is the plaintext static header. **Block number 0 is therefore
  never a valid COW target**, which is what lets it double as the null pointer
  sentinel (see [§4](#4-blockptr)).
- **Encryption unit = block.** Every block outside the plaintext static header
  is individually AES-256-GCM encrypted; the full 4096 bytes are ciphertext
  and the authentication tag lives *outside* the block, in the pointing
  `BlockPtr` ([§4](#4-blockptr)). The sole exception is a superblock slot,
  which stores its generation and tag in a plaintext trailer because it has no
  parent ([`03`](03-superblock.md)).

All integers are **little-endian**. All reserved bytes are zero on write and
ignored on read (but see feature-flag policy, [`10`](10-format-and-compat.md)).

---

## 2. Device region map

The device is divided into a small **fixed region** (blocks 0–2, at known
addresses, found without reading any pointer) and the **dynamic region**
(block 3 onward, entirely COW-managed, with no fixed assignment of purpose to
any address).

| Block | Region | Contents | Mutability |
|-------|--------|----------|------------|
| `0` | Static header | Geometry, KDF params, wrapped DEK. Plaintext. | Immutable after mkfs |
| `1` | Superblock slot A | A superblock (or stale/blank). Encrypted + self-tagged. | Overwritten in place, alternating with B |
| `2` | Superblock slot B | A superblock (or stale/blank). Encrypted + self-tagged. | Overwritten in place, alternating with A |
| `3 … total_blocks-1` | Dynamic (COW) region | Inode-map nodes, inodes, block-map nodes, directory blocks, file data — intermixed, no fixed layout. Encrypted, tag in parent. | Append-by-allocation; never mutated in place while live |

`first_data_block = 3` and the two slot addresses are recorded in the static
header ([§3](#3-static-header-block-0)) so a reader never hard-codes them, but
V2 fixes them to 1, 2, 3.

**The superblock slots (1, 2) are the only blocks written in place.** This is
safe precisely because a torn write to a slot is *detected* (GCM tag mismatch)
and the other slot survives — the crash-consistency argument in
[`03 §5`](03-superblock.md#5-crash-consistency-argument) and
[`09`](09-consistency.md) depends on it. Every other block is immutable once
written and made free only by mark-and-sweep
([`05`](05-space-management.md)).

### Alignment

- The static region occupies exactly 3 blocks (12 KiB) at the device origin.
- No sub-block structure straddles a block boundary. Inodes (128 B), index
  nodes (arrays of 32-B `BlockPtr`), and directory blocks are all sized and
  packed to fit whole within one 4096-B block.
- The device is expected to be block-aligned; a device whose size is not a
  multiple of 4096 has its trailing partial block ignored (`total_blocks =
  floor(device_bytes / 4096)`).

---

## 3. Static header (block 0)

Plaintext, immutable, written once by mkfs ([`10`](10-format-and-compat.md)),
read before any key material exists. It holds device geometry, the KDF
parameters needed to derive the KEK, and the wrapped DEK. It is **not
encrypted**; its integrity guarantees are described in
[`08 §static-header-authentication`](08-encryption.md#7-what-is-and-isnt-authenticated).

| Offset | Size | Field | Notes |
|--------|------|-------|-------|
| 0 | 8 | `magic` | `"RFS_V2\0\0"` (0x00 = `52 46 53 5F 56 32 00 00`) |
| 8 | 2 | `format_version` | On-disk format major version (= `2`) |
| 10 | 2 | `header_version` | Static-header layout revision (= `1`) |
| 12 | 4 | `block_size` | Bytes per block (= `4096`) |
| 16 | 8 | `total_blocks` | Device size in blocks |
| 24 | 8 | `sb_slot_a` | Superblock slot A block number (= `1`) |
| 32 | 8 | `sb_slot_b` | Superblock slot B block number (= `2`) |
| 40 | 8 | `first_data_block` | First allocatable dynamic block (= `3`) |
| 48 | 16 | `uuid` | Volume UUID (random at mkfs) |
| 64 | 8 | `feature_compat` | Compat feature bitmask ([`10`](10-format-and-compat.md)) |
| 72 | 8 | `feature_incompat` | Incompat feature bitmask |
| 80 | 8 | `feature_ro_compat` | Read-only-compat feature bitmask |
| 88 | 1 | `kdf_algo` | `1` = Argon2id (only value defined) |
| 89 | 7 | `reserved` | Zero |
| 96 | 16 | `kdf_salt` | Argon2id salt |
| 112 | 4 | `argon_m_cost` | Memory cost, KiB ([`08 §kdf`](08-encryption.md#5-key-derivation-argon2id)) |
| 116 | 4 | `argon_t_cost` | Time cost, iterations |
| 120 | 4 | `argon_p` | Parallelism, lanes |
| 124 | 4 | `reserved` | Zero |
| 128 | 12 | `dek_wrap_nonce` | GCM nonce for the DEK-wrap ([`08 §key-hierarchy`](08-encryption.md#6-key-hierarchy)) |
| 140 | 4 | `reserved` | Zero |
| 144 | 32 | `dek_wrapped` | AES-256-GCM ciphertext of the 32-byte DEK |
| 176 | 16 | `dek_wrap_tag` | GCM tag over the DEK-wrap (AAD binds the fields above — [`08`](08-encryption.md)) |
| 192 | 64 | `label` | UTF-8 volume label, NUL-padded |
| 256 | 3840 | `reserved` | Zero to end of block |

The DEK-wrap's AAD covers `magic ‖ format_version ‖ block_size ‖ total_blocks
‖ uuid ‖ feature_* ‖ kdf_algo ‖ kdf_salt ‖ argon_*` so that tampering any of
those plaintext fields is detected at unwrap time
([`08 §7`](08-encryption.md#7-what-is-and-isnt-authenticated)).

---

## 4. `BlockPtr` {#blockptr}

The single pointer type. Every reference from a parent structure to a child
block is a `BlockPtr`. It both **locates** the child (`block`) and
**authenticates** it (`gen` + `tag`), which is what makes the whole tree
self-validating.

**Size: 32 bytes.**

| Offset | Size | Field | Meaning |
|--------|------|-------|---------|
| 0 | 8 | `block` | Physical block number of the target. `0` = null pointer (no target). |
| 8 | 8 | `gen` | Generation in which the target block was written ([`01 §5`](01-overview.md#5-glossary)). Also the second half of the target's GCM nonce. |
| 16 | 16 | `tag` | AES-256-GCM authentication tag over the target block's full 4096-byte ciphertext, under nonce `block ‖ gen`. |

### Null pointer

`block == 0` denotes "no target." Because block 0 is the plaintext static
header and never a COW target, this is unambiguous. A null `BlockPtr` has
`gen = 0` and `tag = [0; 16]` by convention; readers test only `block == 0`.

### Reading through a `BlockPtr`

To dereference `p`:

1. If `p.block == 0`, the target is absent (hole / empty subtree). Stop.
2. Read the 4096-byte block at `p.block`.
3. Decrypt-and-verify with AES-256-GCM: key = DEK, nonce = `p.block ‖ p.gen`
   (16 bytes, [`08 §nonce`](08-encryption.md#4-nonce-construction)),
   expected tag = `p.tag`, AAD as specified in [`08`](08-encryption.md).
4. If authentication fails, the target is corrupt, tampered, replayed, or
   torn — return an I/O error; **never** return unverified plaintext
   ([`09`](09-consistency.md)).
5. Otherwise the decrypted 4096 bytes are the child structure.

### Writing a child and its pointer

When COW allocates a fresh block `b` for a child in commit `g`:

1. Encrypt the child's 4096-byte plaintext with nonce `b ‖ g`, producing
   ciphertext + a 16-byte tag `t`.
2. Write the ciphertext to block `b`.
3. Set the parent's `BlockPtr = { block: b, gen: g, tag: t }`.

The parent is itself a freshly COW-allocated block, so its pointer update is
part of writing the parent — never an in-place edit
([`04`](04-cow-and-commit.md)). The recursion bottoms out at the superblock,
whose tag is self-stored ([`03`](03-superblock.md)).

### Invariant (pointer/target agreement)

> For every non-null `BlockPtr p` reachable from a valid superblock, the block
> at `p.block` decrypts and authenticates under nonce `p.block ‖ p.gen` to
> exactly the tag `p.tag`. Any violation is a detected fault, not a state the
> reader tolerates.

---

## 5. What lives in the dynamic region

The dynamic region (block 3+) holds four kinds of encrypted block, all reached
only through `BlockPtr`s rooted at a superblock. None has a fixed address; each
is placed wherever the allocator ([`05`](05-space-management.md)) had a free
block at write time.

| Kind | Defined in | Fanout / packing |
|------|-----------|------------------|
| Inode-map index node | [`06 §inode-map`](06-inodes.md#3-the-inode-map) | 128 × `BlockPtr` |
| Inode-map leaf node | [`06 §inode-map`](06-inodes.md#3-the-inode-map) | 32 × 128-byte inode |
| Block-map index node | [`06 §block-map`](06-inodes.md#4-the-per-file-block-map) | 128 × `BlockPtr` |
| Directory block | [`07`](07-directories.md) | ext2-style dirents |
| File data block | — | raw file bytes (last block zero-padded to 4096 before encryption) |

Symlink target storage is resolved in [`06 §1`](06-inodes.md#symlinks):
targets ≤ 48 bytes are stored inline in the inode (fast symlink, no data
block); longer targets are ordinary file data reached through the block map.
