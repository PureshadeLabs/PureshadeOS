//! Inode-map radix growth (doc 06 §3): height 0 (32 inodes) → height 1
//! (4096) → height 2 (524288), with no fixed table anywhere.

mod common;

use rfs2::{IdentityTransform, Rfs2};

use common::{fresh_fs, tnow};

#[test]
fn imap_grows_past_leaf_capacity_to_height_1() {
    let mut fs = fresh_fs(2048);
    assert_eq!(fs.inode_map_height(), 0);
    // General allocation starts at ino 10; inos 10..40 cross the 32-inode
    // leaf boundary (capacity of height 0).
    for i in 0..30 {
        fs.create(&format!("/f{i:03}")).unwrap();
    }
    fs.commit().unwrap();
    assert_eq!(fs.inode_map_height(), 1);
    assert_eq!(fs.superblock().inode_count, 31);

    let dev = fs.into_device().crash();
    let mut fs = Rfs2::mount(dev, IdentityTransform, tnow).unwrap();
    assert_eq!(fs.inode_map_height(), 1);
    for i in 0..30 {
        fs.lookup(&format!("/f{i:03}")).unwrap();
    }
}

#[test]
fn imap_grows_past_initial_fanout_to_height_2() {
    // Height 1 addresses 32 × 128 = 4096 inodes; pushing past that forces a
    // second index level. ~4100 creates, committed in batches so superseded
    // spine churn is reclaimed as generations retire.
    let mut fs = fresh_fs(16 * 1024); // 64 MiB image
    let n = 4120u64;
    for i in 0..n {
        fs.mkdir(&format!("/d{i:05}")).unwrap();
        if i % 128 == 127 {
            fs.commit().unwrap();
        }
    }
    fs.commit().unwrap();
    assert_eq!(fs.inode_map_height(), 2);
    assert_eq!(fs.superblock().inode_count, 1 + n);
    assert_eq!(fs.superblock().next_inode, 10 + n);

    // Every inode reachable through the two-level radix; spot-check both
    // ends and a middle band, then verify a full remount walks the tree.
    let dev = fs.into_device().crash();
    let mut fs = Rfs2::mount(dev, IdentityTransform, tnow).unwrap();
    assert_eq!(fs.inode_map_height(), 2);
    for i in [0u64, 31, 32, 4095, 4096, n - 1] {
        let st = fs.stat(&format!("/d{i:05}")).unwrap();
        assert!(st.is_dir());
    }
    assert!(!fs.block_count_mismatch());
}
