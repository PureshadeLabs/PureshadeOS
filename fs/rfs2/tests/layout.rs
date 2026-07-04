//! Byte-exact on-disk layout tests against the offset tables in docs 02, 03,
//! 06, 07.

mod common;

use rfs2::inode::{Inode, FLAG_FAST_SYMLINK, FLAG_USED, S_IFDIR, S_IFLNK};
use rfs2::layout::{
    rd_u16, rd_u32, rd_u64, BlockPtr, StaticHeader, BLOCK_SIZE, FIRST_DATA_BLOCK, SB_SLOT_A,
    SB_SLOT_B,
};
use rfs2::superblock::Superblock;
use rfs2::{dirent, mkfs, IdentityTransform};

use common::{opts, MemDev, UUID};

fn sample_header() -> StaticHeader {
    StaticHeader {
        format_version: 2,
        header_version: 1,
        block_size: 4096,
        total_blocks: 0x1122_3344_5566_7788,
        sb_slot_a: SB_SLOT_A,
        sb_slot_b: SB_SLOT_B,
        first_data_block: FIRST_DATA_BLOCK,
        uuid: UUID,
        feature_compat: 0xA1,
        feature_incompat: 0xB2,
        feature_ro_compat: 0xC3,
        kdf_algo: 1,
        kdf_salt: [0x55; 16],
        argon_m_cost: 65536,
        argon_t_cost: 3,
        argon_p: 1,
        dek_wrap_nonce: [0x66; 12],
        dek_wrapped: [0x77; 32],
        dek_wrap_tag: [0x88; 16],
        label: [0x4c; 64],
    }
}

#[test]
fn static_header_offsets_byte_exact() {
    // Offset table, doc 02 §3.
    let h = sample_header();
    let mut buf = vec![0u8; BLOCK_SIZE];
    h.encode(&mut buf);
    assert_eq!(&buf[0..8], b"RFS_V2\0\0");
    assert_eq!(rd_u16(&buf[8..]), 2); // format_version
    assert_eq!(rd_u16(&buf[10..]), 1); // header_version
    assert_eq!(rd_u32(&buf[12..]), 4096); // block_size
    assert_eq!(rd_u64(&buf[16..]), 0x1122_3344_5566_7788); // total_blocks
    assert_eq!(rd_u64(&buf[24..]), 1); // sb_slot_a
    assert_eq!(rd_u64(&buf[32..]), 2); // sb_slot_b
    assert_eq!(rd_u64(&buf[40..]), 3); // first_data_block
    assert_eq!(&buf[48..64], &UUID); // uuid
    assert_eq!(rd_u64(&buf[64..]), 0xA1); // feature_compat
    assert_eq!(rd_u64(&buf[72..]), 0xB2); // feature_incompat
    assert_eq!(rd_u64(&buf[80..]), 0xC3); // feature_ro_compat
    assert_eq!(buf[88], 1); // kdf_algo
    assert_eq!(&buf[89..96], &[0u8; 7]); // reserved
    assert_eq!(&buf[96..112], &[0x55; 16]); // kdf_salt
    assert_eq!(rd_u32(&buf[112..]), 65536); // argon_m_cost
    assert_eq!(rd_u32(&buf[116..]), 3); // argon_t_cost
    assert_eq!(rd_u32(&buf[120..]), 1); // argon_p
    assert_eq!(&buf[128..140], &[0x66; 12]); // dek_wrap_nonce
    assert_eq!(&buf[144..176], &[0x77; 32]); // dek_wrapped
    assert_eq!(&buf[176..192], &[0x88; 16]); // dek_wrap_tag
    assert_eq!(&buf[192..256], &[0x4c; 64]); // label
    assert!(buf[256..].iter().all(|&b| b == 0)); // reserved to end

    let back = StaticHeader::decode(&buf).unwrap();
    assert_eq!(back, h);
}

#[test]
fn block_ptr_is_32_bytes_le() {
    // Doc 02 §4: {block u64, gen u64, tag[16]}; null tested by block only.
    let p = BlockPtr { block: 0x0102_0304_0506_0708, gen: 0x1112_1314_1516_1718, tag: [0xEE; 16] };
    let b = p.to_bytes();
    assert_eq!(b.len(), 32);
    assert_eq!(rd_u64(&b[0..]), 0x0102_0304_0506_0708);
    assert_eq!(rd_u64(&b[8..]), 0x1112_1314_1516_1718);
    assert_eq!(&b[16..32], &[0xEE; 16]);
    assert_eq!(BlockPtr::from_bytes(&b), p);
    assert!(BlockPtr::NULL.is_null());
    assert_eq!(BlockPtr::NULL.to_bytes(), [0u8; 32]);
}

#[test]
fn superblock_payload_offsets_byte_exact() {
    // Offset table, doc 03 §2.
    let root = BlockPtr { block: 77, gen: 9, tag: [0xAB; 16] };
    let sb = Superblock {
        gen: 9,
        total_blocks: 4096,
        inode_map_root: root,
        inode_map_height: 2,
        next_inode: 42,
        inode_count: 17,
        block_count: 200,
        commit_time: 111_222_333,
        uuid: UUID,
    };
    let mut buf = vec![0u8; BLOCK_SIZE];
    sb.encode_payload(&mut buf);
    assert_eq!(&buf[0..8], b"RFSSB\0\0\0");
    assert_eq!(rd_u64(&buf[8..]), 9); // gen
    assert_eq!(rd_u64(&buf[16..]), 4096); // total_blocks
    assert_eq!(BlockPtr::from_bytes(&buf[24..56]), root); // inode_map_root
    assert_eq!(rd_u64(&buf[56..]), 2); // inode_map_height
    assert_eq!(rd_u64(&buf[64..]), 42); // next_inode
    assert_eq!(rd_u64(&buf[72..]), 17); // inode_count
    assert_eq!(rd_u64(&buf[80..]), 200); // block_count
    assert_eq!(rd_u64(&buf[88..]), 111_222_333); // commit_time
    assert_eq!(&buf[96..112], &UUID); // uuid
    assert!(buf[112..4080].iter().all(|&b| b == 0)); // reserved
    assert_eq!(Superblock::decode_payload(&buf).unwrap(), sb);
}

#[test]
fn inode_offsets_byte_exact() {
    // Offset table, doc 06 §1: 128 bytes total.
    let root = BlockPtr { block: 5, gen: 3, tag: [0xCD; 16] };
    let ino = Inode {
        mode: S_IFDIR | 0o755,
        flags: FLAG_USED,
        uid: 1000,
        gid: 2000,
        nlink: 3,
        size: 8192,
        blocks: 2,
        mtime: 1,
        ctime: 2,
        atime: 3,
        btime: 4,
        inode_gen: 5,
        bmap_height: 1,
        rdev: 0,
        bmap_root: root,
        inline: [0; 48],
    };
    let b = ino.encode();
    assert_eq!(b.len(), 128);
    assert_eq!(rd_u16(&b[0..]), S_IFDIR | 0o755); // mode
    assert_eq!(rd_u16(&b[2..]), FLAG_USED); // flags
    assert_eq!(rd_u32(&b[4..]), 1000); // uid
    assert_eq!(rd_u32(&b[8..]), 2000); // gid
    assert_eq!(rd_u32(&b[12..]), 3); // nlink
    assert_eq!(rd_u64(&b[16..]), 8192); // size
    assert_eq!(rd_u64(&b[24..]), 2); // blocks
    assert_eq!(rd_u64(&b[32..]), 1); // mtime
    assert_eq!(rd_u64(&b[40..]), 2); // ctime
    assert_eq!(rd_u64(&b[48..]), 3); // atime
    assert_eq!(rd_u64(&b[56..]), 4); // btime
    assert_eq!(rd_u64(&b[64..]), 5); // inode_gen
    assert_eq!(b[72], 1); // bmap_height
    assert_eq!(rd_u64(&b[80..]), 0); // rdev
    assert_eq!(BlockPtr::from_bytes(&b[96..128]), root); // bmap_root
    assert_eq!(Inode::decode(&b), ino);

    // All-zero slot is the canonical free inode (doc 06 §2).
    assert!(!Inode::decode(&[0u8; 128]).is_used());
    assert_eq!(Inode::FREE.encode(), [0u8; 128]);
}

#[test]
fn fast_symlink_inline_span_byte_exact() {
    // Doc 06 §1 symlinks: FAST_SYMLINK stores the target at bytes 80..128
    // (rdev + reserved + bmap_root span); no block map exists.
    let target = b"lth/bin/target-of-this-link";
    let mut inline = [0u8; 48];
    inline[..target.len()].copy_from_slice(target);
    let ino = Inode {
        mode: S_IFLNK | 0o777,
        flags: FLAG_USED | FLAG_FAST_SYMLINK,
        nlink: 1,
        size: target.len() as u64,
        inline,
        ..Inode::FREE
    };
    let b = ino.encode();
    assert_eq!(&b[80..80 + target.len()], target);
    assert!(b[80 + target.len()..128].iter().all(|&x| x == 0));

    let back = Inode::decode(&b);
    assert!(back.is_symlink() && back.is_fast_symlink());
    // The 80..128 span is target data, never a BlockPtr: decode must not
    // fabricate rdev or a dereferenceable bmap_root from it.
    assert_eq!(back.rdev, 0);
    assert!(back.bmap_root.is_null());
    assert_eq!(&back.inline[..target.len()], target);
    assert_eq!(back, ino);
}

#[test]
fn dirent_chain_dir1() {
    // Doc 07 §1: 12-byte header, u64 inode, 4-byte alignment, rec_len
    // partitions the block; last record reaches the block end.
    let mut block = vec![0u8; BLOCK_SIZE];
    dirent::init_empty(&mut block);
    dirent::insert(&mut block, 1, dirent::FT_DIR, b".").unwrap();
    dirent::insert(&mut block, 1, dirent::FT_DIR, b"..").unwrap();

    // "." at 0: rec_len 16 after ".." split it.
    assert_eq!(rd_u64(&block[0..]), 1);
    assert_eq!(rd_u16(&block[8..]), 16);
    assert_eq!(block[10], 1); // name_len
    assert_eq!(block[11], dirent::FT_DIR); // file_type
    assert_eq!(&block[12..13], b".");
    // ".." at 16: rec_len stretches to block end.
    assert_eq!(rd_u64(&block[16..]), 1);
    assert_eq!(rd_u16(&block[24..]), (BLOCK_SIZE - 16) as u16);
    assert_eq!(&block[28..30], b"..");

    let entries = dirent::parse_block(&block).unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries.iter().map(|d| d.rec_len).sum::<usize>(), BLOCK_SIZE);

    // Insert splits ".."'s slack; chain still partitions exactly.
    dirent::insert(&mut block, 42, dirent::FT_REG, b"hello.txt").unwrap();
    let entries = dirent::parse_block(&block).unwrap();
    assert_eq!(entries.len(), 3);
    assert_eq!(entries.iter().map(|d| d.rec_len).sum::<usize>(), BLOCK_SIZE);
    assert!(entries.iter().all(|d| d.offset % 4 == 0 && d.rec_len % 4 == 0));
    assert_eq!(dirent::find(&block, b"hello.txt").unwrap().unwrap().1, 42);

    // Remove merges into the previous record; partition preserved.
    dirent::remove(&mut block, b"hello.txt").unwrap();
    let entries = dirent::parse_block(&block).unwrap();
    assert_eq!(entries.iter().map(|d| d.rec_len).sum::<usize>(), BLOCK_SIZE);
    assert!(dirent::find(&block, b"hello.txt").unwrap().is_none());
}

#[test]
fn mkfs_device_layout() {
    // Doc 02 §2: block 0 header, slot A valid superblock, slot B blank,
    // dynamic region from block 3.
    let mut dev = MemDev::new(64);
    mkfs(&mut dev, &IdentityTransform, &opts()).unwrap();
    assert_eq!(&dev.block(0)[0..8], b"RFS_V2\0\0");
    assert_eq!(&dev.block(1)[0..8], b"RFSSB\0\0\0");
    assert_eq!(rd_u64(&dev.block(1)[8..]), 1); // gen 1 (doc 10 §3: no pre-genesis)
    assert!(dev.block(2).iter().all(|&b| b == 0)); // slot B blank
}
