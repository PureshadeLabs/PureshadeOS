//! Superblock: dual slots, generation numbers, validity (doc 03).

use alloc::vec;

use crate::device::BlockDevice;
use crate::layout::{
    rd_u64, wr_u64, BlockPtr, StaticHeader, BLOCK_SIZE, SB_MAGIC, SB_PAYLOAD_SIZE,
};
use crate::transform::BlockTransform;
use crate::{Error, Result};

/// Superblock payload (bytes 0–4071 of a slot, doc 03 §2). The plaintext
/// trailer — `gen_copy` at 4072, tag at 4080 — is handled by the transform
/// seam.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Superblock {
    pub gen: u64,
    pub total_blocks: u64,
    pub inode_map_root: BlockPtr,
    pub inode_map_height: u64,
    pub next_inode: u64,
    pub inode_count: u64,
    /// Advisory live-set size: blocks reachable via `inode_map_root`
    /// (excluding fixed blocks 0–2). Cross-checked by mark-and-sweep, not
    /// trusted (doc 05 §2).
    pub block_count: u64,
    pub commit_time: u64,
    pub uuid: [u8; 16],
}

impl Superblock {
    pub fn encode_payload(&self, out: &mut [u8]) {
        assert!(out.len() >= SB_PAYLOAD_SIZE);
        out[..SB_PAYLOAD_SIZE].fill(0);
        out[0..8].copy_from_slice(&SB_MAGIC);
        wr_u64(&mut out[8..], self.gen);
        wr_u64(&mut out[16..], self.total_blocks);
        out[24..56].copy_from_slice(&self.inode_map_root.to_bytes());
        wr_u64(&mut out[56..], self.inode_map_height);
        wr_u64(&mut out[64..], self.next_inode);
        wr_u64(&mut out[72..], self.inode_count);
        wr_u64(&mut out[80..], self.block_count);
        wr_u64(&mut out[88..], self.commit_time);
        out[96..112].copy_from_slice(&self.uuid);
    }

    pub fn decode_payload(buf: &[u8]) -> Result<Superblock> {
        if buf[0..8] != SB_MAGIC {
            return Err(Error::NoSuperblock);
        }
        let mut uuid = [0u8; 16];
        uuid.copy_from_slice(&buf[96..112]);
        Ok(Superblock {
            gen: rd_u64(&buf[8..]),
            total_blocks: rd_u64(&buf[16..]),
            inode_map_root: BlockPtr::from_bytes(&buf[24..56]),
            inode_map_height: rd_u64(&buf[56..]),
            next_inode: rd_u64(&buf[64..]),
            inode_count: rd_u64(&buf[72..]),
            block_count: rd_u64(&buf[80..]),
            commit_time: rd_u64(&buf[88..]),
            uuid,
        })
    }
}

/// Seal and write a superblock into `slot`. The only in-place block write in
/// the filesystem (doc 02 §2); safe because a torn slot is detected and the
/// other slot survives.
pub fn write_slot<D: BlockDevice, T: BlockTransform>(
    dev: &mut D,
    xform: &T,
    slot: u64,
    sb: &Superblock,
) -> Result<()> {
    let mut buf = vec![0u8; BLOCK_SIZE];
    sb.encode_payload(&mut buf);
    xform.seal_superblock(slot, sb.gen, &sb.uuid, &mut buf);
    dev.write_block(slot, &buf)
}

/// Read and validate one slot. Returns `None` for any invalid slot — blank,
/// torn, tampered, or failing the structural checks (`sb_magic`,
/// `total_blocks`, `uuid`, `gen ≥ 1`, payload `gen` == trailer `gen_copy`)
/// per doc 03 §2–3. Never an error: an invalid slot is an expected state,
/// and the caller falls back to the other.
pub fn read_slot<D: BlockDevice, T: BlockTransform>(
    dev: &mut D,
    xform: &T,
    slot: u64,
    header: &StaticHeader,
) -> Option<Superblock> {
    let mut buf = vec![0u8; BLOCK_SIZE];
    dev.read_block(slot, &mut buf).ok()?;
    let trailer_gen = xform.open_superblock(slot, &header.uuid, &mut buf).ok()?;
    let sb = Superblock::decode_payload(&buf).ok()?;
    if sb.gen == 0
        || sb.gen != trailer_gen
        || sb.total_blocks != header.total_blocks
        || sb.uuid != header.uuid
    {
        return None;
    }
    Some(sb)
}
