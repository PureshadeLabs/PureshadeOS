//! Crypto tests, incl. the doc 08 §4 gate: a known-answer test proving the
//! `aes-gcm` crate computes GCM's `J₀` correctly for a **non-96-bit** nonce
//! (the GHASH-derived path our 128-bit `block‖gen` nonce depends on).

extern crate std;
use std::vec::Vec;

use aes_gcm::aead::consts::U60;
use aes_gcm::aead::generic_array::GenericArray;
use aes_gcm::aead::AeadInPlace;
use aes_gcm::aes::Aes256;
use aes_gcm::{AesGcm, KeyInit};

use super::*;
use crate::layout::{
    StaticHeader, BLOCK_SIZE, FORMAT_VERSION, HEADER_VERSION, INCOMPAT_ENCRYPTION,
};
use crate::transform::BlockTransform;
use crate::Error;

fn hx(s: &str) -> Vec<u8> {
    let s: Vec<u8> = s.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    assert!(s.len() % 2 == 0);
    s.chunks(2)
        .map(|c| {
            let h = |b: u8| (b as char).to_digit(16).unwrap() as u8;
            (h(c[0]) << 4) | h(c[1])
        })
        .collect()
}

/// **KAT gate (doc 08 §4 "Residual risk to close").** NIST/McGrew GCM spec
/// Test Case 18: AES-256-GCM with a **60-byte IV** (non-96-bit → `J₀` via
/// GHASH). If `aes-gcm` fast-paths only 96-bit nonces or mishandles the GHASH
/// `J₀`, this fails and encryption must fall back to Option B (48b‖48b, 96-bit).
/// A passing KAT clears `block‖gen` (128-bit) for production use.
#[test]
fn kat_tc18_non96bit_nonce() {
    let k = hx("feffe9928665731c6d6a8f9467308308feffe9928665731c6d6a8f9467308308");
    let iv = hx(
        "9313225df88406e555909c5aff5269aa6a7a9538534f7da1e4c303d2a318a728\
         c3c0c95156809539fcf0e2429a6b525416aedbf5a0de6a57a637b39b",
    );
    let p = hx(
        "d9313225f88406e5a55909c5aff5269a86a7a9531534f7da2e4c303d8a318a72\
         1c3c0c95956809532fcf0e2449a6b525b16aedf5aa0de657ba637b39",
    );
    let a = hx("feedfacedeadbeeffeedfacedeadbeefabaddad2");
    let want_c = hx(
        "5a8def2f0c9e53f1f75d7853659e2a20eeb2b22aafde6419a058ab4f6f746bf4\
         0fc0c3b780f244452da3ebf1c5d82cdea2418997200ef82e44ae7e3f",
    );
    let want_t = hx("a44a8266ee1c8eb0c8b5d4cf5ae9f19a");

    assert_eq!(iv.len(), 60, "TC18 IV is 60 bytes (non-96-bit path)");
    let cipher = AesGcm::<Aes256, U60>::new(GenericArray::from_slice(&k));
    let mut buf = p.clone();
    let tag = cipher
        .encrypt_in_place_detached(GenericArray::from_slice(&iv), &a, &mut buf)
        .unwrap();
    assert_eq!(buf, want_c, "ciphertext mismatch — J₀/GHASH path wrong");
    assert_eq!(tag.as_slice(), &want_t[..], "tag mismatch — J₀/GHASH path wrong");
}

// ── GcmTransform block path ──────────────────────────────────────────────────

fn dek() -> [u8; 32] {
    let mut d = [0u8; 32];
    for (i, b) in d.iter_mut().enumerate() {
        *b = (i as u8).wrapping_mul(7).wrapping_add(3);
    }
    d
}

#[test]
fn block_roundtrip() {
    let x = GcmTransform::new(&dek());
    let plain = {
        let mut v = alloc::vec![0u8; BLOCK_SIZE];
        for (i, b) in v.iter_mut().enumerate() {
            *b = (i % 251) as u8;
        }
        v
    };
    let (block, gen) = (42u64, 7u64);
    let aad = crate::transform::dyn_aad(block, gen);
    let mut buf = plain.clone();
    let tag = x.encrypt_block(block, gen, &aad, &mut buf);
    assert_ne!(buf, plain, "ciphertext must differ from plaintext");
    x.decrypt_block(block, gen, &aad, &tag, &mut buf).unwrap();
    assert_eq!(buf, plain, "roundtrip must recover plaintext");
}

#[test]
fn block_tamper_is_auth_error() {
    let x = GcmTransform::new(&dek());
    let (block, gen) = (9u64, 3u64);
    let aad = crate::transform::dyn_aad(block, gen);
    let mut buf = alloc::vec![0xABu8; BLOCK_SIZE];
    let tag = x.encrypt_block(block, gen, &aad, &mut buf);
    buf[100] ^= 0x01; // flip one ciphertext byte
    let e = x.decrypt_block(block, gen, &aad, &tag, &mut buf);
    assert_eq!(e, Err(Error::Auth { block }), "tamper must be a loud Auth error");
}

#[test]
fn block_wrong_nonce_fails() {
    // Decrypting block b/gen g with the wrong (block, gen) — the nonce-reuse
    // guard's dual: a relocated or replayed block fails authentication.
    let x = GcmTransform::new(&dek());
    let aad = crate::transform::dyn_aad(9, 3);
    let mut buf = alloc::vec![0x5Au8; BLOCK_SIZE];
    let tag = x.encrypt_block(9, 3, &aad, &mut buf);
    assert!(x.decrypt_block(9, 4, &aad, &tag, &mut buf.clone()).is_err());
    assert!(x.decrypt_block(10, 3, &aad, &tag, &mut buf.clone()).is_err());
}

// ── Key hierarchy: Argon2id KEK + DEK wrap ───────────────────────────────────

fn test_header(dek_wrapped: [u8; 32], nonce: [u8; 12], tag: [u8; 16]) -> StaticHeader {
    StaticHeader {
        format_version: FORMAT_VERSION,
        header_version: HEADER_VERSION,
        block_size: BLOCK_SIZE as u32,
        total_blocks: 1024,
        sb_slot_a: 1,
        sb_slot_b: 2,
        first_data_block: 3,
        uuid: [0x11; 16],
        feature_compat: 0,
        feature_incompat: INCOMPAT_ENCRYPTION,
        feature_ro_compat: 1,
        kdf_algo: 1,
        kdf_salt: [0x22; 16],
        argon_m_cost: 8, // tiny for tests
        argon_t_cost: 1,
        argon_p: 1,
        dek_wrap_nonce: nonce,
        dek_wrapped,
        dek_wrap_tag: tag,
        label: [0; 64],
    }
}

fn derive_test_kek(pass: &[u8], salt: &[u8; 16]) -> [u8; 32] {
    let n = argon2_block_count(8, 1, 1).unwrap();
    let mut mem = alloc::vec![Argon2Block::new(); n];
    let mut kek = [0u8; 32];
    derive_kek(pass, salt, 8, 1, 1, &mut mem, &mut kek).unwrap();
    kek
}

#[test]
fn kek_derivation_deterministic_and_passphrase_sensitive() {
    let salt = [0x22u8; 16];
    let a = derive_test_kek(b"correct horse", &salt);
    let b = derive_test_kek(b"correct horse", &salt);
    let c = derive_test_kek(b"battery staple", &salt);
    assert_eq!(a, b, "same passphrase+salt → same KEK");
    assert_ne!(a, c, "different passphrase → different KEK");
    assert_ne!(a, [0u8; 32]);
}

#[test]
fn kek_derivation_rejects_undersized_memory() {
    let mut mem = alloc::vec![Argon2Block::new(); 1]; // far too small
    let mut kek = [0u8; 32];
    let e = derive_kek(b"pw", &[0x22; 16], 8, 1, 1, &mut mem, &mut kek);
    assert_eq!(e, Err(Error::Invalid), "undersized buffer fails loud, no truncation");
}

#[test]
fn dek_wrap_roundtrip() {
    let kek = derive_test_kek(b"unlock", &[0x22; 16]);
    let dek = dek();
    let nonce = [0x33u8; 12];
    let mut h = test_header([0; 32], nonce, [0; 16]);
    let (wrapped, tag) = wrap_dek(&kek, &dek, &nonce, &h);
    h.dek_wrapped = wrapped;
    h.dek_wrap_tag = tag;
    let got = unwrap_dek(&kek, &h).unwrap();
    assert_eq!(got, dek, "unwrap under the right KEK recovers the DEK");
}

#[test]
fn dek_wrap_wrong_passphrase_fails() {
    let kek = derive_test_kek(b"unlock", &[0x22; 16]);
    let nonce = [0x33u8; 12];
    let mut h = test_header([0; 32], nonce, [0; 16]);
    let (wrapped, tag) = wrap_dek(&kek, &dek(), &nonce, &h);
    h.dek_wrapped = wrapped;
    h.dek_wrap_tag = tag;
    let wrong = derive_test_kek(b"WRONG", &[0x22; 16]);
    assert_eq!(unwrap_dek(&wrong, &h), Err(Error::Auth { block: 0 }));
}

#[test]
fn dek_wrap_tampered_header_fails() {
    let kek = derive_test_kek(b"unlock", &[0x22; 16]);
    let nonce = [0x33u8; 12];
    let mut h = test_header([0; 32], nonce, [0; 16]);
    let (wrapped, tag) = wrap_dek(&kek, &dek(), &nonce, &h);
    h.dek_wrapped = wrapped;
    h.dek_wrap_tag = tag;
    // Flip a geometry field that is in the wrap AAD → unwrap must fail.
    h.total_blocks ^= 1;
    assert_eq!(unwrap_dek(&kek, &h), Err(Error::Auth { block: 0 }));
}
