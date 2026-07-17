//! AES-256-GCM block transform + Argon2id key hierarchy (doc 08).
//!
//! This is the real implementation behind the [`crate::transform::BlockTransform`]
//! seam — [`GcmTransform`] plugs in exactly where [`crate::transform::IdentityTransform`]
//! does, with no change to any call site, `BlockPtr` layout, or on-disk offset
//! (the seam's contract). Ciphertext length equals plaintext length (GCM stream
//! core), so the disk footprint is byte-identical to the identity path except
//! that tags are now real.
//!
//! Key hierarchy (doc 08 §6):
//!
//! ```text
//! passphrase --Argon2id(salt,m,t,p)--> KEK --AES-256-GCM unwrap--> DEK
//!                                                                    |
//!                                        AES-256-GCM per block (nonce=block‖gen)
//! ```
//!
//! - [`derive_kek`] runs Argon2id against a **caller-provided** memory buffer
//!   (the kernel hands it a dedicated PMM-backed 64 MiB region via the direct
//!   map; the host hands it a `Vec`). Argon2's memory never touches the heap.
//! - [`unwrap_dek`] / [`wrap_dek`] GCM-(un)wrap the DEK under the KEK, AAD-bound
//!   to the plaintext static-header fields (doc 08 §7): tampering geometry or
//!   KDF params fails the unwrap → mount denied.
//! - [`GcmTransform`] seals/opens every dynamic block and superblock slot.

use aes_gcm::aead::consts::{U12, U16};
use aes_gcm::aead::generic_array::GenericArray;
use aes_gcm::aead::AeadInPlace;
use aes_gcm::aes::Aes256;
use aes_gcm::{AesGcm, KeyInit};

use alloc::vec::Vec;

pub use argon2::Block as Argon2Block;
use argon2::{Algorithm, Argon2, Params, Version};

use crate::layout::{StaticHeader, INCOMPAT_ENCRYPTION, MAGIC};
use crate::transform::{BlockTransform, Tag, TAG_SIZE};
use crate::{Error, Result};

// Pure-data types + size constants live in the parent (ungated) module so the
// plaintext build path never needs the crypto crates.
use super::{CryptoParams, KEY_SIZE, WRAP_NONCE_SIZE};

/// AES-256-GCM with a **128-bit** nonce (`block ‖ gen`, doc 08 §4 Option A) and
/// a 128-bit tag. The non-96-bit nonce engages GCM's GHASH-derived `J₀` path;
/// [`tests`](crate::crypto::tests) gates that against a NIST SP 800-38D KAT.
type BlockCipher = AesGcm<Aes256, U16>;

/// AES-256-GCM with the native 96-bit nonce, used only to wrap/unwrap the DEK
/// in the static header (doc 08 §6). `dek_wrap_nonce` is 12 bytes.
type WrapCipher = AesGcm<Aes256, U12>;

// ── Block transform (doc 08 §3) ──────────────────────────────────────────────

/// The real block sealer: AES-256-GCM under the volume DEK, nonce `block ‖ gen`.
pub struct GcmTransform {
    cipher: BlockCipher,
}

impl GcmTransform {
    /// Build a transform bound to `dek`. The DEK is copied into the cipher's
    /// expanded key schedule and the caller's slice can be zeroized after.
    pub fn new(dek: &[u8; KEY_SIZE]) -> Self {
        GcmTransform {
            cipher: BlockCipher::new(GenericArray::from_slice(dek)),
        }
    }

    /// The 128-bit GCM nonce for a block written to physical `block` in
    /// generation `gen`: `block (u64 LE) ‖ gen (u64 LE)` (doc 08 §4).
    fn nonce(block: u64, gen: u64) -> [u8; 16] {
        let mut n = [0u8; 16];
        n[0..8].copy_from_slice(&block.to_le_bytes());
        n[8..16].copy_from_slice(&gen.to_le_bytes());
        n
    }
}

impl BlockTransform for GcmTransform {
    fn encrypt_block(&self, block: u64, gen: u64, aad: &[u8], data: &mut [u8]) -> Tag {
        let nonce = Self::nonce(block, gen);
        // In-place: `data` holds ciphertext on return, same length. The only
        // error path is a >~64 GiB message; a 4096-byte block never hits it.
        let tag = self
            .cipher
            .encrypt_in_place_detached(GenericArray::from_slice(&nonce), aad, data)
            .expect("GCM encrypt of a bounded block cannot fail");
        let mut out = [0u8; TAG_SIZE];
        out.copy_from_slice(tag.as_slice());
        out
    }

    fn decrypt_block(
        &self,
        block: u64,
        gen: u64,
        aad: &[u8],
        expected_tag: &Tag,
        data: &mut [u8],
    ) -> Result<()> {
        let nonce = Self::nonce(block, gen);
        // Decrypt-and-verify are inseparable (CRYPTO-2, doc 08 §7): on any tag
        // mismatch `data` is left indeterminate and we return Auth, never
        // unverified plaintext.
        self.cipher
            .decrypt_in_place_detached(
                GenericArray::from_slice(&nonce),
                aad,
                data,
                GenericArray::from_slice(expected_tag),
            )
            .map_err(|_| Error::Auth { block })
    }

    fn incompat_features(&self) -> u64 {
        INCOMPAT_ENCRYPTION
    }
}

// ── Key derivation (doc 08 §5) ───────────────────────────────────────────────

/// Number of 1 KiB Argon2 blocks a run with these params needs. The caller
/// sizes its memory buffer from this (kernel: PMM frames; host: `Vec`).
pub fn argon2_block_count(m_cost: u32, t_cost: u32, p: u32) -> Result<usize> {
    let params = Params::new(m_cost, t_cost, p, Some(KEY_SIZE)).map_err(|_| Error::Invalid)?;
    Ok(params.block_count())
}

/// Derive the 256-bit KEK from `passphrase` with Argon2id, using the
/// caller-provided `memory` (must be at least [`argon2_block_count`] blocks —
/// checked; too small is [`Error::Invalid`], never a silent truncation).
///
/// The memory buffer holds passphrase-derived state; the caller must zeroize
/// and (kernel) wipe-on-free it after this returns (doc 08 §6, key hygiene).
pub fn derive_kek(
    passphrase: &[u8],
    salt: &[u8; 16],
    m_cost: u32,
    t_cost: u32,
    p: u32,
    memory: &mut [Argon2Block],
    out: &mut [u8; KEY_SIZE],
) -> Result<()> {
    let params = Params::new(m_cost, t_cost, p, Some(KEY_SIZE)).map_err(|_| Error::Invalid)?;
    if memory.len() < params.block_count() {
        return Err(Error::Invalid);
    }
    let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    argon
        .hash_password_into_with_memory(passphrase, salt, out, memory)
        .map_err(|_| Error::Invalid)
}

// ── DEK wrap / unwrap (doc 08 §6–7) ──────────────────────────────────────────

/// AAD binding the DEK-wrap to the plaintext static-header fields (doc 08 §7):
/// `magic ‖ format_ver ‖ header_ver ‖ block_size ‖ total_blocks ‖ uuid ‖
/// feature_compat ‖ feature_incompat ‖ feature_ro_compat ‖ kdf_algo ‖ kdf_salt
/// ‖ argon_m ‖ argon_t ‖ argon_p`. Flipping any of these in the header makes
/// the unwrap fail at mount. `label` is deliberately excluded (cosmetic;
/// doc 08 §7 open item).
fn dek_wrap_aad(h: &StaticHeader) -> Vec<u8> {
    let mut aad = Vec::with_capacity(128);
    aad.extend_from_slice(&MAGIC);
    aad.extend_from_slice(&h.format_version.to_le_bytes());
    aad.extend_from_slice(&h.header_version.to_le_bytes());
    aad.extend_from_slice(&h.block_size.to_le_bytes());
    aad.extend_from_slice(&h.total_blocks.to_le_bytes());
    aad.extend_from_slice(&h.uuid);
    aad.extend_from_slice(&h.feature_compat.to_le_bytes());
    aad.extend_from_slice(&h.feature_incompat.to_le_bytes());
    aad.extend_from_slice(&h.feature_ro_compat.to_le_bytes());
    aad.push(h.kdf_algo);
    aad.extend_from_slice(&h.kdf_salt);
    aad.extend_from_slice(&h.argon_m_cost.to_le_bytes());
    aad.extend_from_slice(&h.argon_t_cost.to_le_bytes());
    aad.extend_from_slice(&h.argon_p.to_le_bytes());
    aad
}

/// Wrap `dek` under `kek` for storage in the static header. Returns
/// `(wrapped_ciphertext, tag)`; the caller writes these plus `nonce` into
/// `dek_wrapped` / `dek_wrap_tag` / `dek_wrap_nonce`. `h` must already carry
/// the final geometry + KDF fields (they are the AAD).
pub fn wrap_dek(
    kek: &[u8; KEY_SIZE],
    dek: &[u8; KEY_SIZE],
    nonce: &[u8; WRAP_NONCE_SIZE],
    h: &StaticHeader,
) -> ([u8; KEY_SIZE], [u8; TAG_SIZE]) {
    let cipher = WrapCipher::new(GenericArray::from_slice(kek));
    let aad = dek_wrap_aad(h);
    let mut buf = *dek;
    let tag = cipher
        .encrypt_in_place_detached(GenericArray::from_slice(nonce), &aad, &mut buf)
        .expect("GCM wrap of a 32-byte DEK cannot fail");
    let mut tag_out = [0u8; TAG_SIZE];
    tag_out.copy_from_slice(tag.as_slice());
    (buf, tag_out)
}

/// Unwrap the DEK from the static header under `kek` (derived from the
/// passphrase). [`Error::Auth`] on any tag mismatch — a wrong passphrase **or**
/// a tampered header (doc 08 §6). The unwrapped DEK never touches disk.
pub fn unwrap_dek(kek: &[u8; KEY_SIZE], h: &StaticHeader) -> Result<[u8; KEY_SIZE]> {
    let cipher = WrapCipher::new(GenericArray::from_slice(kek));
    let aad = dek_wrap_aad(h);
    let mut buf = h.dek_wrapped;
    cipher
        .decrypt_in_place_detached(
            GenericArray::from_slice(&h.dek_wrap_nonce),
            &aad,
            &mut buf,
            GenericArray::from_slice(&h.dek_wrap_tag),
        )
        .map_err(|_| Error::Auth { block: 0 })?;
    Ok(buf)
}

// ── High-level volume key ceremony (used by kernel / mkrfs2 / tests) ─────────

/// Header geometry + feature fields that participate in the DEK-wrap AAD
/// (doc 08 §7). At seal time the caller must pass **exactly** what `mkfs` will
/// write into block 0, or the mount-time unwrap AAD won't match. The three
/// version/`block_size` fields are fixed constants and filled internally.
pub struct WrapGeometry {
    pub total_blocks: u64,
    pub uuid: [u8; 16],
    pub feature_compat: u64,
    pub feature_incompat: u64,
    pub feature_ro_compat: u64,
}

fn mirror_header(
    geom: &WrapGeometry,
    kdf_salt: [u8; 16],
    m: u32,
    t: u32,
    p: u32,
    dek_wrap_nonce: [u8; WRAP_NONCE_SIZE],
    dek_wrapped: [u8; KEY_SIZE],
    dek_wrap_tag: [u8; TAG_SIZE],
) -> StaticHeader {
    use crate::layout::{
        BLOCK_SIZE, FIRST_DATA_BLOCK, FORMAT_VERSION, HEADER_VERSION, SB_SLOT_A, SB_SLOT_B,
    };
    StaticHeader {
        format_version: FORMAT_VERSION,
        header_version: HEADER_VERSION,
        block_size: BLOCK_SIZE as u32,
        total_blocks: geom.total_blocks,
        sb_slot_a: SB_SLOT_A,
        sb_slot_b: SB_SLOT_B,
        first_data_block: FIRST_DATA_BLOCK,
        uuid: geom.uuid,
        feature_compat: geom.feature_compat,
        feature_incompat: geom.feature_incompat,
        feature_ro_compat: geom.feature_ro_compat,
        kdf_algo: 1,
        kdf_salt,
        argon_m_cost: m,
        argon_t_cost: t,
        argon_p: p,
        dek_wrap_nonce,
        dek_wrapped,
        dek_wrap_tag,
        label: [0; 64],
    }
}

/// mkfs-side ceremony: derive the KEK from `passphrase`, wrap `dek`, and return
/// the [`CryptoParams`] to hand to `MkfsOptions`. `memory` is the caller-owned
/// Argon2 buffer (kernel PMM / host `Vec`); zeroize it after. `geom` must match
/// what `mkfs` writes (see [`WrapGeometry`]).
#[allow(clippy::too_many_arguments)]
pub fn seal_dek(
    passphrase: &[u8],
    dek: &[u8; KEY_SIZE],
    geom: &WrapGeometry,
    kdf_salt: &[u8; 16],
    m: u32,
    t: u32,
    p: u32,
    dek_wrap_nonce: &[u8; WRAP_NONCE_SIZE],
    memory: &mut [Argon2Block],
) -> Result<CryptoParams> {
    let mut kek = [0u8; KEY_SIZE];
    derive_kek(passphrase, kdf_salt, m, t, p, memory, &mut kek)?;
    let h = mirror_header(geom, *kdf_salt, m, t, p, *dek_wrap_nonce, [0; KEY_SIZE], [0; TAG_SIZE]);
    let (dek_wrapped, dek_wrap_tag) = wrap_dek(&kek, dek, dek_wrap_nonce, &h);
    kek.iter_mut().for_each(|b| *b = 0);
    Ok(CryptoParams {
        kdf_algo: 1,
        kdf_salt: *kdf_salt,
        argon_m_cost: m,
        argon_t_cost: t,
        argon_p: p,
        dek_wrap_nonce: *dek_wrap_nonce,
        dek_wrapped,
        dek_wrap_tag,
    })
}

/// mount-side ceremony: derive the KEK from `passphrase` + the header's stored
/// KDF params, unwrap the DEK. [`Error::Auth`] ⇒ wrong passphrase or tampered
/// header. `memory` sized per [`argon2_block_count`] on the header's params.
pub fn open_dek(
    passphrase: &[u8],
    header: &StaticHeader,
    memory: &mut [Argon2Block],
) -> Result<[u8; KEY_SIZE]> {
    let mut kek = [0u8; KEY_SIZE];
    derive_kek(
        passphrase,
        &header.kdf_salt,
        header.argon_m_cost,
        header.argon_t_cost,
        header.argon_p,
        memory,
        &mut kek,
    )?;
    let dek = unwrap_dek(&kek, header);
    kek.iter_mut().for_each(|b| *b = 0);
    dek
}
