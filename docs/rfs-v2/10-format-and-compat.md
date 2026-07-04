# RFS V2 — Format Identification, Feature Flags, mkfs, and Compatibility

This document specifies how an RFS V2 volume is identified (magic + version),
how feature negotiation works (compat / incompat / ro_compat flag sets), the
parameters `mkfs` fixes at format time, and the forward/backward-compatibility
policy. It is the reference for tooling that must decide whether — and how — it
may touch a given volume.

Prerequisites: [`02 §3`](02-on-disk-layout.md#3-static-header-block-0) (static
header layout), [`08`](08-encryption.md) (KDF/key params written by mkfs).

---

## 1. Magic and version

Two magics, at two levels, both checked at mount:

| Magic | Location | Value |
|-------|----------|-------|
| Volume magic | static header offset 0 ([`02 §3`](02-on-disk-layout.md#3-static-header-block-0)) | `"RFS_V2\0\0"` |
| Superblock magic | superblock payload offset 0 ([`03 §2`](03-superblock.md#2-superblock-structure)) | `"RFSSB\0\0\0"` |

Versions in the static header:

- `format_version : u16` = `2` — the on-disk format major version. A reader
  that does not implement major version `2` refuses the volume.
- `header_version : u16` = `1` — the static-header *layout* revision, bumped if
  the header's own field layout changes (independently of the tree format).

Major-version is a hard gate; fine-grained capability negotiation is done by
feature flags ([§2](#2-feature-flags)), not by version bumps, so that a `2.x`
reader can make a safe decision about a volume using features it predates.

---

## 2. Feature flags {#2-feature-flags}

Three 64-bit bitmasks in the static header (offsets 64/72/80,
[`02 §3`](02-on-disk-layout.md#3-static-header-block-0)), following the
ext2/3/4 model. An implementation compares each mask against the set of
features it understands:

| Mask | If the volume sets a bit the reader does **not** understand |
|------|-------------------------------------------------------------|
| `feature_compat` | Mount normally (read-write). The unknown feature does not affect on-disk interpretation the reader relies on. |
| `feature_ro_compat` | Mount **read-only**. The reader can safely read, but writing could violate an invariant the feature maintains. |
| `feature_incompat` | **Refuse to mount.** The on-disk format cannot be correctly interpreted without the feature. |

### Reserved feature bits

Bit assignments are reserved here even where the feature is unspecified, so the
policy is stable. `(none defined = 0)` means a conformant V2.0 volume has all
three masks zero except as noted.

**`feature_incompat`:**

| Bit | Name | Meaning |
|-----|------|---------|
| 0 | `ENCRYPTION` | AES-256-GCM whole-volume encryption. **Always set in V2** — encryption is mandatory ([`08`](08-encryption.md)); a reader without crypto support cannot interpret any block. |
| 1 | `COMPRESSION` | Per-block compression. `TODO(open)` — reserved, not specified. |
| 2 | retired | Was `DIR_HASH_INDEX`, which is resolved to `feature_ro_compat` bit 1 ([`07 §4`](07-directories.md#4-large-directory-behavior)). Never reuse; must be zero. |
| 3–63 | reserved | Zero. |

**`feature_ro_compat`:**

| Bit | Name | Meaning |
|-----|------|---------|
| 0 | `HARDLINKS` | Multiple dirents may share one inode (`nlink > 1` meaningful). **Set at mkfs in V2.0** — hard links are baseline ([`07 §3`](07-directories.md#3-directory-operations)). A reader lacking hardlink-aware freeing can safely read but must not write (it could free an inode that other dirents still reference). |
| 1 | `DIR_HASH_INDEX` | Hashed directory index present ([`07 §4`](07-directories.md#4-large-directory-behavior)). Readers without index support can read via linear scan but cannot keep the index coherent → read-only. On-disk index format `TODO(open)`. |
| 2–63 | reserved | Zero. |

**`feature_compat`:**

| Bit | Name | Meaning |
|-----|------|---------|
| 0–63 | reserved | Zero. |

`DIR_HASH_INDEX`'s mask question is resolved: it is `ro_compat` (bit 1),
because the index is constrained to be an acceleration structure over a
complete linear dirent list — a non-indexed reader always has a correct
read path and only writing desynchronizes the index
([`07 §4`](07-directories.md#4-large-directory-behavior)).

---

## 3. mkfs parameters

`mkfs.rfs2` (host tool; cf. `tools/mkrfs` for V1) writes the static header,
generates keys, and lays down an empty root directory. Inputs:

| Parameter | Required | Default / notes |
|-----------|----------|-----------------|
| Target device / image | yes | Sized to `floor(bytes/4096)` blocks ([`02 §2`](02-on-disk-layout.md#2-device-region-map)) |
| Passphrase | yes | Feeds Argon2id → KEK ([`08 §5`](08-encryption.md#5-key-derivation-argon2id)) |
| `block_size` | no | Fixed 4096 in V2; not tunable |
| `argon_m_cost` / `argon_t_cost` / `argon_p` | no | Baseline 65536 KiB / 3 / 1 ([`08 §5`](08-encryption.md#5-key-derivation-argon2id)); tunable for hardware |
| `uuid` | no | Random if unspecified |
| `label` | no | Empty if unspecified (≤ 64 bytes) |

**mkfs actions:**

1. Generate a random 256-bit **DEK** and a random 16-byte `kdf_salt`
   ([`08 §CRYPTO-1`](08-encryption.md#4-nonce-construction): a fresh DEK per
   format guarantees a fresh nonce space).
2. Derive the **KEK** from the passphrase + salt + Argon2id params; wrap the
   DEK with AES-256-GCM (AAD = the header fields per
   [`08 §7`](08-encryption.md#7-what-is-and-isnt-authenticated)); write
   `dek_wrap_nonce`, `dek_wrapped`, `dek_wrap_tag`.
3. Write the **static header** (block 0): magic, versions, geometry, feature
   masks (`feature_incompat = ENCRYPTION`,
   `feature_ro_compat = HARDLINKS`), KDF params, wrapped DEK, uuid, label.
4. Create the **root directory** inode (inode 1) with `.`/`..`
   ([`07 §1`](07-directories.md#1-directory-entry-format)); build the initial
   inode-map tree containing it.
5. Write the **first superblock** with `gen = 1`, `inode_map_root` set,
   `next_inode = 10` (general-allocation start; 2–9 stay reserved —
   [`06 §3`](06-inodes.md#3-the-inode-map)), `inode_count = 1`, into
   **slot A** (block 1). Leave **slot B** (block 2) blank/invalid — the first
   commit after mount will populate it ([`03 §1`](03-superblock.md#1-slots)).
6. The first on-disk superblock is `gen = 1` (resolved); `gen = 0` is reserved
   to mean "no superblock" (matches null-`BlockPtr` `gen = 0` and a blank
   trailer `gen_copy`).

---

## 4. Compatibility policy

### Backward compatibility (newer reader, older volume)

- A reader of format major `2` mounts any `format_version == 2` volume subject
  to the feature-flag rules ([§2](#2-feature-flags)). Within major 2, new
  `_compat` features never prevent an older-but-still-major-2 reader from
  mounting read-write; new `_ro_compat` features drop it to read-only; new
  `_incompat` features block it.
- `header_version` bumps must remain readable by keeping field offsets stable
  and only appending into the reserved tail
  ([`02 §3`](02-on-disk-layout.md#3-static-header-block-0)); a reader ignores
  reserved bytes it does not recognize.

### Forward compatibility (older reader, newer volume)

- Achieved entirely through the three feature masks: a newer volume advertises
  what it uses, and an older reader makes a safe rw / ro / refuse decision
  without understanding the feature itself. This is the reason feature bits —
  not silent format changes — gate every optional capability.

### Across major versions

- `format_version != 2` is rejected. There is **no in-place V1→V2 upgrade**
  ([`01 §4`](01-overview.md#4-v1-post-mortem-why-rfs-v1-is-unretrofittable));
  migration is an offline copy (read V1, write a fresh V2 image), tracked as
  `TODO(open)` in [`01 §4`](01-overview.md#4-v1-post-mortem-why-rfs-v1-is-unretrofittable).
- `TODO(open):` a future format major `3` — its relationship to `2` (shared
  static-header prologue so a `2` reader can at least *identify* a `3` volume
  and report it cleanly) should be decided before the first incompatible tree
  change.

---

## 5. Conformance checklist

A conformant RFS V2.0 volume:

1. Block 0 magic `"RFS_V2\0\0"`, `format_version = 2`, `header_version = 1`,
   `block_size = 4096`.
2. `feature_incompat` has `ENCRYPTION` (bit 0) set; `feature_ro_compat` has
   `HARDLINKS` (bit 0) set; no unspecified bits set in any mask.
3. Slots at blocks 1 and 2; at least one holds a valid superblock with magic
   `"RFSSB\0\0\0"`, `gen ≥ 1`, and a plaintext trailer `gen_copy` equal to
   the payload `gen` ([`03 §2`](03-superblock.md#2-superblock-structure)).
4. Every block reachable from the current superblock authenticates under nonce
   `block ‖ gen` against its parent `BlockPtr.tag`
   ([`08 §3`](08-encryption.md#3-block-encryption)).
5. The inode map contains inode 1 (root directory) with valid `.`/`..`
   entries.
6. No persistent free-space structure exists anywhere on the device
   ([`05`](05-space-management.md)).
