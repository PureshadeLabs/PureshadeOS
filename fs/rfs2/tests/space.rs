//! Space management (doc 05): mark-and-sweep reconstructs exactly the live
//! set; no live block is ever freed, no free block is leaked; the two-live-
//! trees rule holds; allocation fails cleanly with NoSpace.

mod common;

use std::collections::BTreeSet;

use rfs2::layout::{BLOCK_SIZE, FIRST_DATA_BLOCK};
use rfs2::{Error, IdentityTransform, Rfs2};

use common::{fresh_fs, tnow};

#[test]
fn known_tree_mark_sweep_matches_expected() {
    // Fresh volume: exactly {3 (root dir block), 4 (inode-map leaf)} live.
    let fs = fresh_fs(64);
    assert_eq!(fs.live_current(), vec![3, 4]);
    assert!(fs.live_previous().is_empty()); // slot B blank after mkfs

    // One file, one data block: live set is exactly {dir block, imap leaf,
    // data block} — three blocks, all distinct from the gen-1 set except by
    // sharing nothing (every spine block was rewritten).
    let mut fs = fresh_fs(64);
    let ino = fs.create("/f").unwrap();
    fs.write_at(ino, 0, &[1u8; 10]).unwrap();
    fs.commit().unwrap();
    let live: BTreeSet<u64> = fs.live_current().into_iter().collect();
    assert_eq!(live.len(), 3, "root dir block + imap leaf + data block");
    // Previous tree (gen 1) is retained until the next commit (doc 04 §6).
    let prev: BTreeSet<u64> = fs.live_previous().into_iter().collect();
    assert_eq!(prev, BTreeSet::from([3, 4]));
    // COW: gen 2 shares nothing with gen 1 here (all three paths rewritten).
    assert!(live.is_disjoint(&prev));
    assert_eq!(fs.superblock().block_count, 3);
}

#[test]
fn incremental_free_tracking_equals_remount_mark_sweep() {
    // SPACE-4: after an arbitrary workload, the incrementally-maintained live
    // sets must equal what mark-and-sweep reconstructs from disk. Both
    // directions: nothing live was freed, nothing free was kept live.
    let mut fs = fresh_fs(1024);
    let a = fs.create("/a").unwrap();
    fs.write_at(a, 0, &vec![3u8; 5 * BLOCK_SIZE]).unwrap();
    fs.mkdir("/d").unwrap();
    fs.commit().unwrap();

    let b = fs.create("/d/b").unwrap();
    fs.write_at(b, 0, &vec![4u8; 200 * 1024]).unwrap(); // height-2 bmap
    fs.write_at(a, BLOCK_SIZE as u64, &[5u8; 100]).unwrap(); // partial overwrite
    fs.commit().unwrap();

    fs.unlink("/a").unwrap();
    fs.truncate(b, 3 * BLOCK_SIZE as u64).unwrap();
    fs.commit().unwrap();

    let live_cur = fs.live_current();
    let live_prev = fs.live_previous();
    let gen = fs.generation();

    let dev = fs.into_device().crash();
    let fs2 = Rfs2::mount(dev, IdentityTransform, tnow).unwrap();
    assert_eq!(fs2.generation(), gen);
    assert_eq!(fs2.live_current(), live_cur, "no live block freed / none leaked (current)");
    assert_eq!(fs2.live_previous(), live_prev, "no live block freed / none leaked (previous)");
    assert!(!fs2.block_count_mismatch());
}

#[test]
fn superseded_blocks_free_only_after_fallback_gen_retires() {
    // COW-4 / SPACE-5: blocks superseded in commit G stay allocated while
    // gen K still references them; they free when commit G+1 overwrites K.
    let mut fs = fresh_fs(64);
    let ino = fs.create("/f").unwrap();
    fs.write_at(ino, 0, &[1u8; 100]).unwrap();
    fs.commit().unwrap(); // gen 2
    let gen2_live: BTreeSet<u64> = fs.live_current().into_iter().collect();

    fs.write_at(ino, 0, &[2u8; 100]).unwrap();
    fs.commit().unwrap(); // gen 3; gen 1 (slot A's old tree) abandoned
    let gen3_live: BTreeSet<u64> = fs.live_current().into_iter().collect();
    let prev: BTreeSet<u64> = fs.live_previous().into_iter().collect();
    assert_eq!(prev, gen2_live, "gen 2 tree retained as fallback");
    // The rewritten path (data block + inode-map leaf) is fresh; the
    // untouched root-dir block is shared by reference between the trees
    // (doc 04 §1 corollary).
    let superseded: BTreeSet<u64> = gen2_live.difference(&gen3_live).copied().collect();
    assert_eq!(superseded.len(), 2, "data block + imap leaf superseded");

    fs.write_at(ino, 0, &[3u8; 100]).unwrap();
    fs.commit().unwrap(); // gen 4; gen 2's uniquely-owned blocks now free
    let allocated: BTreeSet<u64> =
        fs.live_current().into_iter().chain(fs.live_previous()).collect();
    for b in &superseded {
        assert!(!allocated.contains(b), "gen-2-only block {b} should be free after two commits");
    }
}

#[test]
fn allocator_never_hands_out_live_blocks_and_enospc_is_clean() {
    // Tiny device; write until it fills. Every allocation is checked against
    // SPACE-1 internally (debug_assert); ENOSPC must surface as Error::NoSpace
    // with the staged transaction rolled back and gen K still mountable.
    let mut fs = fresh_fs(24); // 3 fixed + 21 dynamic blocks
    let ino = fs.create("/f").unwrap();
    fs.commit().unwrap();

    let mut wrote = 0u64;
    let res = loop {
        match fs.write_at(ino, wrote * BLOCK_SIZE as u64, &[9u8; BLOCK_SIZE]) {
            Ok(()) => wrote += 1,
            Err(e) => break e,
        }
    };
    assert_eq!(res, Error::NoSpace);
    assert!(wrote > 0);

    // The filesystem survives: previously committed state intact on remount.
    let dev = fs.into_device().crash();
    let mut fs = Rfs2::mount(dev, IdentityTransform, tnow).unwrap();
    fs.lookup("/f").unwrap();
}

#[test]
fn fixed_blocks_never_allocated() {
    // Blocks 0–2 are outside the dynamic region (doc 02 §2).
    let mut fs = fresh_fs(128);
    for i in 0..20 {
        let ino = fs.create(&format!("/f{i}")).unwrap();
        fs.write_at(ino, 0, &[i as u8; 100]).unwrap();
        fs.commit().unwrap();
    }
    for b in fs.live_current().into_iter().chain(fs.live_previous()) {
        assert!(b >= FIRST_DATA_BLOCK);
    }
}
