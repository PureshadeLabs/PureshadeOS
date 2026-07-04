//! Space management (doc 05): in-memory bitmap + mount-time mark-and-sweep.
//! There is no persistent free-space structure anywhere on disk; a block is
//! free iff it is reachable from no valid superblock (SPACE-1).

use alloc::vec;
use alloc::vec::Vec;

use crate::device::BlockDevice;
use crate::inode::Inode;
use crate::layout::{BlockPtr, BLOCK_PTR_SIZE, BLOCK_SIZE, FANOUT, INODES_PER_LEAF, INODE_SIZE};
use crate::transform::{dyn_aad, BlockTransform};
use crate::Result;

#[derive(Debug, Clone)]
pub struct Bitmap {
    words: Vec<u64>,
    nbits: u64,
}

impl Bitmap {
    pub fn new(nbits: u64) -> Bitmap {
        Bitmap { words: vec![0u64; nbits.div_ceil(64) as usize], nbits }
    }

    pub fn len(&self) -> u64 {
        self.nbits
    }
    pub fn is_empty(&self) -> bool {
        self.nbits == 0
    }

    pub fn get(&self, i: u64) -> bool {
        debug_assert!(i < self.nbits);
        self.words[(i / 64) as usize] & (1u64 << (i % 64)) != 0
    }

    /// Set bit `i`; returns the previous value.
    pub fn set(&mut self, i: u64) -> bool {
        debug_assert!(i < self.nbits);
        let w = &mut self.words[(i / 64) as usize];
        let mask = 1u64 << (i % 64);
        let prev = *w & mask != 0;
        *w |= mask;
        prev
    }

    pub fn clear(&mut self, i: u64) {
        debug_assert!(i < self.nbits);
        self.words[(i / 64) as usize] &= !(1u64 << (i % 64));
    }

    pub fn count(&self) -> u64 {
        self.words.iter().map(|w| w.count_ones() as u64).sum()
    }

    pub fn indices(&self) -> Vec<u64> {
        let mut out = Vec::new();
        for i in 0..self.nbits {
            if self.get(i) {
                out.push(i);
            }
        }
        out
    }
}

/// Dereference a `BlockPtr`: read, then open through the transform seam with
/// AAD `block ‖ gen` and the parent-recorded tag (doc 02 §4). Authentication
/// failure is a hard error, never tolerated.
pub fn read_ptr<D: BlockDevice, T: BlockTransform>(
    dev: &mut D,
    xform: &T,
    ptr: &BlockPtr,
) -> Result<Vec<u8>> {
    debug_assert!(!ptr.is_null());
    let mut buf = vec![0u8; BLOCK_SIZE];
    dev.read_block(ptr.block, &mut buf)?;
    let aad = dyn_aad(ptr.block, ptr.gen);
    xform.decrypt_block(ptr.block, ptr.gen, &aad, &ptr.tag, &mut buf)?;
    Ok(buf)
}

/// Mark every block reachable from an inode-map root (doc 05 §2). Every
/// dereference is authenticated; a failure aborts the mount loudly rather
/// than marking a wrong set. Returns the number of newly-marked blocks.
pub fn mark_tree<D: BlockDevice, T: BlockTransform>(
    dev: &mut D,
    xform: &T,
    root: &BlockPtr,
    height: u64,
    bm: &mut Bitmap,
) -> Result<u64> {
    let mut count = 0u64;
    mark_imap(dev, xform, root, height, bm, &mut count)?;
    Ok(count)
}

fn mark_imap<D: BlockDevice, T: BlockTransform>(
    dev: &mut D,
    xform: &T,
    ptr: &BlockPtr,
    level: u64,
    bm: &mut Bitmap,
    count: &mut u64,
) -> Result<()> {
    if ptr.is_null() {
        return Ok(());
    }
    if bm.set(ptr.block) {
        return Ok(()); // already visited (shared between the two live trees)
    }
    *count += 1;
    let buf = read_ptr(dev, xform, ptr)?;
    if level > 0 {
        for i in 0..FANOUT {
            let child = BlockPtr::from_bytes(&buf[i * BLOCK_PTR_SIZE..(i + 1) * BLOCK_PTR_SIZE]);
            mark_imap(dev, xform, &child, level - 1, bm, count)?;
        }
    } else {
        for slot in 0..INODES_PER_LEAF {
            let ino = Inode::decode(&buf[slot * INODE_SIZE..(slot + 1) * INODE_SIZE]);
            // Fast symlinks store target bytes where the block map lives —
            // there is nothing to traverse (doc 06 §1 symlinks).
            if ino.is_used() && !ino.is_fast_symlink() {
                mark_bmap(dev, xform, &ino.bmap_root, ino.bmap_height as u64, bm, count)?;
            }
        }
    }
    Ok(())
}

fn mark_bmap<D: BlockDevice, T: BlockTransform>(
    dev: &mut D,
    xform: &T,
    ptr: &BlockPtr,
    height: u64,
    bm: &mut Bitmap,
    count: &mut u64,
) -> Result<()> {
    if ptr.is_null() {
        return Ok(());
    }
    if bm.set(ptr.block) {
        return Ok(());
    }
    *count += 1;
    // Read even at height 0 (a data block): "each live block is read and
    // authenticated once" (doc 05 §3).
    let buf = read_ptr(dev, xform, ptr)?;
    if height > 0 {
        for i in 0..FANOUT {
            let child = BlockPtr::from_bytes(&buf[i * BLOCK_PTR_SIZE..(i + 1) * BLOCK_PTR_SIZE]);
            mark_bmap(dev, xform, &child, height - 1, bm, count)?;
        }
    }
    Ok(())
}
