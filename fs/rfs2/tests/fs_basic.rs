//! mkfs → mount round-trip, file/dir operations, COW discipline, block-map
//! growth (docs 04, 06, 07).

mod common;

use rfs2::layout::{BLOCK_SIZE, ROOT_INO};
use rfs2::{dirent, mkfs, Error, IdentityTransform, Rfs2};

use common::{fresh_fs, opts, tnow, MemDev};

#[test]
fn mkfs_mount_round_trip() {
    let mut fs = fresh_fs(256);
    assert_eq!(fs.generation(), 1);
    assert_eq!(fs.current_slot(), 1); // slot A
    assert!(!fs.is_read_only());
    assert!(!fs.block_count_mismatch());
    assert_eq!(fs.superblock().inode_count, 1);
    assert_eq!(fs.superblock().block_count, 2); // root dir block + imap leaf

    let root = fs.stat("/").unwrap();
    assert!(root.is_dir());
    assert_eq!(root.nlink, 2);
    assert_eq!(root.size, BLOCK_SIZE as u64);

    let names: Vec<String> = fs.readdir("/").unwrap().into_iter().map(|d| d.name).collect();
    assert_eq!(names, vec![".", ".."]);
    assert_eq!(fs.lookup("/").unwrap(), ROOT_INO);
}

#[test]
fn create_write_read_persists_across_commit_and_remount() {
    let mut fs = fresh_fs(256);
    let ino = fs.create("/hello.txt").unwrap();
    fs.write_at(ino, 0, b"hello, rfs2").unwrap();
    // Read-your-writes before commit.
    let mut buf = [0u8; 32];
    assert_eq!(fs.read_at(ino, 0, &mut buf).unwrap(), 11);
    assert_eq!(&buf[..11], b"hello, rfs2");

    fs.commit().unwrap();
    assert_eq!(fs.generation(), 2);
    assert_eq!(fs.current_slot(), 2); // flipped to slot B

    let dev = fs.into_device();
    let mut fs = Rfs2::mount(dev, IdentityTransform, tnow).unwrap();
    assert_eq!(fs.generation(), 2);
    let ino = fs.lookup("/hello.txt").unwrap();
    let st = fs.stat("/hello.txt").unwrap();
    assert!(st.is_reg());
    assert_eq!(st.size, 11);
    assert_eq!(st.blocks, 1);
    let mut buf = [0u8; 32];
    assert_eq!(fs.read_at(ino, 0, &mut buf).unwrap(), 11);
    assert_eq!(&buf[..11], b"hello, rfs2");
}

#[test]
fn cow_no_in_place_mutation_of_live_blocks() {
    // COW-1: blocks reachable from the committed superblock are never
    // rewritten by staged operations (only slots 1/2 are in-place, at commit).
    let mut fs = fresh_fs(256);
    let ino = fs.create("/a").unwrap();
    fs.write_at(ino, 0, &[0xAA; 5000]).unwrap();
    fs.commit().unwrap();

    let live = fs.live_current();
    let before: Vec<(u64, Vec<u8>)> =
        live.iter().map(|&b| (b, fs.device().block(b).to_vec())).collect();

    // Stage more work on top — overwrite, new file, dir churn. No commit.
    fs.write_at(ino, 100, &[0xBB; 3000]).unwrap();
    let b = fs.create("/b").unwrap();
    fs.write_at(b, 0, &[0xCC; 100]).unwrap();

    for (blk, bytes) in &before {
        assert_eq!(fs.device().block(*blk), &bytes[..], "live block {blk} mutated in place");
    }
}

#[test]
fn sparse_holes_read_zero() {
    let mut fs = fresh_fs(512);
    let ino = fs.create("/sparse").unwrap();
    // Write one block far out; everything before is holes (null BlockPtrs).
    fs.write_at(ino, 10 * BLOCK_SIZE as u64, b"tail").unwrap();
    let st = fs.stat("/sparse").unwrap();
    assert_eq!(st.size, 10 * BLOCK_SIZE as u64 + 4);
    assert_eq!(st.blocks, 1); // only the tail block is allocated (INODE-2)

    let mut buf = vec![0xFFu8; 2 * BLOCK_SIZE];
    assert_eq!(fs.read_at(ino, 0, &mut buf).unwrap(), 2 * BLOCK_SIZE);
    assert!(buf.iter().all(|&b| b == 0));
    let mut tail = [0u8; 4];
    fs.read_at(ino, 10 * BLOCK_SIZE as u64, &mut tail).unwrap();
    assert_eq!(&tail, b"tail");
}

#[test]
fn bmap_grows_to_height_2_and_reads_back() {
    // Doc 06 §4: height 0 → 4 KiB, height 1 → 512 KiB, height 2 → 64 MiB.
    let mut fs = fresh_fs(1024);
    let ino = fs.create("/big").unwrap();
    let pattern: Vec<u8> = (0..600 * 1024u32).map(|i| (i % 251) as u8).collect();
    fs.write_at(ino, 0, &pattern).unwrap();
    fs.commit().unwrap();
    assert_eq!(fs.stat("/big").unwrap().bmap_height, 2);

    let dev = fs.into_device().crash();
    let mut fs = Rfs2::mount(dev, IdentityTransform, tnow).unwrap();
    let ino = fs.lookup("/big").unwrap();
    let mut back = vec![0u8; pattern.len()];
    assert_eq!(fs.read_at(ino, 0, &mut back).unwrap(), pattern.len());
    assert_eq!(back, pattern);
}

#[test]
fn truncate_shrink_and_grow() {
    let mut fs = fresh_fs(512);
    let ino = fs.create("/t").unwrap();
    fs.write_at(ino, 0, &[7u8; 3 * BLOCK_SIZE]).unwrap();
    fs.truncate(ino, 100).unwrap();
    let st = fs.stat("/t").unwrap();
    assert_eq!(st.size, 100);
    assert_eq!(st.blocks, 1);

    // Grow sparsely: tail reads as zeros.
    fs.truncate(ino, BLOCK_SIZE as u64 * 2).unwrap();
    let mut buf = vec![0xFFu8; 200];
    fs.read_at(ino, 50, &mut buf).unwrap();
    assert_eq!(&buf[..50], &[7u8; 50]);
    assert!(buf[50..].iter().all(|&b| b == 0));

    fs.truncate(ino, 0).unwrap();
    let st = fs.stat("/t").unwrap();
    assert_eq!((st.size, st.blocks, st.bmap_height), (0, 0, 0));
    assert!(st.bmap_root.is_null());
    fs.commit().unwrap();
}

#[test]
fn directory_tree_ops_and_errors() {
    let mut fs = fresh_fs(512);
    fs.mkdir("/dir").unwrap();
    fs.mkdir("/dir/sub").unwrap();
    let f = fs.create("/dir/sub/file").unwrap();
    fs.write_at(f, 0, b"x").unwrap();
    fs.commit().unwrap();

    assert_eq!(fs.stat("/dir").unwrap().nlink, 3); // ".", parent entry, sub's ".."
    assert_eq!(fs.stat("/").unwrap().nlink, 3);

    assert_eq!(fs.create("/dir/sub/file").unwrap_err(), Error::Exists);
    assert_eq!(fs.lookup("/nope").unwrap_err(), Error::NotFound);
    assert_eq!(fs.lookup("/dir/sub/file/deeper").unwrap_err(), Error::NotDir);
    assert_eq!(fs.rmdir("/dir/sub").unwrap_err(), Error::NotEmpty);
    assert_eq!(fs.unlink("/dir/sub").unwrap_err(), Error::IsDir);
    assert_eq!(fs.rmdir("/dir/sub/file").unwrap_err(), Error::NotDir);

    fs.unlink("/dir/sub/file").unwrap();
    assert_eq!(fs.lookup("/dir/sub/file").unwrap_err(), Error::NotFound);
    fs.rmdir("/dir/sub").unwrap();
    fs.rmdir("/dir").unwrap();
    fs.commit().unwrap();

    assert_eq!(fs.stat("/").unwrap().nlink, 2);
    assert_eq!(fs.superblock().inode_count, 1); // only root remains
    let names: Vec<String> = fs.readdir("/").unwrap().into_iter().map(|d| d.name).collect();
    assert_eq!(names, vec![".", ".."]);
}

#[test]
fn dir_op_errno_completeness() {
    // Doc 07 §3 canonical error table: the conditions not already covered by
    // directory_tree_ops_and_errors.
    let mut fs = fresh_fs(512);
    fs.create("/plainfile").unwrap();
    fs.commit().unwrap();

    // Missing parent → ENOENT; parent not a directory → ENOTDIR.
    assert_eq!(fs.create("/nodir/x").unwrap_err(), Error::NotFound);
    assert_eq!(fs.mkdir("/nodir/x").unwrap_err(), Error::NotFound);
    assert_eq!(fs.create("/plainfile/x").unwrap_err(), Error::NotDir);
    assert_eq!(fs.mkdir("/plainfile/x").unwrap_err(), Error::NotDir);

    // Malformed names → EINVAL: empty, `.`/`..`, over NAME_MAX, relative,
    // trailing slash.
    let long = format!("/{}", "n".repeat(dirent::NAME_MAX + 1));
    assert_eq!(fs.create(&long).unwrap_err(), Error::Invalid);
    assert_eq!(fs.create("/dir/.").unwrap_err(), Error::Invalid);
    assert_eq!(fs.create("/dir/..").unwrap_err(), Error::Invalid);
    assert_eq!(fs.lookup("relative").unwrap_err(), Error::Invalid);
    assert_eq!(fs.mkdir("/trailing/").unwrap_err(), Error::Invalid);

    // unlink/rmdir of missing targets → ENOENT; rmdir of root → EINVAL.
    assert_eq!(fs.unlink("/absent").unwrap_err(), Error::NotFound);
    assert_eq!(fs.rmdir("/absent").unwrap_err(), Error::NotFound);
    assert_eq!(fs.rmdir("/").unwrap_err(), Error::Invalid);

    // Every failed operation rolled back: nothing staged, no commit needed.
    assert!(!fs.has_staged_changes());
}

#[test]
fn dirent_hole_reuse_within_block() {
    let mut fs = fresh_fs(512);
    fs.create("/one").unwrap();
    fs.create("/two").unwrap();
    fs.create("/three").unwrap();
    fs.unlink("/two").unwrap();
    // Reuses two's hole; root stays one block.
    fs.create("/two2").unwrap();
    fs.commit().unwrap();
    assert_eq!(fs.stat("/").unwrap().size, BLOCK_SIZE as u64);
    let names: Vec<String> = fs.readdir("/").unwrap().into_iter().map(|d| d.name).collect();
    assert_eq!(names, vec![".", "..", "one", "two2", "three"]);
    // file_type cache agrees with inode mode (DIR-2).
    for d in fs.readdir("/").unwrap() {
        if d.name == "." || d.name == ".." {
            continue;
        }
        let ino = fs.stat(&format!("/{}", d.name)).unwrap();
        assert_eq!(d.file_type == dirent::FT_DIR, ino.is_dir());
    }
}

#[test]
fn directory_grows_past_one_block() {
    // Eager COW staging emits a fresh spine per op (no coalescing), so batch
    // commits to let superseded generations retire and their blocks recycle.
    let mut fs = fresh_fs(4096);
    for i in 0..300 {
        fs.create(&format!("/file-with-a-longer-name-{i:04}")).unwrap();
        if i % 64 == 63 {
            fs.commit().unwrap();
        }
    }
    fs.commit().unwrap();
    assert!(fs.stat("/").unwrap().size > BLOCK_SIZE as u64);
    let dev = fs.into_device().crash();
    let mut fs = Rfs2::mount(dev, IdentityTransform, tnow).unwrap();
    assert_eq!(fs.readdir("/").unwrap().len(), 302);
    fs.lookup("/file-with-a-longer-name-0299").unwrap();
}

#[test]
fn feature_gates_refuse_or_readonly() {
    // Unknown incompat bit → refuse; unknown ro_compat bit → read-only
    // (doc 10 §2).
    let mut dev = MemDev::new(64);
    mkfs(&mut dev, &IdentityTransform, &opts()).unwrap();

    let mut d = dev.clone();
    d.corrupt(0, 72); // feature_incompat low byte: sets unknown bits
    assert!(matches!(
        Rfs2::mount(d, IdentityTransform, tnow),
        Err(Error::Unsupported)
    ));

    // ENCRYPTION bit set but identity transform provides no crypto → refuse.
    let mut d = dev.clone();
    d.vol[72] = 0x01;
    d.durable[72] = 0x01;
    assert!(matches!(
        Rfs2::mount(d, IdentityTransform, tnow),
        Err(Error::Unsupported)
    ));

    let mut d = dev.clone();
    d.vol[80] = 0x02; // unknown ro_compat bit
    d.durable[80] = 0x02;
    let mut fs = Rfs2::mount(d, IdentityTransform, tnow).unwrap();
    assert!(fs.is_read_only());
    assert_eq!(fs.create("/x").unwrap_err(), Error::ReadOnly);
    assert_eq!(fs.readdir("/").unwrap().len(), 2); // reads still fine
}
