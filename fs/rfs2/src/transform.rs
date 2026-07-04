//! The crypto seam (doc 08).
//!
//! Every block that crosses the disk boundary is routed through a
//! [`BlockTransform`]. The real transform is AES-256-GCM under the volume DEK
//! with the 128-bit nonce `block (u64 LE) ‖ gen (u64 LE)` — **not implemented
//! here**. It is gated on the SP 800-38D known-answer test for non-96-bit
//! nonces (GHASH-derived `J₀`), per doc 08 §4 "Residual risk to close".
//!
//! TODO(crypto): once the KAT clears, implement `GcmTransform` against this
//! trait: AES-256-GCM (AES-NI), nonce = `block ‖ gen`, AAD as passed by the
//! call sites (already spec-exact), plus Argon2id KEK derivation and DEK
//! unwrap at mount (doc 08 §5–6). Swapping it in must require no change to any
//! call site, to `BlockPtr` layout, or to on-disk offsets — that is the
//! contract of this module.

use crate::{Error, Result};
use crate::layout::{
    rd_u64, wr_u64, SB_GEN_COPY_OFFSET, SB_MAGIC, SB_PAYLOAD_SIZE, SB_TAG_OFFSET,
};

pub const TAG_SIZE: usize = 16;
pub type Tag = [u8; TAG_SIZE];

/// AAD for a dynamic-region block: `block (u64 LE) ‖ gen (u64 LE)` (doc 08 §3).
pub fn dyn_aad(block: u64, gen: u64) -> [u8; 16] {
    let mut aad = [0u8; 16];
    wr_u64(&mut aad[0..], block);
    wr_u64(&mut aad[8..], gen);
    aad
}

/// AAD for a superblock slot: `sb_magic ‖ gen ‖ slot_block ‖ uuid`
/// (doc 03 §2, doc 08 §3). Binds the superblock to its slot and generation.
pub fn sb_aad(gen: u64, slot: u64, uuid: &[u8; 16]) -> [u8; 40] {
    let mut aad = [0u8; 40];
    aad[0..8].copy_from_slice(&SB_MAGIC);
    wr_u64(&mut aad[8..], gen);
    wr_u64(&mut aad[16..], slot);
    aad[24..40].copy_from_slice(uuid);
    aad
}

/// Pluggable block sealing/opening. `data` is transformed **in place**;
/// ciphertext length equals plaintext length (GCM has a stream core), so the
/// on-disk footprint is identical for every implementation.
pub trait BlockTransform {
    /// Seal `data` for physical block `block` written in generation `gen`.
    /// Returns the 16-byte authentication tag to be stored in the parent
    /// `BlockPtr` (or the superblock trailer).
    fn encrypt_block(&self, block: u64, gen: u64, aad: &[u8], data: &mut [u8]) -> Tag;

    /// Open `data` read from `block`, expected generation `gen`. Must verify
    /// `expected_tag` and fail with [`Error::Auth`] on any mismatch — a read
    /// never returns unverified plaintext (CRYPTO-2, doc 08 §7).
    fn decrypt_block(
        &self,
        block: u64,
        gen: u64,
        aad: &[u8],
        expected_tag: &Tag,
        data: &mut [u8],
    ) -> Result<()>;

    /// Seal a superblock slot: plaintext payload in `slot_buf[..4072]`;
    /// ciphertext plus the plaintext trailer (`gen_copy` + tag) out
    /// (doc 03 §2).
    fn seal_superblock(&self, slot: u64, gen: u64, uuid: &[u8; 16], slot_buf: &mut [u8]) {
        let aad = sb_aad(gen, slot, uuid);
        let tag = self.encrypt_block(slot, gen, &aad, &mut slot_buf[..SB_PAYLOAD_SIZE]);
        wr_u64(&mut slot_buf[SB_GEN_COPY_OFFSET..], gen);
        slot_buf[SB_TAG_OFFSET..SB_TAG_OFFSET + TAG_SIZE].copy_from_slice(&tag);
    }

    /// Open a superblock slot; on success `slot_buf[..4072]` is plaintext and
    /// the trailer generation is returned. The caller must additionally check
    /// that the decrypted payload's `gen` equals the returned value
    /// (doc 03 §2 validity).
    ///
    /// `gen` is read from the plaintext trailer `gen_copy` *before*
    /// decryption, so this default works unchanged for the future GCM
    /// transform (the former gen-before-decrypt gap is resolved in doc 03 §2:
    /// a tampered `gen_copy` feeds a wrong nonce/AAD and fails
    /// authentication).
    fn open_superblock(&self, slot: u64, uuid: &[u8; 16], slot_buf: &mut [u8]) -> Result<u64> {
        let gen = rd_u64(&slot_buf[SB_GEN_COPY_OFFSET..SB_GEN_COPY_OFFSET + 8]);
        let aad = sb_aad(gen, slot, uuid);
        let mut tag = [0u8; TAG_SIZE];
        tag.copy_from_slice(&slot_buf[SB_TAG_OFFSET..SB_TAG_OFFSET + TAG_SIZE]);
        self.decrypt_block(slot, gen, &aad, &tag, &mut slot_buf[..SB_PAYLOAD_SIZE])?;
        Ok(gen)
    }

    /// `feature_incompat` bits this transform provides. The GCM transform
    /// returns `INCOMPAT_ENCRYPTION`; identity returns 0, so an unencrypted
    /// volume is honestly labelled and an encrypted one is refused
    /// (see `fs::mount` and doc 10 §2).
    fn incompat_features(&self) -> u64 {
        0
    }
}

/// No-op transform: plaintext on disk, all tags zero.
///
/// Proves the seam: same call sites, same `BlockPtr` layout, same on-disk
/// offsets as the future GCM transform. Provides **no** confidentiality and no
/// torn-write/tamper detection (those are properties of the real transform);
/// `decrypt_block` still verifies the recorded tag is the zero tag so that
/// tag plumbing bugs surface immediately.
pub struct IdentityTransform;

impl BlockTransform for IdentityTransform {
    fn encrypt_block(&self, _block: u64, _gen: u64, _aad: &[u8], _data: &mut [u8]) -> Tag {
        [0u8; TAG_SIZE]
    }

    fn decrypt_block(
        &self,
        block: u64,
        _gen: u64,
        _aad: &[u8],
        expected_tag: &Tag,
        _data: &mut [u8],
    ) -> Result<()> {
        if expected_tag != &[0u8; TAG_SIZE] {
            return Err(Error::Auth { block });
        }
        Ok(())
    }
}
