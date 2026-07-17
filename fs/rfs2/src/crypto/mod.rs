//! Crypto module (doc 08).
//!
//! Split so the plaintext-only build path (`tools/mkrfs2`, which compiles this
//! crate with bare `rustc` and no cargo features) needs neither `aes-gcm` nor
//! `argon2`: the pure-data [`CryptoParams`] and the size constants live here,
//! ungated, while every item that pulls the RustCrypto crates lives in the
//! `crypto` feature-gated [`imp`] submodule. `MkfsOptions.crypto` refers only
//! to [`CryptoParams`], so formatting a plaintext volume never touches crypto.

use crate::transform::TAG_SIZE;

/// KEK/DEK size in bytes (256-bit).
pub const KEY_SIZE: usize = 32;
/// DEK-wrap nonce size in bytes (native GCM, 96-bit).
pub const WRAP_NONCE_SIZE: usize = 12;

/// The plaintext static-header crypto fields written at format time for an
/// **encrypted** volume (doc 08 §5–6). The caller (kernel / mkrfs2) generates a
/// random DEK + salt + wrap nonce, derives the KEK from the passphrase, wraps
/// the DEK with [`imp::wrap_dek`], and hands the resulting fields here; `mkfs`
/// copies them verbatim into block 0. `None` on `MkfsOptions` ⇒ a plaintext
/// volume. Pure data — no crypto-crate dependency, so it is always available.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CryptoParams {
    /// KDF identifier (1 = Argon2id). Recorded plaintext, authenticated as
    /// DEK-wrap AAD.
    pub kdf_algo: u8,
    pub kdf_salt: [u8; 16],
    pub argon_m_cost: u32,
    pub argon_t_cost: u32,
    pub argon_p: u32,
    pub dek_wrap_nonce: [u8; WRAP_NONCE_SIZE],
    pub dek_wrapped: [u8; KEY_SIZE],
    pub dek_wrap_tag: [u8; TAG_SIZE],
}

#[cfg(feature = "crypto")]
mod imp;
#[cfg(feature = "crypto")]
pub use imp::{
    argon2_block_count, derive_kek, open_dek, seal_dek, unwrap_dek, wrap_dek, Argon2Block,
    GcmTransform, WrapGeometry,
};

#[cfg(all(test, feature = "crypto"))]
mod tests;
