//! The 128-byte inode (doc 06 §1–2). Packed 32 per inode-map leaf block.

use crate::layout::{
    rd_u16, rd_u32, rd_u64, wr_u16, wr_u32, wr_u64, BlockPtr, INLINE_SYMLINK_MAX, INODE_SIZE,
};

pub const FLAG_USED: u16 = 1 << 0;
pub const FLAG_FAST_SYMLINK: u16 = 1 << 1;

pub const S_IFMT: u16 = 0xF000;
pub const S_IFREG: u16 = 0x8000;
pub const S_IFDIR: u16 = 0x4000;
pub const S_IFLNK: u16 = 0xA000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Inode {
    pub mode: u16,
    pub flags: u16,
    pub uid: u32,
    pub gid: u32,
    pub nlink: u32,
    pub size: u64,
    pub blocks: u64,
    pub mtime: u64,
    pub ctime: u64,
    pub atime: u64,
    pub btime: u64,
    pub inode_gen: u64,
    pub bmap_height: u8,
    /// TODO(open): device nodes (doc 06 §1). Zero for now.
    pub rdev: u64,
    pub bmap_root: BlockPtr,
    /// Fast-symlink target bytes (doc 06 §1 symlinks). Occupies the on-disk
    /// span 80..128 (the `rdev` + `bmap_root` span) when `FAST_SYMLINK` is
    /// set; such an inode has NO block map — these bytes must never be
    /// interpreted as a `BlockPtr`. All-zero otherwise.
    pub inline: [u8; INLINE_SYMLINK_MAX],
}

impl Inode {
    /// Canonical free slot: all-zero 128 bytes, `USED = 0` (doc 06 §2).
    pub const FREE: Inode = Inode {
        mode: 0,
        flags: 0,
        uid: 0,
        gid: 0,
        nlink: 0,
        size: 0,
        blocks: 0,
        mtime: 0,
        ctime: 0,
        atime: 0,
        btime: 0,
        inode_gen: 0,
        bmap_height: 0,
        rdev: 0,
        bmap_root: BlockPtr::NULL,
        inline: [0; INLINE_SYMLINK_MAX],
    };

    pub fn is_used(&self) -> bool {
        self.flags & FLAG_USED != 0
    }
    pub fn is_dir(&self) -> bool {
        self.mode & S_IFMT == S_IFDIR
    }
    pub fn is_reg(&self) -> bool {
        self.mode & S_IFMT == S_IFREG
    }
    pub fn is_symlink(&self) -> bool {
        self.mode & S_IFMT == S_IFLNK
    }
    pub fn is_fast_symlink(&self) -> bool {
        self.flags & FLAG_FAST_SYMLINK != 0
    }

    pub fn encode(&self) -> [u8; INODE_SIZE] {
        let mut out = [0u8; INODE_SIZE];
        wr_u16(&mut out[0..], self.mode);
        wr_u16(&mut out[2..], self.flags);
        wr_u32(&mut out[4..], self.uid);
        wr_u32(&mut out[8..], self.gid);
        wr_u32(&mut out[12..], self.nlink);
        wr_u64(&mut out[16..], self.size);
        wr_u64(&mut out[24..], self.blocks);
        wr_u64(&mut out[32..], self.mtime);
        wr_u64(&mut out[40..], self.ctime);
        wr_u64(&mut out[48..], self.atime);
        wr_u64(&mut out[56..], self.btime);
        wr_u64(&mut out[64..], self.inode_gen);
        out[72] = self.bmap_height;
        if self.is_fast_symlink() {
            out[80..128].copy_from_slice(&self.inline);
        } else {
            wr_u64(&mut out[80..], self.rdev);
            out[96..128].copy_from_slice(&self.bmap_root.to_bytes());
        }
        out
    }

    pub fn decode(buf: &[u8]) -> Inode {
        let flags = rd_u16(&buf[2..]);
        let fast = flags & FLAG_FAST_SYMLINK != 0;
        let mut inline = [0u8; INLINE_SYMLINK_MAX];
        if fast {
            inline.copy_from_slice(&buf[80..128]);
        }
        Inode {
            mode: rd_u16(&buf[0..]),
            flags,
            uid: rd_u32(&buf[4..]),
            gid: rd_u32(&buf[8..]),
            nlink: rd_u32(&buf[12..]),
            size: rd_u64(&buf[16..]),
            blocks: rd_u64(&buf[24..]),
            mtime: rd_u64(&buf[32..]),
            ctime: rd_u64(&buf[40..]),
            atime: rd_u64(&buf[48..]),
            btime: rd_u64(&buf[56..]),
            inode_gen: rd_u64(&buf[64..]),
            bmap_height: buf[72],
            rdev: if fast { 0 } else { rd_u64(&buf[80..]) },
            bmap_root: if fast { BlockPtr::NULL } else { BlockPtr::from_bytes(&buf[96..128]) },
            inline,
        }
    }
}
