# RFS V2 — Encryption, Authentication, and Key Hierarchy

RFS V2 encrypts and authenticates **every block** with AES-256-GCM (AES-NI
hardware accelerated). Confidentiality and tamper-detection are properties of
the normal read/write path, not an optional layer. AES-256-GCM was chosen over
a bare checksum (e.g. crc32c) because it delivers authenticated encryption —
confidentiality *and* integrity — in one hardware-accelerated pass, whereas a
checksum gives neither confidentiality nor cryptographic tamper resistance.

The authentication tag of a block lives in the pointer that references it — the
self-validating `BlockPtr` ([`02 §4`](02-on-disk-layout.md#blockptr)) — so
following a pointer both locates and verifies its target. The GCM nonce is
`block ‖ gen`, which the COW + generation design keeps globally unique.

Prerequisites: [`02`](02-on-disk-layout.md) (`BlockPtr`, static header),
[`03`](03-superblock.md) (superblock, `gen`),
[`05 §4`](05-space-management.md#4-allocation) (allocation ↔ nonce freshness).

---

## 1. Primitives

- **AES-256-GCM** for all block encryption and for wrapping the DEK. 256-bit
  keys, 128-bit tags, hardware AES-NI + CLMUL for GHASH.
- **Argon2id** for passphrase → key derivation ([§5](#5-key-derivation-argon2id)).
- No other cipher or MAC is defined. `TODO(open):` algorithm agility — the
  static header records `kdf_algo` but there is no cipher-suite selector for
  the block cipher yet; V2 hard-codes AES-256-GCM.

---

## 2. What is encrypted

| Block | Encrypted? | Tag location |
|-------|-----------|--------------|
| Static header (block 0) | **No** (plaintext geometry + KDF params) | DEK-wrap tag covers the wrapped DEK + AAD ([§7](#7-what-is-and-isnt-authenticated)) |
| Superblock slots (1, 2) | Yes (4072-B payload) | Self-stored in the 24-B plaintext slot trailer (`gen_copy` + tag, [§3](#3-block-encryption)) |
| All dynamic-region blocks (3+) | Yes (full 4096 B) | In the parent `BlockPtr.tag` ([§3](#3-block-encryption)) |

Everything except block 0 is ciphertext. Block 0 must be plaintext because it
holds the parameters needed to *derive* the key before any key exists.

---

## 3. Block encryption {#3-block-encryption}

### Dynamic-region blocks (data + all tree nodes)

For a block written to physical block number `b` in commit generation `g`:

- **Key:** the DEK ([§6](#6-key-hierarchy)).
- **Nonce:** `b ‖ g` ([§4](#4-nonce-construction)).
- **Plaintext:** the full 4096-byte block image.
- **AAD:** `b ‖ g` (binds the ciphertext to its intended location and
  generation; a block replayed to a different location or generation fails
  authentication even before the tag is checked against the parent).
- **Output:** 4096 bytes of ciphertext (GCM is a stream cipher core; ciphertext
  length = plaintext length) **written to block `b`**, plus a 16-byte tag `t`
  **stored in the parent's `BlockPtr = {b, g, t}`** ([`02 §4`](02-on-disk-layout.md#blockptr)).

Read is the inverse: decrypt block `b` under nonce `b ‖ g` and verify against
the parent's `t`; any mismatch is a hard I/O error, never tolerated
([`09`](09-consistency.md)).

Because the tag is carried by the *parent*, the child block stores no tag and
uses all 4096 bytes for content. The whole tree is thus a Merkle-like
structure: each pointer commits to its subtree's contents through the tag,
recursively, up to the superblock.

### Superblock slots

A superblock has no parent, so it stores its own tag:

- **Payload:** 4072 bytes ([`03 §2`](03-superblock.md#2-superblock-structure)).
- **Key:** DEK. **Nonce:** `slot_block ‖ gen`. **AAD:** `sb_magic ‖ gen ‖
  slot_block ‖ uuid`.
- **Trailer (plaintext):** `gen_copy` (u64) at offset 4072, then the 16-byte
  tag at offset 4080.

Because the nonce and AAD both include `gen`, the reader takes `gen` from the
plaintext `gen_copy` **before** decrypting, then verifies the decrypted
payload's `gen` field equals it ([`03 §2`](03-superblock.md#2-superblock-structure)).
`gen_copy` needs no separate MAC: it *is* the nonce/AAD input, so any tampering
of it fails authentication of the payload.

The AAD binds the superblock to its slot and generation, so it cannot be
authenticated if relocated to the other slot or presented at a different
generation ([`03 §SB-4`](03-superblock.md#6-invariants)).

---

## 4. Nonce construction {#4-nonce-construction}

The GCM nonce is the 16-byte little-endian concatenation

```
nonce = block (u64 LE)  ‖  gen (u64 LE)
```

where `block` is the physical block number the ciphertext is written to and
`gen` is the commit generation ([`01 §5`](01-overview.md#5-glossary)).

### Uniqueness argument (no nonce ever repeats under the DEK)

GCM security requires that a (key, nonce) pair never encrypts two different
messages. With a fixed DEK, this reduces to: **no `(block, gen)` pair is ever
reused.**

1. **Within a single commit `g`:** the COW write path allocates each fresh
   block exactly once and writes it exactly once
   ([`05 §4`](05-space-management.md#4-allocation),
   [`04 §2`](04-cow-and-commit.md#2-the-cow-write-path-single-modification)).
   No physical block is written twice in the same generation. Hence for fixed
   `g`, every `block` value is distinct.
2. **Across commits:** `gen` is strictly increasing and never repeats
   ([`03 §3`](03-superblock.md#3-generation-numbers)). If physical block `b` is
   reused by a later commit, that commit's generation `g' > g` for every prior
   generation `g` in which `b` was written. Hence the `gen` component differs.

Together: two writes to the same physical block always carry different `gen`;
two writes in the same generation always carry different `block`. Therefore
`(block, gen)` — and thus the nonce — is globally unique under the DEK. ∎

This is precisely why the nonce couples the *spatial* identity (block) with the
*temporal* identity (generation): neither alone is unique across the filesystem
lifetime, but together they are, for free, as a consequence of COW + monotonic
generations. No nonce counter needs to be persisted.

> **CRYPTO-1.** The invariants this rests on — a block written at most once per
> generation, and `gen` strictly monotonic and never reset — are load-bearing
> for confidentiality, not just tidiness. Violating either (e.g. resetting
> `gen` on reformat-in-place over old data under the *same* DEK) reuses a
> nonce and breaks GCM. mkfs therefore always generates a **fresh DEK**
> ([§6](#6-key-hierarchy)), so a reformat starts a new nonce space.

### Nonce width tradeoff: 128-bit `block ‖ gen` vs. 96-bit GCM {#nonce-width-tradeoff}

The fixed design decision is `nonce = block ‖ gen`, which is **128 bits** (two
`u64`s). NIST SP 800-38D defines AES-GCM's *native* nonce as **96 bits**. GCM
still accepts other lengths, but the two paths differ:

- **96-bit nonce:** used directly as the initial counter block `J₀` (the nonce
  followed by the 32-bit counter `1`). No pre-processing.
- **Non-96-bit nonce (our 128 bits):** `J₀` is instead computed by running the
  nonce through **GHASH** (`J₀ = GHASH_H(nonce ‖ 0…0 ‖ len(nonce))`). This adds
  one GHASH block of work per encryption and is the less-exercised path in most
  AES-GCM implementations.

The three options and their tradeoffs:

| Option | Uniqueness | Cost | Risk |
|--------|-----------|------|------|
| **A. `block ‖ gen`, 128-bit (current decision)** | Exact — the [§4](#4-nonce-construction) proof holds directly with no field narrowing. | One extra GHASH per block op. Negligible vs. the AES + data-GHASH already done per 4096-B block (< ~1% on AES-NI). | Uses GCM's non-96-bit path; must confirm the chosen crypto lib implements the GHASH-derived `J₀` correctly (some embedded libs only handle 96-bit). |
| **B. Pack `block` + `gen` into 96 bits** (e.g. 48 b block ‖ 48 b gen) | Conditional — needs `block < 2⁴⁸` (256 Ti blocks = 1 EiB device: fine) **and** `gen < 2⁴⁸` (281 T commits: ~9000 yr at 1 kHz — fine, but now a hard cap that must be enforced). | Fastest — native 96-bit path, no GHASH pre-step. | Silent nonce reuse if either field ever overflows its 48-bit budget. Turns two comfortably-`u64` quantities into bounded fields whose limits become load-bearing security invariants ([CRYPTO-1](#4-nonce-construction)). |
| **C. Derive 96-bit nonce = truncate/hash of `block ‖ gen`** | Probabilistic — a 96-bit hash of a 128-bit input admits birthday collisions (~2⁴⁸ writes before a likely collision). | Native 96-bit path + one hash. | Trades the *exact* uniqueness guarantee for a statistical one, discarding the whole point of the [§4](#4-nonce-construction) proof. Rejected on that ground. |

**Why A is the default.** The design's headline property is *exact,
proof-backed* nonce uniqueness derived for free from COW + monotonic `gen`
([§4](#4-nonce-construction)). Option A preserves that proof unchanged; B and C
both convert a proof into a bounded assumption (B) or a probability (C). The
only cost A pays is one extra GHASH invocation per block — negligible against
the per-4096-B-block AES-CTR encryption and payload GHASH that already run on
AES-NI/CLMUL. Correctness and auditability outweigh a sub-1% throughput
difference for a filesystem.

**Residual risk to close.** A's viability depends on the block-crypto
implementation correctly supporting non-96-bit nonces (the GHASH-derived `J₀`).
Some hardware-oriented AES-GCM libraries fast-path *only* 96-bit nonces and
either reject or mishandle other lengths.

`TODO(open):` verify the selected AES-256-GCM implementation (kernel crypto
module / chosen crate) computes `J₀` correctly for 128-bit nonces, with a KAT
(known-answer test) against SP 800-38D vectors. If it does not, fall back to
Option B (48 b ‖ 48 b) and add explicit overflow guards on `block` and `gen`
rather than adopting Option C. Until verified, this is the one crypto
correctness item gating implementation.

---

## 5. Key derivation (Argon2id) {#5-key-derivation-argon2id}

The KEK is derived from the user passphrase with **Argon2id**:

```
KEK = Argon2id(pass = passphrase,
               salt = kdf_salt,          // 16 B, static header
               m    = argon_m_cost,      // KiB
               t    = argon_t_cost,      // iterations
               p    = argon_p,           // lanes
               out  = 32 bytes)          // 256-bit KEK
```

All four parameters + salt are stored in the plaintext static header
([`02 §3`](02-on-disk-layout.md#3-static-header-block-0)) and are authenticated
as AAD of the DEK-wrap ([§7](#7-what-is-and-isnt-authenticated)).

**Baseline parameters** (`TODO(open):` finalize after benchmarking on target
hardware):

| Param | Baseline | Rationale |
|-------|----------|-----------|
| `argon_m_cost` | 65 536 (64 MiB) | Memory-hardness vs. a small-RAM boot environment |
| `argon_t_cost` | 3 | Iterations |
| `argon_p` | 1 | Single lane (bootstrap simplicity) |
| `kdf_salt` | 16 random bytes | Unique per volume, from mkfs |

Argon2id (hybrid) is chosen for resistance to both GPU/ASIC (data-independent
first pass) and side-channel (data-dependent later passes) attacks. There is
**no TPM** and no sealed storage ([`01 §2`](01-overview.md#2-non-goals)); the
passphrase is the sole secret.

---

## 6. Key hierarchy {#6-key-hierarchy}

```
passphrase
   │  Argon2id(salt, m, t, p)              [§5]
   ▼
KEK  (256-bit, never stored)
   │  AES-256-GCM unwrap
   │    key   = KEK
   │    nonce = dek_wrap_nonce             (static header, offset 128)
   │    ct    = dek_wrapped                (static header, offset 144)
   │    tag   = dek_wrap_tag               (static header, offset 176)
   │    AAD   = magic ‖ version ‖ block_size ‖ total_blocks ‖ uuid
   │            ‖ feature_* ‖ kdf_algo ‖ kdf_salt ‖ argon_*     [§7]
   ▼
DEK  (256-bit, random at mkfs, wrapped in static header)
   │  AES-256-GCM per block
   │    nonce = block ‖ gen                [§4]
   ▼
every superblock, tree node, directory block, and data block
```

- **DEK** (Data-Encryption Key): random 256 bits generated at mkfs; encrypts
  every block for the life of the volume. Stored only in wrapped form.
- **KEK** (Key-Encryption Key): derived from the passphrase on each mount;
  wraps/unwraps the DEK. Never persisted.

**Mount:** read static header → derive KEK from passphrase + stored params →
GCM-unwrap the DEK (AAD = the header fields). If unwrap authentication fails,
the passphrase is wrong **or** the header was tampered → mount denied
([`01 §3`](01-overview.md#3-threat-model)). On success, the DEK decrypts the
two superblock slots and the rest of the tree.

**Passphrase change** re-derives a new KEK and re-wraps the *same* DEK (so bulk
data need not be re-encrypted); only the static header's wrap fields + salt
change. `TODO(open):` because the static header is written in place, a
passphrase change is the one post-mkfs in-place write outside the superblock
slots — its crash-safety (e.g. a second header copy, or write-new-then-swap)
is unspecified. DEK rotation (which *would* require rewriting all blocks under
new nonces/`gen`) is out of scope.

---

## 7. What is and isn't authenticated {#7-what-is-and-isnt-authenticated}

**Authenticated:**

- Every dynamic-region block, via its parent `BlockPtr.tag` — recursively up to
  the superblock, forming a hash-tree: tampering any block invalidates its tag,
  which would require changing the parent, and so on to the root
  ([`01 §3`](01-overview.md#3-threat-model)).
- Each superblock, via its self-stored tag, bound by AAD to its slot and `gen`
  ([§3](#3-block-encryption)).
- The **DEK** and the **KDF/geometry parameters**, via the DEK-wrap tag whose
  AAD covers those plaintext header fields. Flipping `total_blocks`,
  `kdf_salt`, `argon_*`, `uuid`, or `feature_*` in the header makes the unwrap
  fail → tampering is detected at mount.

**Not authenticated (accepted exposure):**

- The **plaintext static header bytes themselves are not confidential**: an
  offline attacker reads geometry, KDF params, and the volume label. This is by
  design — they are needed before the key exists.
- Header fields **not** in the DEK-wrap AAD (currently `label`, `reserved`) are
  neither confidential nor integrity-protected; tampering them cannot affect
  security, only cosmetics. `TODO(open):` decide whether `label` should be in
  the AAD.
- **Whole-device rollback** is not prevented — an attacker who restores an
  older full-device snapshot presents a self-consistent, correctly
  authenticated older filesystem. Without a TPM/external monotonic counter the
  FS cannot prove its `gen` is the newest ever
  ([`01 §3`](01-overview.md#3-threat-model)). Within a mounted session `gen`
  only moves forward.
- **Access patterns / sizes:** block-granular access patterns, file sizes
  rounded to blocks, and tree shape are observable to an offline or
  block-layer observer.

> **CRYPTO-2.** A read never returns unverified plaintext. Decryption and tag
> verification are inseparable; a verification failure propagates as an I/O
> error to the caller ([`09`](09-consistency.md)), and the affected subtree is
> treated as unreadable rather than guessed.
