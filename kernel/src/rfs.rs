//! RFS — Raptor File System kernel driver.
//!
//! Read/write access to RFS_V1 disk images produced by `mkrfs`.
//! Block I/O is performed through the VirtIO block device (8 sectors = 1 block).
//!
//! ## Public API
//!
//! - [`init`]          — mount; call once after virtio-blk init. Returns `true` on success.
//! - [`open`]          — path → fd (≥ 0) or negative error code (read-only).
//! - [`read`]          — fd, buf → bytes read or negative error.
//! - [`write`]         — fd, buf → bytes written or negative error (fd must be writable).
//! - [`close`]         — fd → 0 or negative error.
//! - [`create`]        — path → writable fd (≥ 0) or negative error code.
//! - [`unlink`]        — path → 0 or negative error code.
//! - [`stat_path`]     — path, stat_out → `true` on success.
//! - [`readdir_path`]  — path → `Option<Vec<DirEntry>>`.
//!
//! ## Error codes (returned as negative i64)
//!
//! | Value | Meaning                        |
//! |-------|--------------------------------|
//! |   -1  | No VirtIO block device         |
//! |   -4  | Invalid argument               |
//! |   -5  | No such file or directory      |
//! |   -6  | Bad file descriptor            |
//! |   -7  | Is a directory                 |
//! |   -8  | Not a directory                |
//! |   -9  | Filesystem not mounted         |
//! |  -10  | Too many open files            |
//! |  -11  | File already exists            |
//! |  -12  | No space left on device        |

#![allow(dead_code)]

extern crate alloc;
use alloc::string::String;
use alloc::vec::Vec;

use crate::serial::SpinLock;
use crate::virtio_blk;

// ── Error codes ───────────────────────────────────────────────────────────────

pub const ENODEV:  i64 = -1;
pub const EINVAL:  i64 = -4;
pub const ENOENT:  i64 = -5;
pub const EBADF:   i64 = -6;
pub const EISDIR:  i64 = -7;
pub const ENOTDIR: i64 = -8;
pub const ENOMNT:  i64 = -9;
pub const EMFILE:  i64 = -10;
pub const EEXIST:  i64 = -11;
pub const ENOSPC:  i64 = -12;

// ── On-disk constants ─────────────────────────────────────────────────────────

const BLOCK_SIZE:       usize = 4096;
const SECTORS_PER_BLK: u64   = (BLOCK_SIZE / virtio_blk::SECTOR_SIZE) as u64; // 8

const MAGIC: &[u8; 8] = b"RFS_V1\0\0";

const BITMAP_BLOCK:     u32   = 1;
const INODE_START:      u32   = 2;
const INODE_COUNT:      u32   = 1024;
const INODES_PER_BLOCK: u32   = (BLOCK_SIZE / INODE_SIZE) as u32; // 32
const INODE_SIZE:       usize = 128;
const DATA_START:       u32   = 34; // first usable data block

const INLINE_EXTENTS: usize = 4;
const OVFL_EXTENTS:   usize = 255;
const EXTENT_SIZE:    usize = 16;
const OVFL_HDR:       usize = 16; // next(4) + used(4) + _pad(8)

pub const INODE_USED:     u32 = 1 << 0;
pub const INODE_DIR:      u32 = 1 << 1;
pub const INODE_SYMLINK:  u32 = 1 << 2;
pub const INODE_FAST_SYM: u32 = 1 << 3;

pub const FT_REG:     u8 = 1;
pub const FT_DIR:     u8 = 2;
pub const FT_SYMLINK: u8 = 3;

const MAX_FDS:          usize = 64;
const MAX_SYMLINK_HOPS: usize = 8;

// ── Little-endian helpers ─────────────────────────────────────────────────────

#[inline]
fn get_u16(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes(b[o..o+2].try_into().unwrap())
}
#[inline]
fn get_u32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o+4].try_into().unwrap())
}
#[inline]
fn get_u64(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o+8].try_into().unwrap())
}

// ── Block I/O ─────────────────────────────────────────────────────────────────

fn read_block(blk: u32) -> Option<[u8; BLOCK_SIZE]> {
    let base = blk as u64 * SECTORS_PER_BLK;
    let mut out = [0u8; BLOCK_SIZE];
    for i in 0..SECTORS_PER_BLK {
        let mut sector = [0u8; virtio_blk::SECTOR_SIZE];
        if !virtio_blk::read_sector(base + i, &mut sector) {
            return None;
        }
        let off = (i as usize) * virtio_blk::SECTOR_SIZE;
        out[off..off + virtio_blk::SECTOR_SIZE].copy_from_slice(&sector);
    }
    Some(out)
}

fn write_block(blk: u32, data: &[u8; BLOCK_SIZE]) -> bool {
    let base = blk as u64 * SECTORS_PER_BLK;
    for i in 0..SECTORS_PER_BLK {
        let off = (i as usize) * virtio_blk::SECTOR_SIZE;
        let sector: &[u8; virtio_blk::SECTOR_SIZE] =
            data[off..off + virtio_blk::SECTOR_SIZE].try_into().unwrap();
        if !virtio_blk::write_sector(base + i, sector) {
            return false;
        }
    }
    true
}

// ── Inode ─────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy)]
pub struct Inode {
    pub flags:        u32,
    pub mode:         u16,
    pub uid:          u32,
    pub gid:          u32,
    pub nlink:        u32,
    pub size:         u64,
    pub blocks:       u64,
    pub mtime:        u64,
    pub ctime:        u64,
    pub ovfl_block:   u32,
    pub extent_count: u16,
    // Raw packed extent bytes: [logical(4) physical(4) count(4) flags(4)] × 4
    extents: [[u8; EXTENT_SIZE]; INLINE_EXTENTS],
}

fn parse_inode(b: &[u8]) -> Inode {
    let mut extents = [[0u8; EXTENT_SIZE]; INLINE_EXTENTS];
    for (i, slot) in extents.iter_mut().enumerate() {
        let off = 60 + i * EXTENT_SIZE;
        slot.copy_from_slice(&b[off..off + EXTENT_SIZE]);
    }
    Inode {
        flags:        get_u32(b,  0),
        mode:         get_u16(b,  4),
        uid:          get_u32(b,  8),
        gid:          get_u32(b, 12),
        nlink:        get_u32(b, 16),
        size:         get_u64(b, 20),
        blocks:       get_u64(b, 28),
        mtime:        get_u64(b, 36),
        ctime:        get_u64(b, 44),
        ovfl_block:   get_u32(b, 52),
        extent_count: get_u16(b, 56),
        extents,
    }
}

fn read_inode(ino: u32) -> Option<Inode> {
    if ino >= INODE_COUNT { return None; }
    let blk = INODE_START + ino / INODES_PER_BLOCK;
    let buf = read_block(blk)?;
    let off = ((ino % INODES_PER_BLOCK) as usize) * INODE_SIZE;
    let inode = parse_inode(&buf[off..off + INODE_SIZE]);
    if inode.flags & INODE_USED == 0 { return None; }
    Some(inode)
}

fn serialize_inode(inode: &Inode) -> [u8; INODE_SIZE] {
    let mut b = [0u8; INODE_SIZE];
    b[ 0.. 4].copy_from_slice(&inode.flags.to_le_bytes());
    b[ 4.. 6].copy_from_slice(&inode.mode.to_le_bytes());
    b[ 8..12].copy_from_slice(&inode.uid.to_le_bytes());
    b[12..16].copy_from_slice(&inode.gid.to_le_bytes());
    b[16..20].copy_from_slice(&inode.nlink.to_le_bytes());
    b[20..28].copy_from_slice(&inode.size.to_le_bytes());
    b[28..36].copy_from_slice(&inode.blocks.to_le_bytes());
    b[36..44].copy_from_slice(&inode.mtime.to_le_bytes());
    b[44..52].copy_from_slice(&inode.ctime.to_le_bytes());
    b[52..56].copy_from_slice(&inode.ovfl_block.to_le_bytes());
    b[56..58].copy_from_slice(&inode.extent_count.to_le_bytes());
    for (i, slot) in inode.extents.iter().enumerate() {
        let off = 60 + i * EXTENT_SIZE;
        b[off..off + EXTENT_SIZE].copy_from_slice(slot);
    }
    b
}

fn write_inode(ino: u32, inode: &Inode) -> bool {
    if ino >= INODE_COUNT { return false; }
    let blk = INODE_START + ino / INODES_PER_BLOCK;
    let off = ((ino % INODES_PER_BLOCK) as usize) * INODE_SIZE;
    let mut buf = match read_block(blk) {
        Some(b) => b,
        None    => return false,
    };
    buf[off..off + INODE_SIZE].copy_from_slice(&serialize_inode(inode));
    write_block(blk, &buf)
}

// ── Extent traversal ──────────────────────────────────────────────────────────

/// Map logical block index → physical block number.
/// `None` = sparse hole; caller should zero-fill.
fn resolve_block(inode: &Inode, logical: u32) -> Option<u32> {
    let inline_count = (inode.extent_count as usize).min(INLINE_EXTENTS);

    for i in 0..inline_count {
        let e          = &inode.extents[i];
        let e_logical  = get_u32(e, 0);
        let e_physical = get_u32(e, 4);
        let e_count    = get_u32(e, 8);
        if logical >= e_logical && logical < e_logical + e_count {
            return Some(e_physical + (logical - e_logical));
        }
    }

    if (inode.extent_count as usize) <= INLINE_EXTENTS || inode.ovfl_block == 0 {
        return None;
    }

    let mut ovfl         = inode.ovfl_block;
    let mut seen_extents = inline_count;

    loop {
        let buf  = read_block(ovfl)?;
        let used = get_u32(&buf, 4) as usize;

        for i in 0..used.min(OVFL_EXTENTS) {
            let off        = OVFL_HDR + i * EXTENT_SIZE;
            let e_logical  = get_u32(&buf, off);
            let e_physical = get_u32(&buf, off + 4);
            let e_count    = get_u32(&buf, off + 8);
            if logical >= e_logical && logical < e_logical + e_count {
                return Some(e_physical + (logical - e_logical));
            }
        }

        seen_extents += used;
        if seen_extents >= inode.extent_count as usize { break; }

        let next = get_u32(&buf, 0);
        if next == 0 { break; }
        ovfl = next;
    }
    None
}

// ── File data read ────────────────────────────────────────────────────────────

/// Read up to `buf.len()` bytes from `inode` starting at byte `offset`.
/// Sparse holes are zero-filled. Returns bytes actually read.
pub fn read_file_data(inode: &Inode, offset: u64, buf: &mut [u8]) -> usize {
    if offset >= inode.size { return 0; }
    let avail    = (inode.size - offset) as usize;
    let to_read  = buf.len().min(avail);
    let mut done = 0usize;

    while done < to_read {
        let file_off = offset + done as u64;
        let logical  = (file_off / BLOCK_SIZE as u64) as u32;
        let blk_off  = (file_off % BLOCK_SIZE as u64) as usize;
        let chunk    = (BLOCK_SIZE - blk_off).min(to_read - done);

        if let Some(phys) = resolve_block(inode, logical) {
            match read_block(phys) {
                Some(b) => buf[done..done + chunk].copy_from_slice(&b[blk_off..blk_off + chunk]),
                None    => break,
            }
        } else {
            buf[done..done + chunk].fill(0); // sparse hole
        }
        done += chunk;
    }
    done
}

// ── Symlink target ────────────────────────────────────────────────────────────

fn read_symlink_target(inode: &Inode) -> Option<Vec<u8>> {
    let len = inode.size as usize;
    if inode.flags & INODE_FAST_SYM != 0 {
        // Target stored inline in the extents[] field (max 64 bytes).
        let capped = len.min(64);
        let mut out = Vec::with_capacity(capped);
        'done: for slot in &inode.extents {
            for &byte in slot {
                if out.len() >= capped { break 'done; }
                out.push(byte);
            }
        }
        Some(out)
    } else {
        let mut buf = alloc::vec![0u8; len];
        let n = read_file_data(inode, 0, &mut buf);
        if n < len { None } else { Some(buf) }
    }
}

// ── Directory scanning ────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct DirEntry {
    pub ino:       u32,
    pub file_type: u8,
    pub name:      String,
}

/// Return all live entries in directory `inode` (including `.` and `..`).
pub fn scan_dir(inode: &Inode) -> Vec<DirEntry> {
    let mut entries = Vec::new();
    let size        = inode.size as usize;
    if size == 0 { return entries; }
    let n_blocks = (size + BLOCK_SIZE - 1) / BLOCK_SIZE;

    for logical in 0..n_blocks as u32 {
        let phys = match resolve_block(inode, logical) {
            Some(p) => p,
            None    => continue,
        };
        let buf = match read_block(phys) {
            Some(b) => b,
            None    => continue,
        };

        // The last block may be partial; all others are full.
        let is_last    = (logical as usize + 1) * BLOCK_SIZE > size;
        let block_used = if is_last && size % BLOCK_SIZE != 0 { size % BLOCK_SIZE } else { BLOCK_SIZE };

        let mut pos = 0usize;
        while pos + 8 <= block_used {
            let ino       = get_u32(&buf, pos);
            let rec_len   = get_u16(&buf, pos + 4) as usize;
            let name_len  = buf[pos + 6] as usize;
            let file_type = buf[pos + 7];

            if rec_len == 0 { break; } // corrupted — stop

            if ino != 0 && name_len > 0 && pos + 8 + name_len <= BLOCK_SIZE {
                let raw = &buf[pos + 8..pos + 8 + name_len];
                if let Ok(s) = core::str::from_utf8(raw) {
                    entries.push(DirEntry { ino, file_type, name: String::from(s) });
                }
            }
            pos += rec_len;
        }
    }
    entries
}

fn lookup_in_dir(dir: &Inode, name: &str) -> Option<u32> {
    scan_dir(dir).into_iter().find(|e| e.name == name).map(|e| e.ino)
}

// ── Block/inode allocator ─────────────────────────────────────────────────────

/// Find a free data block, mark it used in the bitmap, return its number.
fn alloc_block(total_blocks: u32) -> Option<u32> {
    let mut bitmap = read_block(BITMAP_BLOCK)?;
    for byte_idx in 0..BLOCK_SIZE {
        if bitmap[byte_idx] == 0xFF { continue; }
        for bit in 0..8u32 {
            let blk = (byte_idx as u32) * 8 + bit;
            if blk < DATA_START || blk >= total_blocks { continue; }
            if bitmap[byte_idx] & (1u8 << bit) == 0 {
                bitmap[byte_idx] |= 1u8 << bit;
                if !write_block(BITMAP_BLOCK, &bitmap) { return None; }
                return Some(blk);
            }
        }
    }
    None
}

/// Clear a block's bitmap bit, returning it to the free pool.
fn free_block(blk: u32) {
    if blk < DATA_START { return; }
    if let Some(mut bitmap) = read_block(BITMAP_BLOCK) {
        let byte_idx = (blk / 8) as usize;
        let bit      = blk % 8;
        bitmap[byte_idx] &= !(1u8 << bit);
        let _ = write_block(BITMAP_BLOCK, &bitmap);
    }
}

/// Find a free inode slot (skips inode 0, the root). Returns inode number.
fn alloc_inode() -> Option<u32> {
    for blk_off in 0..32u32 {
        let buf = read_block(INODE_START + blk_off)?;
        for slot in 0..INODES_PER_BLOCK {
            let ino = blk_off * INODES_PER_BLOCK + slot;
            if ino == 0 { continue; }
            let off = (slot as usize) * INODE_SIZE;
            if get_u32(&buf, off) & INODE_USED == 0 {
                return Some(ino);
            }
        }
    }
    None
}

/// Zero-out an inode on disk, marking it free.
fn free_inode(ino: u32) -> bool {
    if ino == 0 || ino >= INODE_COUNT { return false; }
    let blk = INODE_START + ino / INODES_PER_BLOCK;
    let off = ((ino % INODES_PER_BLOCK) as usize) * INODE_SIZE;
    let mut buf = match read_block(blk) {
        Some(b) => b,
        None    => return false,
    };
    buf[off..off + INODE_SIZE].fill(0);
    write_block(blk, &buf)
}

/// Free all data blocks and overflow extent blocks belonging to `inode`.
fn free_inode_blocks(inode: &Inode) {
    let n_logical = ((inode.size + BLOCK_SIZE as u64 - 1) / BLOCK_SIZE as u64) as u32;
    for logical in 0..n_logical {
        if let Some(phys) = resolve_block(inode, logical) {
            free_block(phys);
        }
    }
    let mut ovfl = inode.ovfl_block;
    while ovfl != 0 {
        let next = match read_block(ovfl) {
            Some(buf) => get_u32(&buf, 0),
            None      => break,
        };
        free_block(ovfl);
        ovfl = next;
    }
}

// ── Write helpers ─────────────────────────────────────────────────────────────

/// Append one extent entry to `inode`, allocating an overflow block if needed.
fn add_extent(inode: &mut Inode, logical: u32, physical: u32, count: u32,
              total_blocks: u32) -> bool {
    let idx = inode.extent_count as usize;

    if idx < INLINE_EXTENTS {
        let e = &mut inode.extents[idx];
        e[0..4].copy_from_slice(&logical.to_le_bytes());
        e[4..8].copy_from_slice(&physical.to_le_bytes());
        e[8..12].copy_from_slice(&count.to_le_bytes());
        e[12..16].fill(0);
        inode.extent_count += 1;
        return true;
    }

    // Needs an overflow block. Find the tail of the chain.
    if inode.ovfl_block == 0 {
        let new_ovfl = match alloc_block(total_blocks) {
            Some(b) => b,
            None    => return false,
        };
        let mut obuf = [0u8; BLOCK_SIZE];
        // next=0, used=1
        obuf[4..8].copy_from_slice(&1u32.to_le_bytes());
        write_ovfl_extent(&mut obuf, 0, logical, physical, count);
        if !write_block(new_ovfl, &obuf) {
            free_block(new_ovfl);
            return false;
        }
        inode.ovfl_block = new_ovfl;
        inode.extent_count += 1;
        return true;
    }

    let mut ovfl = inode.ovfl_block;
    loop {
        let mut obuf = match read_block(ovfl) {
            Some(b) => b,
            None    => return false,
        };
        let next = get_u32(&obuf, 0);
        let used = get_u32(&obuf, 4) as usize;

        if next == 0 {
            if used < OVFL_EXTENTS {
                write_ovfl_extent(&mut obuf, used, logical, physical, count);
                obuf[4..8].copy_from_slice(&((used + 1) as u32).to_le_bytes());
                if !write_block(ovfl, &obuf) { return false; }
            } else {
                let new_ovfl = match alloc_block(total_blocks) {
                    Some(b) => b,
                    None    => return false,
                };
                let mut new_buf = [0u8; BLOCK_SIZE];
                new_buf[4..8].copy_from_slice(&1u32.to_le_bytes());
                write_ovfl_extent(&mut new_buf, 0, logical, physical, count);
                if !write_block(new_ovfl, &new_buf) {
                    free_block(new_ovfl);
                    return false;
                }
                obuf[0..4].copy_from_slice(&new_ovfl.to_le_bytes());
                if !write_block(ovfl, &obuf) { return false; }
            }
            inode.extent_count += 1;
            return true;
        }
        ovfl = next;
    }
}

#[inline]
fn write_ovfl_extent(buf: &mut [u8; BLOCK_SIZE], idx: usize,
                     logical: u32, physical: u32, count: u32) {
    let off = OVFL_HDR + idx * EXTENT_SIZE;
    buf[off   ..off+4 ].copy_from_slice(&logical.to_le_bytes());
    buf[off+4 ..off+8 ].copy_from_slice(&physical.to_le_bytes());
    buf[off+8 ..off+12].copy_from_slice(&count.to_le_bytes());
    buf[off+12..off+16].fill(0);
}

/// Append `data` to the end of `inode`, allocating new blocks as needed.
/// Updates `inode.size`, `inode.blocks`, and writes the inode to disk.
fn append_to_file(ino_num: u32, inode: &mut Inode, data: &[u8],
                  total_blocks: u32) -> bool {
    let mut written = 0usize;
    while written < data.len() {
        let file_off = inode.size;
        let logical  = (file_off / BLOCK_SIZE as u64) as u32;
        let blk_off  = (file_off % BLOCK_SIZE as u64) as usize;

        let phys = if let Some(p) = resolve_block(inode, logical) {
            p
        } else {
            let new_blk = match alloc_block(total_blocks) {
                Some(b) => b,
                None    => return false,
            };
            if !add_extent(inode, logical, new_blk, 1, total_blocks) {
                free_block(new_blk);
                return false;
            }
            inode.blocks += 1;
            new_blk
        };

        let mut block_buf = match read_block(phys) {
            Some(b) => b,
            None    => return false,
        };
        let chunk = (BLOCK_SIZE - blk_off).min(data.len() - written);
        block_buf[blk_off..blk_off + chunk].copy_from_slice(&data[written..written + chunk]);
        if !write_block(phys, &block_buf) { return false; }

        written      += chunk;
        inode.size   += chunk as u64;
    }
    write_inode(ino_num, inode)
}

/// Add a new entry to a directory inode, allocating a new block if needed.
fn add_dir_entry(dir_ino_num: u32, entry_ino: u32, name: &str,
                 file_type: u8, total_blocks: u32) -> bool {
    let name_bytes = name.as_bytes();
    let name_len   = name_bytes.len();
    if name_len == 0 || name_len > 255 { return false; }
    let rec_len = (((8 + name_len + 3) / 4) * 4) as u16;

    let mut dir = match read_inode(dir_ino_num) {
        Some(i) => i,
        None    => return false,
    };
    let n_blocks = ((dir.size as usize + BLOCK_SIZE - 1) / BLOCK_SIZE) as u32;

    for logical in 0..n_blocks {
        let phys = match resolve_block(&dir, logical) {
            Some(p) => p,
            None    => continue,
        };
        let mut buf = match read_block(phys) {
            Some(b) => b,
            None    => continue,
        };

        let is_last   = logical + 1 == n_blocks;
        let block_used = if is_last && dir.size as usize % BLOCK_SIZE != 0 {
            dir.size as usize % BLOCK_SIZE
        } else {
            BLOCK_SIZE
        };

        // Scan for a deleted slot large enough to reuse.
        let mut pos = 0usize;
        while pos + 8 <= block_used {
            let ino_at   = get_u32(&buf, pos);
            let this_rec = get_u16(&buf, pos + 4) as usize;
            if this_rec == 0 { break; }
            if ino_at == 0 && (this_rec as u16) >= rec_len {
                write_dirent(&mut buf, pos, entry_ino, rec_len, name_bytes, file_type);
                return write_block(phys, &buf);
            }
            pos += this_rec;
        }

        // Append at end of this block if there is room.
        if is_last && block_used + rec_len as usize <= BLOCK_SIZE {
            write_dirent(&mut buf, block_used, entry_ino, rec_len, name_bytes, file_type);
            if !write_block(phys, &buf) { return false; }
            dir.size += rec_len as u64;
            return write_inode(dir_ino_num, &dir);
        }
    }

    // Allocate a new directory block.
    let new_blk = match alloc_block(total_blocks) {
        Some(b) => b,
        None    => return false,
    };
    let logical = n_blocks;
    if !add_extent(&mut dir, logical, new_blk, 1, total_blocks) {
        free_block(new_blk);
        return false;
    }
    let mut buf = [0u8; BLOCK_SIZE];
    write_dirent(&mut buf, 0, entry_ino, rec_len, name_bytes, file_type);
    if !write_block(new_blk, &buf) {
        free_block(new_blk);
        return false;
    }
    dir.size   += rec_len as u64;
    dir.blocks += 1;
    write_inode(dir_ino_num, &dir)
}

#[inline]
fn write_dirent(buf: &mut [u8; BLOCK_SIZE], pos: usize, ino: u32,
                rec_len: u16, name: &[u8], file_type: u8) {
    buf[pos   ..pos+4].copy_from_slice(&ino.to_le_bytes());
    buf[pos+4 ..pos+6].copy_from_slice(&rec_len.to_le_bytes());
    buf[pos+6]  = name.len() as u8;
    buf[pos+7]  = file_type;
    buf[pos+8..pos+8+name.len()].copy_from_slice(name);
}

/// Zero the inode field of the first matching directory entry (soft delete).
/// Returns `true` if the entry was found and zeroed.
fn remove_dir_entry(dir_ino_num: u32, name: &str) -> bool {
    let dir = match read_inode(dir_ino_num) {
        Some(i) => i,
        None    => return false,
    };
    let n_blocks = ((dir.size as usize + BLOCK_SIZE - 1) / BLOCK_SIZE) as u32;

    for logical in 0..n_blocks {
        let phys = match resolve_block(&dir, logical) {
            Some(p) => p,
            None    => continue,
        };
        let mut buf = match read_block(phys) {
            Some(b) => b,
            None    => continue,
        };

        let is_last    = logical + 1 == n_blocks;
        let block_used = if is_last && dir.size as usize % BLOCK_SIZE != 0 {
            dir.size as usize % BLOCK_SIZE
        } else {
            BLOCK_SIZE
        };

        let mut pos = 0usize;
        while pos + 8 <= block_used {
            let ino_at   = get_u32(&buf, pos);
            let rec_len  = get_u16(&buf, pos + 4) as usize;
            if rec_len == 0 { break; }
            let name_len = buf[pos + 6] as usize;

            if ino_at != 0 && pos + 8 + name_len <= BLOCK_SIZE {
                if let Ok(n) = core::str::from_utf8(&buf[pos+8..pos+8+name_len]) {
                    if n == name {
                        buf[pos..pos+4].fill(0); // ino = 0
                        return write_block(phys, &buf);
                    }
                }
            }
            pos += rec_len;
        }
    }
    false
}

/// Split `path` into `(parent_path, filename)`.
fn split_parent(path: &str) -> (&str, &str) {
    match path.rfind('/') {
        Some(0)   => ("/", &path[1..]),
        Some(idx) => (&path[..idx], &path[idx+1..]),
        None      => ("/", path),
    }
}

// ── Path resolution ───────────────────────────────────────────────────────────

/// Resolve `path` to `(inode_number, Inode)`. Follows symlinks.
pub fn resolve_path(path: &str) -> Option<(u32, Inode)> {
    resolve_impl(0, String::from(path), 0)
}

fn resolve_impl(start_ino: u32, path: String, hops: usize) -> Option<(u32, Inode)> {
    if hops > MAX_SYMLINK_HOPS { return None; }

    let (mut ino, mut inode) = if path.starts_with('/') {
        (0u32, read_inode(0)?)
    } else {
        (start_ino, read_inode(start_ino)?)
    };

    let components: Vec<String> = path
        .trim_start_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect();

    let mut i = 0;
    while i < components.len() {
        let part = &components[i];
        i += 1;

        if inode.flags & INODE_DIR == 0 { return None; }

        let child_ino = lookup_in_dir(&inode, part)?;
        let child     = read_inode(child_ino)?;

        if child.flags & INODE_SYMLINK != 0 {
            let target_bytes = read_symlink_target(&child)?;
            let target       = core::str::from_utf8(&target_bytes).ok()?;

            // Append remaining components to the symlink target.
            let mut new_path = String::from(target);
            for j in i..components.len() {
                new_path.push('/');
                new_path.push_str(&components[j]);
            }

            let base_ino = if target.starts_with('/') { 0 } else { ino };
            return resolve_impl(base_ino, new_path, hops + 1);
        }

        ino   = child_ino;
        inode = child;
    }
    Some((ino, inode))
}

// ── Stat ──────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Default)]
pub struct Stat {
    pub size:   u64,
    pub flags:  u32,
    pub mode:   u16,
    pub uid:    u32,
    pub gid:    u32,
    pub nlink:  u32,
    pub mtime:  u64,
    pub ctime:  u64,
}

impl From<&Inode> for Stat {
    fn from(n: &Inode) -> Self {
        Stat { size: n.size, flags: n.flags, mode: n.mode, uid: n.uid,
               gid: n.gid, nlink: n.nlink, mtime: n.mtime, ctime: n.ctime }
    }
}

// ── Open-file table ───────────────────────────────────────────────────────────

#[derive(Clone, Copy)]
struct OpenFile {
    ino_num:  u32,
    inode:    Inode,
    offset:   u64,
    writable: bool,
}

struct RfsState {
    mounted:      bool,
    total_blocks: u32,
    fds:          [Option<OpenFile>; MAX_FDS],
}

static STATE: SpinLock<RfsState> = SpinLock::new(RfsState {
    mounted:      false,
    total_blocks: 0,
    fds:          [None; MAX_FDS],
});

// ── Mount ─────────────────────────────────────────────────────────────────────

/// Validate the superblock and mark the filesystem as mounted.
pub fn init() -> bool {
    if !virtio_blk::is_present() { return false; }
    crate::kprintln!("[rfs] reading superblock...");
    let buf = match read_block(0) {
        Some(b) => b,
        None    => return false,
    };
    if &buf[0..8] != MAGIC { return false; }

    let total_blocks = get_u32(&buf, 16);
    let mut st       = STATE.lock();
    st.mounted       = true;
    st.total_blocks  = total_blocks;
    true
}

fn is_mounted() -> bool {
    STATE.lock().mounted
}

// ── VFS operations ────────────────────────────────────────────────────────────

/// Open a regular file by path. Returns fd (≥ 0) or a negative error code.
pub fn open(path: &[u8]) -> i64 {
    if !is_mounted() { return ENOMNT; }

    let path_str = match core::str::from_utf8(path) {
        Ok(s)  => s,
        Err(_) => return EINVAL,
    };
    let (ino_num, inode) = match resolve_path(path_str) {
        Some(r) => r,
        None    => return ENOENT,
    };
    if inode.flags & INODE_DIR != 0 { return EISDIR; }

    let mut st = STATE.lock();
    for (fd, slot) in st.fds.iter_mut().enumerate() {
        if slot.is_none() {
            *slot = Some(OpenFile { ino_num, inode, offset: 0, writable: false });
            return fd as i64;
        }
    }
    EMFILE
}

/// Read up to `buf.len()` bytes from `fd` at its current offset.
pub fn read(fd: u64, buf: &mut [u8]) -> i64 {
    if !is_mounted() { return ENOMNT; }
    if fd as usize >= MAX_FDS { return EBADF; }

    // Copy inode+offset out before releasing the lock; disk I/O via hlt
    // requires interrupts enabled, but SpinLock holds cli for its duration.
    let (inode, offset) = {
        let mut st = STATE.lock();
        match st.fds[fd as usize].as_mut() {
            Some(f) => (f.inode, f.offset),
            None    => return EBADF,
        }
    };

    let n = read_file_data(&inode, offset, buf);

    let mut st = STATE.lock();
    if let Some(Some(of)) = st.fds.get_mut(fd as usize) {
        of.offset += n as u64;
    }
    n as i64
}

/// Release `fd`.
pub fn close(fd: u64) -> i64 {
    if fd as usize >= MAX_FDS { return EBADF; }
    let mut st = STATE.lock();
    if st.fds[fd as usize].is_none() { return EBADF; }
    st.fds[fd as usize] = None;
    0
}

/// Fill `out` with stat info for `path`. Returns `true` on success.
pub fn stat_path(path: &[u8], out: &mut Stat) -> bool {
    if !is_mounted() { return false; }
    let path_str = match core::str::from_utf8(path) {
        Ok(s)  => s,
        Err(_) => return false,
    };
    match resolve_path(path_str) {
        Some((_, inode)) => { *out = Stat::from(&inode); true }
        None             => false,
    }
}

/// Read an entire file by path into a `Vec<u8>`. Returns `None` on error.
/// Capped at 32 MiB to guard against runaway allocations.
pub fn load_file(path: &str) -> Option<Vec<u8>> {
    let (_, inode) = resolve_path(path)?;
    if inode.flags & INODE_DIR != 0 { return None; }
    let size = inode.size as usize;
    if size == 0 || size > 32 * 1024 * 1024 { return None; }
    let mut buf = alloc::vec![0u8; size];
    let n = read_file_data(&inode, 0, &mut buf);
    if n < size { return None; }
    Some(buf)
}

/// Return directory entries for `path`, or `None` on error.
pub fn readdir_path(path: &[u8]) -> Option<Vec<DirEntry>> {
    if !is_mounted() { return None; }
    let path_str = match core::str::from_utf8(path) {
        Ok(s)  => s,
        Err(_) => return None,
    };
    let (_, inode) = resolve_path(path_str)?;
    if inode.flags & INODE_DIR == 0 { return None; }
    Some(scan_dir(&inode))
}

/// Write `buf` to the fd's current offset (fd must be opened writable).
pub fn write(fd: u64, buf: &[u8]) -> i64 {
    let (ino_num, inode, total_blocks) = {
        let st = STATE.lock();
        if !st.mounted { return ENOMNT; }
        let of = match st.fds.get(fd as usize).and_then(|s| s.as_ref()) {
            Some(f) => f,
            None    => return EBADF,
        };
        if !of.writable { return EBADF; }
        (of.ino_num, of.inode, st.total_blocks)
    };
    if buf.is_empty() { return 0; }
    let mut inode = inode;
    inode.mtime = crate::time::epoch_ms();
    if !append_to_file(ino_num, &mut inode, buf, total_blocks) {
        return ENOSPC;
    }
    let mut st = STATE.lock();
    if let Some(Some(of)) = st.fds.get_mut(fd as usize) {
        of.inode  = inode;
        of.offset = inode.size;
    }
    buf.len() as i64
}

/// Create a new empty regular file at `path`. Returns a writable fd (≥ 0).
pub fn create(path: &[u8]) -> i64 {
    let total_blocks = {
        let st = STATE.lock();
        if !st.mounted { return ENOMNT; }
        st.total_blocks
    };
    let path_str = match core::str::from_utf8(path) {
        Ok(s)  => s,
        Err(_) => return EINVAL,
    };
    if path_str.is_empty() { return EINVAL; }
    if resolve_path(path_str).is_some() { return EEXIST; }

    let (parent_str, name) = split_parent(path_str);
    if name.is_empty() { return EINVAL; }

    let (par_ino_num, par_inode) = match resolve_path(parent_str) {
        Some(r) => r,
        None    => return ENOENT,
    };
    if par_inode.flags & INODE_DIR == 0 { return ENOTDIR; }

    let new_ino = match alloc_inode() {
        Some(i) => i,
        None    => return ENOSPC,
    };
    let now = crate::time::epoch_ms();
    let new_inode = Inode {
        flags:        INODE_USED,
        mode:         0o644,
        uid:          0, gid: 0, nlink: 1,
        size:         0, blocks: 0,
        mtime:        now, ctime: now,
        ovfl_block:   0, extent_count: 0,
        extents:      [[0u8; EXTENT_SIZE]; INLINE_EXTENTS],
    };
    if !write_inode(new_ino, &new_inode) { return ENOSPC; }

    if !add_dir_entry(par_ino_num, new_ino, name, FT_REG, total_blocks) {
        let _ = free_inode(new_ino);
        return ENOSPC;
    }

    let mut st = STATE.lock();
    for (fd, slot) in st.fds.iter_mut().enumerate() {
        if slot.is_none() {
            *slot = Some(OpenFile { ino_num: new_ino, inode: new_inode, offset: 0, writable: true });
            return fd as i64;
        }
    }
    EMFILE
}

/// Create a directory at `path`. Parent must exist. Returns 0 on success.
pub fn mkdir(path: &[u8]) -> i64 {
    let total_blocks = {
        let st = STATE.lock();
        if !st.mounted { return ENOMNT; }
        st.total_blocks
    };
    let path_str = match core::str::from_utf8(path) {
        Ok(s)  => s,
        Err(_) => return EINVAL,
    };
    if path_str.is_empty() { return EINVAL; }
    if resolve_path(path_str).is_some() { return EEXIST; }

    let (parent_str, name) = split_parent(path_str);
    if name.is_empty() { return EINVAL; }

    let (par_ino_num, par_inode) = match resolve_path(parent_str) {
        Some(r) => r,
        None    => return ENOENT,
    };
    if par_inode.flags & INODE_DIR == 0 { return ENOTDIR; }

    let new_ino = match alloc_inode() {
        Some(i) => i,
        None    => return ENOSPC,
    };
    let now = crate::time::epoch_ms();
    let new_inode = Inode {
        flags:       INODE_USED | INODE_DIR,
        mode:        0o755,
        uid:         0, gid: 0, nlink: 1,
        size:        0, blocks: 0,
        mtime:       now, ctime: now,
        ovfl_block:  0, extent_count: 0,
        extents:     [[0u8; EXTENT_SIZE]; INLINE_EXTENTS],
    };
    if !write_inode(new_ino, &new_inode) { return ENOSPC; }

    if !add_dir_entry(par_ino_num, new_ino, name, FT_DIR, total_blocks) {
        let _ = free_inode(new_ino);
        return ENOSPC;
    }
    0
}

/// Seek within an open file descriptor. `whence`: 0=SET, 1=CUR, 2=END.
/// Returns new offset (≥ 0) or a negative error code.
pub fn seek(fd: u64, offset: i64, whence: u32) -> i64 {
    if !is_mounted() { return ENOMNT; }
    if fd as usize >= MAX_FDS { return EBADF; }
    let mut st = STATE.lock();
    let of = match st.fds[fd as usize].as_mut() {
        Some(f) => f,
        None    => return EBADF,
    };
    let size = of.inode.size as i64;
    let new_offset: i64 = match whence {
        0 => offset,                     // SEEK_SET
        1 => of.offset as i64 + offset,  // SEEK_CUR
        2 => size + offset,              // SEEK_END
        _ => return EINVAL,
    };
    if new_offset < 0 { return EINVAL; }
    of.offset = new_offset as u64;
    new_offset
}

/// Rename (or move) a regular file. Returns 0 on success or a negative error code.
/// Directory rename is not supported.
pub fn rename(old_path: &[u8], new_path: &[u8]) -> i64 {
    let total_blocks = {
        let st = STATE.lock();
        if !st.mounted { return ENOMNT; }
        st.total_blocks
    };
    let old_str = match core::str::from_utf8(old_path) {
        Ok(s)  => s,
        Err(_) => return EINVAL,
    };
    let new_str = match core::str::from_utf8(new_path) {
        Ok(s)  => s,
        Err(_) => return EINVAL,
    };
    if old_str.is_empty() || new_str.is_empty() { return EINVAL; }

    let (old_ino, old_inode) = match resolve_path(old_str) {
        Some(r) => r,
        None    => return ENOENT,
    };
    if old_inode.flags & INODE_DIR != 0 { return EISDIR; }

    if resolve_path(new_str).is_some() { return EEXIST; }

    let (old_par_str, old_name) = split_parent(old_str);
    let (new_par_str, new_name) = split_parent(new_str);
    if old_name.is_empty() || new_name.is_empty() { return EINVAL; }

    let (old_par_ino, _) = match resolve_path(old_par_str) {
        Some(r) => r,
        None    => return ENOENT,
    };
    let (new_par_ino, new_par_inode) = match resolve_path(new_par_str) {
        Some(r) => r,
        None    => return ENOENT,
    };
    if new_par_inode.flags & INODE_DIR == 0 { return ENOTDIR; }

    if !add_dir_entry(new_par_ino, old_ino, new_name, FT_REG, total_blocks) {
        return ENOSPC;
    }
    if !remove_dir_entry(old_par_ino, old_name) {
        remove_dir_entry(new_par_ino, new_name);
        return ENOENT;
    }
    0
}

/// Delete the regular file at `path`, freeing its blocks and inode.
pub fn unlink(path: &[u8]) -> i64 {
    if !is_mounted() { return ENOMNT; }
    let path_str = match core::str::from_utf8(path) {
        Ok(s)  => s,
        Err(_) => return EINVAL,
    };
    if path_str.is_empty() { return EINVAL; }

    let (parent_str, name) = split_parent(path_str);
    if name.is_empty() || name == "." || name == ".." { return EINVAL; }

    let (target_ino, target_inode) = match resolve_path(path_str) {
        Some(r) => r,
        None    => return ENOENT,
    };
    if target_inode.flags & INODE_DIR != 0 { return EISDIR; }

    let (par_ino_num, _) = match resolve_path(parent_str) {
        Some(r) => r,
        None    => return ENOENT,
    };

    if !remove_dir_entry(par_ino_num, name) { return ENOENT; }
    free_inode_blocks(&target_inode);
    free_inode(target_ino);
    0
}
