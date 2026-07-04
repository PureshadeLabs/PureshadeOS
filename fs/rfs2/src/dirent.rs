//! ext2-style directory entries (doc 07). 12-byte header (inode is u64),
//! 4-byte-aligned records, `rec_len` chains that partition each 4096-byte
//! block exactly (DIR-1).

use alloc::vec::Vec;

use crate::layout::{rd_u16, rd_u64, wr_u16, wr_u64, BLOCK_SIZE};
use crate::{Error, Result};

pub const DIRENT_HDR: usize = 12;
pub const NAME_MAX: usize = 255;

pub const FT_UNKNOWN: u8 = 0;
pub const FT_REG: u8 = 1;
pub const FT_DIR: u8 = 2;
pub const FT_SYMLINK: u8 = 3;
pub const FT_CHRDEV: u8 = 4;
pub const FT_BLKDEV: u8 = 5;
pub const FT_FIFO: u8 = 6;
pub const FT_SOCK: u8 = 7;

pub fn align4(n: usize) -> usize {
    (n + 3) & !3
}

/// One parsed record (holes included: `inode == 0`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dirent {
    pub offset: usize,
    pub inode: u64,
    pub rec_len: usize,
    pub file_type: u8,
    pub name: Vec<u8>,
}

/// Parse a full directory block, enforcing DIR-1: records 4-byte aligned,
/// `rec_len` values partition the block exactly, names fit their records.
pub fn parse_block(block: &[u8]) -> Result<Vec<Dirent>> {
    debug_assert_eq!(block.len(), BLOCK_SIZE);
    let mut out = Vec::new();
    let mut off = 0usize;
    while off < BLOCK_SIZE {
        if off + DIRENT_HDR > BLOCK_SIZE {
            return Err(Error::Corrupt);
        }
        let inode = rd_u64(&block[off..]);
        let rec_len = rd_u16(&block[off + 8..]) as usize;
        let name_len = block[off + 10] as usize;
        let file_type = block[off + 11];
        if rec_len < DIRENT_HDR
            || !rec_len.is_multiple_of(4)
            || off + rec_len > BLOCK_SIZE
            || DIRENT_HDR + name_len > rec_len
        {
            return Err(Error::Corrupt);
        }
        if inode != 0 && name_len == 0 {
            return Err(Error::Corrupt);
        }
        out.push(Dirent {
            offset: off,
            inode,
            rec_len,
            file_type,
            name: block[off + DIRENT_HDR..off + DIRENT_HDR + name_len].to_vec(),
        });
        off += rec_len;
    }
    if off != BLOCK_SIZE {
        return Err(Error::Corrupt);
    }
    Ok(out)
}

/// Initialize a fresh directory block: one hole record spanning the block, so
/// the chain invariant holds before any entry is inserted.
pub fn init_empty(block: &mut [u8]) {
    debug_assert_eq!(block.len(), BLOCK_SIZE);
    block.fill(0);
    wr_u16(&mut block[8..], BLOCK_SIZE as u16);
}

fn write_entry(block: &mut [u8], off: usize, rec_len: usize, ino: u64, ftype: u8, name: &[u8]) {
    wr_u64(&mut block[off..], ino);
    wr_u16(&mut block[off + 8..], rec_len as u16);
    block[off + 10] = name.len() as u8;
    block[off + 11] = ftype;
    block[off + DIRENT_HDR..off + DIRENT_HDR + name.len()].copy_from_slice(name);
    // Zero the padding to the end of the header+name span we own.
    let used_end = off + DIRENT_HDR + name.len();
    let pad_end = off + align4(DIRENT_HDR + name.len()).min(rec_len);
    block[used_end..pad_end].fill(0);
}

/// Find a live entry by name. Returns `(offset, inode, file_type)`.
pub fn find(block: &[u8], name: &[u8]) -> Result<Option<(usize, u64, u8)>> {
    for d in parse_block(block)? {
        if d.inode != 0 && d.name == name {
            return Ok(Some((d.offset, d.inode, d.file_type)));
        }
    }
    Ok(None)
}

/// Insert an entry: reuse a hole with room, or split the slack of a live
/// record (doc 07 §3 create step 3). Returns `false` if the block is full.
pub fn insert(block: &mut [u8], ino: u64, ftype: u8, name: &[u8]) -> Result<bool> {
    if name.is_empty() || name.len() > NAME_MAX {
        return Err(Error::Invalid);
    }
    let needed = align4(DIRENT_HDR + name.len());
    for d in parse_block(block)? {
        if d.inode == 0 && d.rec_len >= needed {
            // Reuse the hole, keeping its rec_len so the chain stays intact.
            write_entry(block, d.offset, d.rec_len, ino, ftype, name);
            return Ok(true);
        }
        if d.inode != 0 {
            let used = align4(DIRENT_HDR + d.name.len());
            if d.rec_len - used >= needed {
                // Split: shrink the live record to its used span, put the new
                // entry in the freed slack with the remaining rec_len.
                wr_u16(&mut block[d.offset + 8..], used as u16);
                write_entry(block, d.offset + used, d.rec_len - used, ino, ftype, name);
                return Ok(true);
            }
        }
    }
    Ok(false)
}

/// Remove an entry by name: merge its record into the previous one, or (if it
/// is the first record) turn it into a hole (`inode = 0`) — doc 07 §1.
pub fn remove(block: &mut [u8], name: &[u8]) -> Result<bool> {
    let entries = parse_block(block)?;
    let mut prev: Option<&Dirent> = None;
    for d in &entries {
        if d.inode != 0 && d.name == name {
            match prev {
                Some(p) => {
                    wr_u16(&mut block[p.offset + 8..], (p.rec_len + d.rec_len) as u16);
                }
                None => {
                    wr_u64(&mut block[d.offset..], 0);
                    block[d.offset + 10] = 0; // name_len
                    block[d.offset + 11] = 0; // file_type
                }
            }
            return Ok(true);
        }
        prev = Some(d);
    }
    Ok(false)
}

/// Repoint a live entry at a new inode, in place (used for `..` when a
/// directory moves — doc 07 §3 rename step 5). The buffer is then rewritten
/// COW by the caller; no live block is mutated.
pub fn retarget(block: &mut [u8], name: &[u8], new_ino: u64) -> Result<bool> {
    for d in parse_block(block)? {
        if d.inode != 0 && d.name == name {
            wr_u64(&mut block[d.offset..], new_ino);
            return Ok(true);
        }
    }
    Ok(false)
}

/// True if the block contains no live entry other than `.` and `..`.
pub fn only_dots(block: &[u8]) -> Result<bool> {
    for d in parse_block(block)? {
        if d.inode != 0 && d.name != b"." && d.name != b".." {
            return Ok(false);
        }
    }
    Ok(true)
}
