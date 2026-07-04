//! Shared test fixtures: in-memory block device with a volatile/durable split
//! (crash simulation), and a XOR "cipher" transform that exercises the crypto
//! seam end-to-end (real tags carried in BlockPtrs and the SB trailer).

#![allow(dead_code)]

use rfs2::layout::BLOCK_SIZE;
use rfs2::transform::{Tag, TAG_SIZE};
use rfs2::{mkfs, BlockDevice, BlockTransform, Error, IdentityTransform, MkfsOptions, Rfs2};

pub const UUID: [u8; 16] = *b"rfs2-test-uuid!!";

pub fn tnow() -> u64 {
    1_751_500_000_000_000_000
}

// ── In-memory device ─────────────────────────────────────────────────────────

/// Writes land in `vol`; `flush` copies to `durable`. `crash()` yields a
/// device holding only what was durable — everything since the last barrier
/// is lost, at whole-block granularity (torn sub-block writes are the real
/// transform's detection job; see doc 02 §2).
#[derive(Clone)]
pub struct MemDev {
    pub vol: Vec<u8>,
    pub durable: Vec<u8>,
}

impl MemDev {
    pub fn new(blocks: u64) -> MemDev {
        let bytes = blocks as usize * BLOCK_SIZE;
        MemDev { vol: vec![0u8; bytes], durable: vec![0u8; bytes] }
    }

    pub fn crash(&self) -> MemDev {
        MemDev { vol: self.durable.clone(), durable: self.durable.clone() }
    }

    pub fn block(&self, b: u64) -> &[u8] {
        &self.vol[b as usize * BLOCK_SIZE..(b as usize + 1) * BLOCK_SIZE]
    }

    pub fn corrupt(&mut self, block: u64, offset: usize) {
        let i = block as usize * BLOCK_SIZE + offset;
        self.vol[i] ^= 0xff;
        self.durable[i] ^= 0xff;
    }
}

impl BlockDevice for MemDev {
    fn total_blocks(&self) -> u64 {
        (self.vol.len() / BLOCK_SIZE) as u64
    }

    fn read_block(&mut self, block: u64, buf: &mut [u8]) -> rfs2::Result<()> {
        buf.copy_from_slice(self.block(block));
        Ok(())
    }

    fn write_block(&mut self, block: u64, buf: &[u8]) -> rfs2::Result<()> {
        let off = block as usize * BLOCK_SIZE;
        self.vol[off..off + BLOCK_SIZE].copy_from_slice(buf);
        Ok(())
    }

    fn flush(&mut self) -> rfs2::Result<()> {
        self.durable.copy_from_slice(&self.vol);
        Ok(())
    }
}

// ── XOR transform ────────────────────────────────────────────────────────────

/// Toy transform: XOR "cipher" + additive checksum tag mixing block/gen/AAD.
/// Zero cryptographic value — it exists to prove the seam is pluggable with a
/// transform that actually changes bytes and carries meaningful tags through
/// the same call sites the future AES-GCM transform will use.
pub struct XorTransform {
    pub key: u8,
}

impl XorTransform {
    fn tag_for(&self, block: u64, gen: u64, aad: &[u8], ciphertext: &[u8]) -> Tag {
        let mut sum: u64 = block
            .wrapping_mul(0x9e37_79b9_7f4a_7c15)
            .wrapping_add(gen.wrapping_mul(0xc2b2_ae3d_27d4_eb4f));
        for &b in aad {
            sum = sum.rotate_left(7).wrapping_add(b as u64);
        }
        for &b in ciphertext {
            sum = sum.rotate_left(1) ^ (b as u64).wrapping_mul(0x0101_0101_0101_0101);
        }
        let mut tag = [0u8; TAG_SIZE];
        tag[..8].copy_from_slice(&sum.to_le_bytes());
        tag[8..].copy_from_slice(&(!sum).to_le_bytes());
        tag
    }
}

impl BlockTransform for XorTransform {
    fn encrypt_block(&self, block: u64, gen: u64, aad: &[u8], data: &mut [u8]) -> Tag {
        for b in data.iter_mut() {
            *b ^= self.key;
        }
        self.tag_for(block, gen, aad, data)
    }

    fn decrypt_block(
        &self,
        block: u64,
        gen: u64,
        aad: &[u8],
        expected_tag: &Tag,
        data: &mut [u8],
    ) -> rfs2::Result<()> {
        if &self.tag_for(block, gen, aad, data) != expected_tag {
            return Err(Error::Auth { block });
        }
        for b in data.iter_mut() {
            *b ^= self.key;
        }
        Ok(())
    }

    // No `open_superblock` override: the default reads `gen` from the
    // plaintext trailer `gen_copy` (doc 03 §2), which is exactly what a real
    // cipher needs — proving the former gen-before-decrypt gap is closed.
}

// ── Fixture helpers ──────────────────────────────────────────────────────────

pub fn opts() -> MkfsOptions<'static> {
    MkfsOptions { uuid: UUID, label: "test-volume", now: tnow() }
}

pub fn fresh_fs(blocks: u64) -> Rfs2<MemDev, IdentityTransform> {
    let mut dev = MemDev::new(blocks);
    mkfs(&mut dev, &IdentityTransform, &opts()).unwrap();
    Rfs2::mount(dev, IdentityTransform, tnow).unwrap()
}
