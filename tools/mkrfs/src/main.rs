/// mkrfs — format a raw disk image with the RFS_V1 filesystem
///
/// Usage:
///   mkrfs <image> <size>  [<src-dir>]
///   mkrfs disk.img 64M
///   mkrfs disk.img 64M rootfs/
///
/// Size suffixes: K, M, G (powers of 1024).
///
/// On-disk layout
/// ──────────────
/// Block 0 : Superblock (4096 bytes, first 64 used)
/// Block 1 : Block bitmap (4096 bytes = 32768 bits = 32768 blocks addressable)
/// Block 2–33 : Inode table (32 blocks × 32 inodes/block = 1024 inodes, 128 B each)
/// Block 34+ : Data blocks
///
/// Inode 0 : root directory
///
/// All numbers little-endian.

use std::fs::{File, OpenOptions, read_dir, read_link, symlink_metadata};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::MetadataExt;
use std::path::Path;

// ── Constants ────────────────────────────────────────────────────────────────

const BLOCK_SIZE:      usize = 4096;

const MAGIC: &[u8; 8] = b"RFS_V1\0\0";

const BITMAP_BLOCK:    u32 = 1;
const INODE_START:     u32 = 2;
const INODE_BLOCKS:    u32 = 32;
const INODE_COUNT:     u32 = INODE_BLOCKS * INODES_PER_BLOCK;
const INODES_PER_BLOCK: u32 = (BLOCK_SIZE / INODE_SIZE) as u32; // 32
const INODE_SIZE:      usize = 128;
const DATA_START:      u32 = INODE_START + INODE_BLOCKS; // 34

const INLINE_EXTENTS:  usize = 4;   // extents in inode
const OVFL_EXTENTS:    usize = 255; // extents in overflow block
const EXTENT_SIZE:     usize = 16;

const ROOT_INODE: u32 = 0;

// Inode flags
const INODE_USED:    u32 = 1 << 0;
const INODE_DIR:     u32 = 1 << 1;
const INODE_SYMLINK: u32 = 1 << 2;
const INODE_FAST_SYM:u32 = 1 << 3; // target stored inline (≤64 bytes)

// File-type byte in dir entries
const FT_REG:     u8 = 1;
const FT_DIR:     u8 = 2;
const FT_SYMLINK: u8 = 3;

// Overflow block header size (next:u32 + pad[12])
const OVFL_HDR: usize = 16;

// ── Serialisation helpers ────────────────────────────────────────────────────

fn put_u16(buf: &mut [u8], off: usize, v: u16) {
    buf[off..off+2].copy_from_slice(&v.to_le_bytes());
}
fn put_u32(buf: &mut [u8], off: usize, v: u32) {
    buf[off..off+4].copy_from_slice(&v.to_le_bytes());
}
fn put_u64(buf: &mut [u8], off: usize, v: u64) {
    buf[off..off+8].copy_from_slice(&v.to_le_bytes());
}
fn get_u32(buf: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(buf[off..off+4].try_into().unwrap())
}

// ── Disk I/O ─────────────────────────────────────────────────────────────────

struct Disk {
    file: File,
}

impl Disk {
    fn open(path: &str, size_bytes: u64) -> Self {
        let file = OpenOptions::new()
            .read(true).write(true).create(true).truncate(true)
            .open(path)
            .unwrap_or_else(|e| panic!("open {path}: {e}"));
        file.set_len(size_bytes)
            .unwrap_or_else(|e| panic!("set_len: {e}"));
        Disk { file }
    }

    fn read_block(&mut self, blk: u32) -> [u8; BLOCK_SIZE] {
        let mut buf = [0u8; BLOCK_SIZE];
        self.file.seek(SeekFrom::Start(blk as u64 * BLOCK_SIZE as u64))
            .unwrap_or_else(|e| panic!("seek blk {blk}: {e}"));
        self.file.read_exact(&mut buf)
            .unwrap_or_else(|e| panic!("read blk {blk}: {e}"));
        buf
    }

    fn write_block(&mut self, blk: u32, buf: &[u8; BLOCK_SIZE]) {
        self.file.seek(SeekFrom::Start(blk as u64 * BLOCK_SIZE as u64))
            .unwrap_or_else(|e| panic!("seek blk {blk}: {e}"));
        self.file.write_all(buf)
            .unwrap_or_else(|e| panic!("write blk {blk}: {e}"));
    }
}

// ── Block allocator ──────────────────────────────────────────────────────────

struct Allocator {
    bitmap: [u8; BLOCK_SIZE], // in-memory bitmap
    next:   u32,              // next block to try
    total:  u32,
}

impl Allocator {
    fn new(total_blocks: u32) -> Self {
        let mut bitmap = [0u8; BLOCK_SIZE];
        // Mark blocks 0..DATA_START as used.
        for b in 0..DATA_START {
            let byte = (b / 8) as usize;
            let bit  = b % 8;
            bitmap[byte] |= 1 << bit;
        }
        Allocator { bitmap, next: DATA_START, total: total_blocks }
    }

    fn alloc(&mut self) -> u32 {
        let start = self.next;
        let mut b = start;
        loop {
            if b >= self.total {
                b = DATA_START; // wrap
            }
            let byte = (b / 8) as usize;
            let bit  = b % 8;
            if self.bitmap[byte] & (1 << bit) == 0 {
                self.bitmap[byte] |= 1 << bit;
                self.next = b + 1;
                return b;
            }
            b += 1;
            if b == start {
                panic!("mkrfs: disk full");
            }
        }
    }

    fn write_bitmap(&self, disk: &mut Disk) {
        disk.write_block(BITMAP_BLOCK, &self.bitmap);
    }
}

// ── Inode table ──────────────────────────────────────────────────────────────

/// In-memory inode (matches the 128-byte on-disk layout exactly).
#[derive(Clone, Default)]
struct Inode {
    flags:        u32,
    mode:         u16,
    _pad0:        u16,
    uid:          u32,
    gid:          u32,
    nlink:        u32,
    size:         u64,
    blocks:       u64, // allocated disk blocks (not block-equivalent of size)
    mtime:        u64,
    ctime:        u64,
    ovfl_block:   u32, // first overflow extent block, or 0
    extent_count: u16,
    _pad1:        u16,
    extents:      [[u8; EXTENT_SIZE]; INLINE_EXTENTS],
    _pad2:        [u8; 4],
}

impl Inode {
    fn serialise(&self) -> [u8; INODE_SIZE] {
        let mut b = [0u8; INODE_SIZE];
        put_u32(&mut b, 0,  self.flags);
        put_u16(&mut b, 4,  self.mode);
        // b[6..8] = 0
        put_u32(&mut b, 8,  self.uid);
        put_u32(&mut b, 12, self.gid);
        put_u32(&mut b, 16, self.nlink);
        put_u64(&mut b, 20, self.size);
        put_u64(&mut b, 28, self.blocks);
        put_u64(&mut b, 36, self.mtime);
        put_u64(&mut b, 44, self.ctime);
        put_u32(&mut b, 52, self.ovfl_block);
        put_u16(&mut b, 56, self.extent_count);
        // b[58..60] = 0
        for (i, ext) in self.extents.iter().enumerate() {
            b[60 + i*EXTENT_SIZE .. 60 + (i+1)*EXTENT_SIZE].copy_from_slice(ext);
        }
        // b[124..128] = 0
        b
    }
}

fn make_extent(logical: u32, physical: u32, count: u32, flags: u32) -> [u8; EXTENT_SIZE] {
    let mut e = [0u8; EXTENT_SIZE];
    put_u32(&mut e, 0,  logical);
    put_u32(&mut e, 4,  physical);
    put_u32(&mut e, 8,  count);
    put_u32(&mut e, 12, flags);
    e
}

struct InodeTable {
    inodes: Vec<Inode>,
    next_free: u32,
}

impl InodeTable {
    fn new() -> Self {
        let inodes = vec![Inode::default(); INODE_COUNT as usize];
        InodeTable { inodes, next_free: 0 }
    }

    fn alloc(&mut self) -> u32 {
        let start = self.next_free;
        loop {
            if self.next_free >= INODE_COUNT {
                panic!("mkrfs: inode table full");
            }
            if self.inodes[self.next_free as usize].flags & INODE_USED == 0 {
                let ino = self.next_free;
                self.next_free += 1;
                return ino;
            }
            self.next_free += 1;
            if self.next_free == start {
                panic!("mkrfs: inode table full");
            }
        }
    }

    fn get_mut(&mut self, ino: u32) -> &mut Inode {
        &mut self.inodes[ino as usize]
    }

    fn write_all(&self, disk: &mut Disk) {
        for blk_off in 0..INODE_BLOCKS {
            let mut buf = [0u8; BLOCK_SIZE];
            let base = (blk_off * INODES_PER_BLOCK) as usize;
            for i in 0..INODES_PER_BLOCK as usize {
                let ino_idx = base + i;
                let serialised = self.inodes[ino_idx].serialise();
                buf[i*INODE_SIZE..(i+1)*INODE_SIZE].copy_from_slice(&serialised);
            }
            disk.write_block(INODE_START + blk_off, &buf);
        }
    }
}

// ── Extent attachment ─────────────────────────────────────────────────────────

/// Attach a run of blocks to an inode.
/// Uses inline extents first; spills into overflow blocks if needed.
fn attach_extent(
    ino:   &mut Inode,
    logical: u32,
    physical: u32,
    count: u32,
    disk:  &mut Disk,
    alloc: &mut Allocator,
) {
    let extent = make_extent(logical, physical, count, 0);
    let ec = ino.extent_count as usize;

    if ec < INLINE_EXTENTS {
        ino.extents[ec] = extent;
        ino.extent_count += 1;
        return;
    }

    // Need an overflow block.  Walk the chain to the last one.
    let mut ovfl = ino.ovfl_block;
    if ovfl == 0 {
        // Allocate first overflow block.
        let new_blk = alloc.alloc();
        ino.ovfl_block = new_blk;
        ovfl = new_blk;
        // Zero-initialise it on disk (already zeroed by sparse file, but be explicit).
        disk.write_block(ovfl, &[0u8; BLOCK_SIZE]);
    }

    loop {
        let buf = disk.read_block(ovfl);
        let next = get_u32(&buf, 0);
        // Count extents in this block: (BLOCK_SIZE - OVFL_HDR) / EXTENT_SIZE = 255
        // Find first zero extent (flags==0 && count==0 suffices — but we track by a simple count
        // embedded at offset 4 of the overflow header).
        let used = get_u32(&buf, 4) as usize;

        if used < OVFL_EXTENTS {
            // Write extent here.
            let off = OVFL_HDR + used * EXTENT_SIZE;
            let mut new_buf = buf;
            new_buf[off..off+EXTENT_SIZE].copy_from_slice(&extent);
            put_u32(&mut new_buf, 4, (used + 1) as u32);
            disk.write_block(ovfl, &new_buf);
            ino.extent_count += 1;
            return;
        }

        if next == 0 {
            // Chain a new overflow block.
            let new_blk = alloc.alloc();
            let mut new_buf = buf;
            put_u32(&mut new_buf, 0, new_blk);
            disk.write_block(ovfl, &new_buf);
            disk.write_block(new_blk, &[0u8; BLOCK_SIZE]);
            ovfl = new_blk;
        } else {
            ovfl = next;
        }
    }
}

// ── File creation ─────────────────────────────────────────────────────────────

/// Write `data` into data blocks and return the allocated inode number.
fn create_file(
    data:  &[u8],
    mode:  u16,
    uid:   u32,
    gid:   u32,
    mtime: u64,
    inodes: &mut InodeTable,
    disk:   &mut Disk,
    alloc:  &mut Allocator,
) -> u32 {
    let ino_num = inodes.alloc();
    let size = data.len() as u64;
    let mut logical = 0u32;
    let mut written = 0usize;
    let mut total_blks = 0u64;

    // Write data in block-sized chunks, grouping consecutive allocations into extents.
    while written < data.len() {
        let chunk = (data.len() - written).min(BLOCK_SIZE);
        let blk = alloc.alloc();
        total_blks += 1;

        // Copy data into block.
        let mut buf = [0u8; BLOCK_SIZE];
        buf[..chunk].copy_from_slice(&data[written..written+chunk]);
        disk.write_block(blk, &buf);

        // Coalesce into previous extent if physically contiguous.
        let ino = inodes.get_mut(ino_num);
        let ec = ino.extent_count as usize;
        let coalesced = if ec > 0 && ec <= INLINE_EXTENTS {
            let last = &mut ino.extents[ec-1];
            let l_phys = get_u32(last, 4);
            let l_cnt  = get_u32(last, 8);
            if l_phys + l_cnt == blk {
                put_u32(last, 8, l_cnt + 1);
                true
            } else { false }
        } else { false };

        if !coalesced {
            let ino = inodes.get_mut(ino_num);
            attach_extent(ino, logical, blk, 1, disk, alloc);
        }

        written += chunk;
        logical += 1;
    }

    let ino = inodes.get_mut(ino_num);
    ino.flags  = INODE_USED;
    ino.mode   = mode;
    ino.uid    = uid;
    ino.gid    = gid;
    ino.nlink  = 1;
    ino.size   = size;
    ino.blocks = total_blks;
    ino.mtime  = mtime;
    ino.ctime  = mtime;

    ino_num
}

/// Create a symlink inode.  Targets ≤64 bytes are stored inline (fast symlink).
fn create_symlink(
    target: &[u8],
    uid:    u32,
    gid:    u32,
    mtime:  u64,
    inodes: &mut InodeTable,
    disk:   &mut Disk,
    alloc:  &mut Allocator,
) -> u32 {
    const MAX_FAST: usize = 64;
    if target.len() <= MAX_FAST {
        let ino_num = inodes.alloc();
        let ino = inodes.get_mut(ino_num);
        ino.flags  = INODE_USED | INODE_SYMLINK | INODE_FAST_SYM;
        ino.mode   = 0o120_777;
        ino.uid    = uid;
        ino.gid    = gid;
        ino.nlink  = 1;
        ino.size   = target.len() as u64;
        ino.mtime  = mtime;
        ino.ctime  = mtime;
        // Inline target: first 64 bytes of the extents array.
        let dst = unsafe {
            core::slice::from_raw_parts_mut(ino.extents.as_mut_ptr() as *mut u8, 64)
        };
        dst[..target.len()].copy_from_slice(target);
        ino_num
    } else {
        // Slow symlink: store target as regular file data.
        let ino_num = create_file(target, 0o120_777, uid, gid, mtime, inodes, disk, alloc);
        let ino = inodes.get_mut(ino_num);
        ino.flags |= INODE_SYMLINK;
        ino.flags &= !INODE_FAST_SYM;
        ino_num
    }
}

// ── Directory creation ────────────────────────────────────────────────────────

/// Build the raw directory data block(s) for a set of (name, inode, file_type) entries.
fn layout_dir_entries(entries: &[(String, u32, u8)]) -> Vec<u8> {
    // Each entry: inode(4) + rec_len(2) + name_len(1) + file_type(1) + name + pad to 4.
    // Entries are packed into BLOCK_SIZE pages; last entry in a block fills the block.

    let mut pages: Vec<[u8; BLOCK_SIZE]> = Vec::new();
    let mut page = [0u8; BLOCK_SIZE];
    let mut pos  = 0usize; // position within current page

    for (idx, (name, ino, ft)) in entries.iter().enumerate() {
        let name_bytes = name.as_bytes();
        let name_len   = name_bytes.len().min(255);
        let base_size  = 4 + 2 + 1 + 1 + name_len; // inode+rec_len+name_len+ft+name
        let aligned    = (base_size + 3) & !3;

        let is_last_global = idx == entries.len() - 1;
        let fits = pos + aligned <= BLOCK_SIZE;

        if !fits {
            // Extend the last entry in this page to fill the block.
            // Find where the last entry started: scan from the beginning.
            let last_entry_off = find_last_entry_off(&page, pos);
            let fill = BLOCK_SIZE - last_entry_off;
            put_u16(&mut page, last_entry_off + 4, fill as u16);
            pages.push(page);
            page = [0u8; BLOCK_SIZE];
            pos  = 0;
        }

        // Write entry at `pos`.
        let rec_len = if is_last_global && pos + aligned + 8 > BLOCK_SIZE {
            // Last entry globally: extend to fill block.
            BLOCK_SIZE - pos
        } else if pos + aligned == BLOCK_SIZE {
            BLOCK_SIZE - pos
        } else {
            aligned
        };

        let rec_len = rec_len.max(aligned);

        put_u32(&mut page, pos,   *ino);
        put_u16(&mut page, pos+4, rec_len as u16);
        page[pos+6] = name_len as u8;
        page[pos+7] = *ft;
        page[pos+8..pos+8+name_len].copy_from_slice(&name_bytes[..name_len]);

        pos += rec_len;

        if is_last_global {
            // Fill remaining space in last page by extending last entry.
            if pos < BLOCK_SIZE {
                let last_off = find_last_entry_off(&page, pos);
                let fill = BLOCK_SIZE - last_off;
                put_u16(&mut page, last_off + 4, fill as u16);
            }
            pages.push(page);
            break;
        }
    }

    if pages.is_empty() {
        // Empty directory: single page with no entries (shouldn't happen — always has . and ..)
        pages.push([0u8; BLOCK_SIZE]);
    }

    let mut out = Vec::with_capacity(pages.len() * BLOCK_SIZE);
    for p in pages {
        out.extend_from_slice(&p);
    }
    out
}

/// Scan a partially-filled page and return the byte offset of the last valid entry.
fn find_last_entry_off(page: &[u8; BLOCK_SIZE], used: usize) -> usize {
    let mut off = 0usize;
    let mut last = 0usize;
    while off < used {
        let rec_len = u16::from_le_bytes(page[off+4..off+6].try_into().unwrap()) as usize;
        if rec_len == 0 { break; }
        last = off;
        off += rec_len;
    }
    last
}

/// Allocate a directory inode, write its entries, and return the inode number.
// ── Recursive population ──────────────────────────────────────────────────────

fn populate(
    src:     &Path,
    dir_ino: u32,
    parent:  u32,
    inodes:  &mut InodeTable,
    disk:    &mut Disk,
    alloc:   &mut Allocator,
) {
    let mut entries: Vec<(String, u32, u8)> = Vec::new();
    entries.push((".".to_string(),  dir_ino, FT_DIR));
    entries.push(("..".to_string(), parent,  FT_DIR));

    for entry in read_dir(src).unwrap_or_else(|e| panic!("readdir {}: {e}", src.display())) {
        let entry = entry.unwrap();
        let name  = entry.file_name().to_string_lossy().to_string();
        let path  = entry.path();
        let meta  = symlink_metadata(&path)
            .unwrap_or_else(|e| panic!("stat {}: {e}", path.display()));
        let mtime = meta.mtime() as u64;
        let uid   = meta.uid();
        let gid   = meta.gid();
        let mode  = (meta.mode() & 0o7777) as u16;
        let ft    = meta.file_type();

        let child_ino = if ft.is_symlink() {
            let target = read_link(&path)
                .unwrap_or_else(|e| panic!("readlink {}: {e}", path.display()));
            create_symlink(
                target.to_string_lossy().as_bytes(),
                uid, gid, mtime,
                inodes, disk, alloc,
            )
        } else if ft.is_dir() {
            // Allocate inode number first, then recurse.
            let child = inodes.alloc();
            // Mark placeholder so alloc skips it.
            inodes.get_mut(child).flags = INODE_USED | INODE_DIR;
            // Recurse: will fill actual directory entries.
            populate_with_ino(&path, child, dir_ino, inodes, disk, alloc, uid, gid, mode, mtime);
            child
        } else {
            let data = std::fs::read(&path)
                .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
            create_file(&data, mode, uid, gid, mtime, inodes, disk, alloc)
        };

        let ft_byte = if ft.is_symlink() { FT_SYMLINK }
                      else if ft.is_dir() { FT_DIR }
                      else { FT_REG };

        entries.push((name, child_ino, ft_byte));

        // Update nlink for directories.
        if ft.is_dir() {
            inodes.get_mut(dir_ino).nlink += 1;
        }
    }

    // Now write this directory's data blocks.
    write_dir_data(dir_ino, entries, inodes, disk, alloc);
}

/// Like `populate` but the inode number is pre-allocated.
fn populate_with_ino(
    src:     &Path,
    dir_ino: u32,
    parent:  u32,
    inodes:  &mut InodeTable,
    disk:    &mut Disk,
    alloc:   &mut Allocator,
    uid:     u32,
    gid:     u32,
    mode:    u16,
    mtime:   u64,
) {
    let mut entries: Vec<(String, u32, u8)> = Vec::new();
    entries.push((".".to_string(),  dir_ino, FT_DIR));
    entries.push(("..".to_string(), parent,  FT_DIR));

    for entry in read_dir(src).unwrap_or_else(|e| panic!("readdir {}: {e}", src.display())) {
        let entry = entry.unwrap();
        let name  = entry.file_name().to_string_lossy().to_string();
        let path  = entry.path();
        let meta  = symlink_metadata(&path)
            .unwrap_or_else(|e| panic!("stat {}: {e}", path.display()));
        let cmtime = meta.mtime() as u64;
        let cuid   = meta.uid();
        let cgid   = meta.gid();
        let cmode  = (meta.mode() & 0o7777) as u16;
        let ft     = meta.file_type();

        let child_ino = if ft.is_symlink() {
            let target = read_link(&path)
                .unwrap_or_else(|e| panic!("readlink {}: {e}", path.display()));
            create_symlink(
                target.to_string_lossy().as_bytes(),
                cuid, cgid, cmtime,
                inodes, disk, alloc,
            )
        } else if ft.is_dir() {
            let child = inodes.alloc();
            inodes.get_mut(child).flags = INODE_USED | INODE_DIR;
            populate_with_ino(&path, child, dir_ino, inodes, disk, alloc, cuid, cgid, cmode, cmtime);
            child
        } else {
            let data = std::fs::read(&path)
                .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
            create_file(&data, cmode, cuid, cgid, cmtime, inodes, disk, alloc)
        };

        let ft_byte = if ft.is_symlink() { FT_SYMLINK }
                      else if ft.is_dir() { FT_DIR }
                      else { FT_REG };

        entries.push((name, child_ino, ft_byte));

        if ft.is_dir() {
            inodes.get_mut(dir_ino).nlink += 1;
        }
    }

    write_dir_data(dir_ino, entries, inodes, disk, alloc);

    // Patch inode metadata (was placeholder).
    let ino = inodes.get_mut(dir_ino);
    ino.mode  = mode;
    ino.uid   = uid;
    ino.gid   = gid;
    ino.mtime = mtime;
    ino.ctime = mtime;
}

/// Write directory entries into data blocks and attach extents to the inode.
fn write_dir_data(
    dir_ino: u32,
    entries: Vec<(String, u32, u8)>,
    inodes:  &mut InodeTable,
    disk:    &mut Disk,
    alloc:   &mut Allocator,
) {
    let data = layout_dir_entries(&entries);
    let n_blocks = (data.len() + BLOCK_SIZE - 1) / BLOCK_SIZE;
    let mut logical = 0u32;
    let mut total_blks = 0u64;

    for bi in 0..n_blocks {
        let blk = alloc.alloc();
        total_blks += 1;
        let mut buf = [0u8; BLOCK_SIZE];
        let off = bi * BLOCK_SIZE;
        let end = (off + BLOCK_SIZE).min(data.len());
        buf[..end-off].copy_from_slice(&data[off..end]);
        disk.write_block(blk, &buf);

        // Try coalesce.
        let ino = inodes.get_mut(dir_ino);
        let ec = ino.extent_count as usize;
        let coalesced = if ec > 0 && ec <= INLINE_EXTENTS {
            let last = &mut ino.extents[ec-1];
            let l_phys = get_u32(last, 4);
            let l_cnt  = get_u32(last, 8);
            if l_phys + l_cnt == blk {
                put_u32(last, 8, l_cnt + 1);
                true
            } else { false }
        } else { false };

        if !coalesced {
            let ino = inodes.get_mut(dir_ino);
            attach_extent(ino, logical, blk, 1, disk, alloc);
        }

        logical += 1;
    }

    let ino = inodes.get_mut(dir_ino);
    ino.size   = data.len() as u64;
    ino.blocks += total_blks;
    ino.nlink   = ino.nlink.max(2); // . and ..
}

// ── Superblock ────────────────────────────────────────────────────────────────

fn write_superblock(disk: &mut Disk, total_blocks: u32, free_blocks: u32) {
    let mut buf = [0u8; BLOCK_SIZE];
    buf[0..8].copy_from_slice(MAGIC);
    put_u32(&mut buf, 8,  1);            // version
    put_u32(&mut buf, 12, BLOCK_SIZE as u32);
    put_u32(&mut buf, 16, total_blocks);
    put_u32(&mut buf, 20, free_blocks);
    put_u32(&mut buf, 24, INODE_COUNT);
    put_u32(&mut buf, 28, ROOT_INODE);
    put_u32(&mut buf, 32, BITMAP_BLOCK);
    put_u32(&mut buf, 36, INODE_START);
    put_u32(&mut buf, 40, INODE_BLOCKS);
    put_u32(&mut buf, 44, DATA_START);
    disk.write_block(0, &buf);
}

// ── Size parsing ──────────────────────────────────────────────────────────────

fn parse_size(s: &str) -> u64 {
    let s = s.trim();
    if let Some(n) = s.strip_suffix('G').or_else(|| s.strip_suffix('g')) {
        n.parse::<u64>().expect("bad size") * 1024 * 1024 * 1024
    } else if let Some(n) = s.strip_suffix('M').or_else(|| s.strip_suffix('m')) {
        n.parse::<u64>().expect("bad size") * 1024 * 1024
    } else if let Some(n) = s.strip_suffix('K').or_else(|| s.strip_suffix('k')) {
        n.parse::<u64>().expect("bad size") * 1024
    } else {
        s.parse::<u64>().expect("bad size")
    }
}

// ── Free block count ──────────────────────────────────────────────────────────

fn count_free(bitmap: &[u8; BLOCK_SIZE], total: u32) -> u32 {
    let mut free = 0u32;
    for b in 0..total {
        let byte = (b / 8) as usize;
        let bit  = b % 8;
        if bitmap[byte] & (1 << bit) == 0 {
            free += 1;
        }
    }
    free
}

// ── main ──────────────────────────────────────────────────────────────────────

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("Usage: mkrfs <image> <size[K|M|G]> [<src-dir>]");
        std::process::exit(1);
    }

    let image_path  = &args[1];
    let size_bytes  = parse_size(&args[2]);
    let src_dir     = args.get(3);

    if size_bytes % BLOCK_SIZE as u64 != 0 {
        eprintln!("Size must be a multiple of {} bytes", BLOCK_SIZE);
        std::process::exit(1);
    }

    let total_blocks = (size_bytes / BLOCK_SIZE as u64) as u32;
    if total_blocks < DATA_START + 2 {
        eprintln!("Image too small (need at least {} blocks)", DATA_START + 2);
        std::process::exit(1);
    }

    println!("mkrfs: creating {image_path} ({size_bytes} bytes, {total_blocks} blocks)");

    let mut disk   = Disk::open(image_path, size_bytes);
    let mut alloc  = Allocator::new(total_blocks);
    let mut inodes = InodeTable::new();

    // Allocate root inode (inode 0).
    let root_ino = inodes.alloc();
    assert_eq!(root_ino, ROOT_INODE);
    // Mark placeholder so sub-calls skip it.
    inodes.get_mut(root_ino).flags = INODE_USED | INODE_DIR;

    if let Some(src) = src_dir {
        let src_path = Path::new(src);
        if !src_path.is_dir() {
            eprintln!("src-dir '{src}' is not a directory");
            std::process::exit(1);
        }
        let meta = std::fs::metadata(src_path).unwrap();
        use std::os::unix::fs::MetadataExt;
        let mtime = meta.mtime() as u64;
        let uid   = meta.uid();
        let gid   = meta.gid();
        let mode  = (meta.mode() & 0o7777) as u16;

        populate(src_path, root_ino, root_ino, &mut inodes, &mut disk, &mut alloc);

        let ino = inodes.get_mut(root_ino);
        ino.mode  = mode | 0o040_000; // ensure dir bit
        ino.uid   = uid;
        ino.gid   = gid;
        ino.mtime = mtime;
        ino.ctime = mtime;
        ino.nlink = ino.nlink.max(2);
    } else {
        // Empty filesystem: root has only . and ..
        let entries = vec![
            (".".to_string(),  root_ino, FT_DIR),
            ("..".to_string(), root_ino, FT_DIR),
        ];
        write_dir_data(root_ino, entries, &mut inodes, &mut disk, &mut alloc);
        let ino = inodes.get_mut(root_ino);
        ino.mode  = 0o040_755;
        ino.nlink = 2;
    }

    // Write metadata.
    alloc.write_bitmap(&mut disk);
    inodes.write_all(&mut disk);
    let free_blks = count_free(&alloc.bitmap, total_blocks);
    write_superblock(&mut disk, total_blocks, free_blks);

    println!(
        "mkrfs: done. {total_blocks} total blocks, {free_blks} free, {} inodes used.",
        inodes.next_free
    );
}
