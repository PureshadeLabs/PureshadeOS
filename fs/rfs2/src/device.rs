//! Block device abstraction. The kernel backs this with virtio-blk
//! (8 × 512-byte sectors per 4096-byte block); tests use an in-memory image.

use crate::Result;

pub trait BlockDevice {
    fn total_blocks(&self) -> u64;

    /// Read one 4096-byte block. `buf.len() == 4096`.
    fn read_block(&mut self, block: u64, buf: &mut [u8]) -> Result<()>;

    /// Write one 4096-byte block. Not durable until [`flush`](Self::flush).
    fn write_block(&mut self, block: u64, buf: &[u8]) -> Result<()>;

    /// Write barrier: everything written before this call is durable when it
    /// returns. Commit ordering (COW-3, doc 04 §4) is load-bearing on this.
    fn flush(&mut self) -> Result<()>;
}
