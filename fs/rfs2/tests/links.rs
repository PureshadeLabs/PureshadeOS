//! Hard links (doc 07 §3 link, doc 06 §5 lifecycle: nlink, orphan pins) and
//! symlinks (doc 06 §1: fast/slow storage, readlink, traversal policy).

mod common;

use rfs2::layout::{BLOCK_SIZE, INLINE_SYMLINK_MAX};
use rfs2::{Error, IdentityTransform, Rfs2};

use common::{fresh_fs, tnow};

// ── Hard links ───────────────────────────────────────────────────────────────

#[test]
fn hardlink_shares_the_inode() {
    let mut fs = fresh_fs(256);
    let ino = fs.create("/f").unwrap();
    fs.write_at(ino, 0, b"shared").unwrap();
    fs.link("/f", "/g").unwrap();
    fs.commit().unwrap();

    assert_eq!(fs.lookup("/g").unwrap(), ino);
    assert_eq!(fs.stat("/f").unwrap().nlink, 2);
    // One inode, two names: writes through either name are the same file.
    fs.write_at(ino, 0, b"SHARED").unwrap();
    fs.commit().unwrap();
    let mut buf = [0u8; 6];
    let g = fs.lookup("/g").unwrap();
    fs.read_at(g, 0, &mut buf).unwrap();
    assert_eq!(&buf, b"SHARED");
    // Still one inode on the books.
    assert_eq!(fs.superblock().inode_count, 2); // root + f
}

#[test]
fn unlink_decrements_frees_only_at_zero() {
    let mut fs = fresh_fs(256);
    let ino = fs.create("/f").unwrap();
    fs.write_at(ino, 0, &[7u8; 2 * BLOCK_SIZE]).unwrap();
    fs.link("/f", "/g").unwrap();
    fs.commit().unwrap();

    fs.unlink("/f").unwrap();
    fs.commit().unwrap();
    // nlink 2 → 1: inode and data survive under the other name.
    assert_eq!(fs.stat("/g").unwrap().nlink, 1);
    let mut buf = [0u8; 16];
    assert_eq!(fs.read_at(ino, 0, &mut buf).unwrap(), 16);
    assert_eq!(fs.superblock().inode_count, 2);

    fs.unlink("/g").unwrap();
    fs.commit().unwrap();
    // nlink 0, no pins: inode + blocks freed (doc 06 §5).
    assert_eq!(fs.superblock().inode_count, 1);
    assert_eq!(fs.read_at(ino, 0, &mut buf).unwrap_err(), Error::NotFound);

    // Two more commits retire the fallback generations; the file's data
    // blocks must actually return to the free pool (doc 05 §5).
    fs.create("/churn1").unwrap();
    fs.commit().unwrap();
    fs.create("/churn2").unwrap();
    fs.commit().unwrap();
    let live = fs.live_current().len() + fs.live_previous().len();
    // Sanity bound: nothing close to the freed file's blocks lingers.
    assert!(live < 20, "freed blocks leaked: {live} still live");
}

#[test]
fn link_errno_paths() {
    let mut fs = fresh_fs(256);
    fs.create("/f").unwrap();
    fs.mkdir("/d").unwrap();
    // Directory hard links are refused (doc 07 §3: EPERM).
    assert_eq!(fs.link("/d", "/d2").unwrap_err(), Error::Perm);
    // Existing destination name.
    assert_eq!(fs.link("/f", "/d").unwrap_err(), Error::Exists);
    // Missing source; missing destination parent.
    assert_eq!(fs.link("/nope", "/x").unwrap_err(), Error::NotFound);
    assert_eq!(fs.link("/f", "/nodir/x").unwrap_err(), Error::NotFound);
}

#[test]
fn symlink_can_be_hardlinked() {
    // Doc 07 §3: the link counts the symlink inode itself, no following.
    let mut fs = fresh_fs(256);
    let ino = fs.symlink("target", "/s").unwrap();
    fs.link("/s", "/s2").unwrap();
    fs.commit().unwrap();
    assert_eq!(fs.lookup("/s2").unwrap(), ino);
    assert_eq!(fs.stat("/s").unwrap().nlink, 2);
    assert_eq!(fs.readlink("/s2").unwrap(), "target");
}

#[test]
fn unlink_while_pinned_orphans_then_frees_on_unpin() {
    // Doc 06 §5 deleted-but-pinned (resolved): nlink 0 + open handle = orphan,
    // readable by inode number; last unpin frees it.
    let mut fs = fresh_fs(256);
    let ino = fs.create("/f").unwrap();
    fs.write_at(ino, 0, b"pinned data").unwrap();
    fs.commit().unwrap();

    fs.pin(ino).unwrap();
    fs.unlink("/f").unwrap();
    fs.commit().unwrap();

    // Unreachable from any directory, still readable through the handle.
    assert_eq!(fs.lookup("/f").unwrap_err(), Error::NotFound);
    let mut buf = [0u8; 11];
    assert_eq!(fs.read_at(ino, 0, &mut buf).unwrap(), 11);
    assert_eq!(&buf, b"pinned data");

    fs.unpin(ino).unwrap();
    fs.commit().unwrap();
    assert_eq!(fs.read_at(ino, 0, &mut buf).unwrap_err(), Error::NotFound);
    assert_eq!(fs.superblock().inode_count, 1);
}

#[test]
fn orphan_left_by_crash_is_reclaimed_at_next_rw_mount() {
    // Session ends with the pin held → orphan persists on disk; the next
    // read-write mount reclaims it in one immediately-committed transaction
    // (doc 06 §5, doc 09 §4).
    let mut fs = fresh_fs(256);
    let ino = fs.create("/f").unwrap();
    fs.write_at(ino, 0, b"doomed").unwrap();
    fs.commit().unwrap();
    fs.pin(ino).unwrap();
    fs.unlink("/f").unwrap();
    fs.commit().unwrap();
    let gen = fs.generation();
    assert_eq!(fs.superblock().inode_count, 2); // orphan still on the books

    let dev = fs.into_device().crash(); // pins die with the session
    let mut fs = Rfs2::mount(dev, IdentityTransform, tnow).unwrap();
    assert_eq!(fs.generation(), gen + 1); // exactly one reclaim commit
    assert_eq!(fs.superblock().inode_count, 1);
    let mut buf = [0u8; 6];
    assert_eq!(fs.read_at(ino, 0, &mut buf).unwrap_err(), Error::NotFound);

    // No orphans → the next mount burns no generation.
    let dev = fs.into_device().crash();
    let fs = Rfs2::mount(dev, IdentityTransform, tnow).unwrap();
    assert_eq!(fs.generation(), gen + 1);
}

// ── Symlinks ─────────────────────────────────────────────────────────────────

#[test]
fn fast_symlink_inline_no_data_block() {
    let mut fs = fresh_fs(256);
    let target = "short/inline/target"; // ≤ 48 bytes
    assert!(target.len() <= INLINE_SYMLINK_MAX);
    fs.symlink(target, "/s").unwrap();
    fs.commit().unwrap();

    let st = fs.stat("/s").unwrap();
    assert!(st.is_symlink() && st.is_fast_symlink());
    assert_eq!(st.size, target.len() as u64);
    assert_eq!(st.blocks, 0); // no block map at all (doc 06 §1)
    assert!(st.bmap_root.is_null());
    assert_eq!(fs.readlink("/s").unwrap(), target);

    let dev = fs.into_device().crash();
    let mut fs = Rfs2::mount(dev, IdentityTransform, tnow).unwrap();
    assert_eq!(fs.readlink("/s").unwrap(), target);
}

#[test]
fn symlink_capacity_boundary_48_in_49_out() {
    let mut fs = fresh_fs(256);
    let t48 = "x".repeat(INLINE_SYMLINK_MAX);
    let t49 = "y".repeat(INLINE_SYMLINK_MAX + 1);
    fs.symlink(&t48, "/in").unwrap();
    fs.symlink(&t49, "/out").unwrap();
    fs.commit().unwrap();

    let st = fs.stat("/in").unwrap();
    assert!(st.is_fast_symlink());
    assert_eq!(st.blocks, 0);

    let st = fs.stat("/out").unwrap();
    assert!(st.is_symlink() && !st.is_fast_symlink());
    assert_eq!(st.blocks, 1); // spilled to a data block

    assert_eq!(fs.readlink("/in").unwrap(), t48);
    assert_eq!(fs.readlink("/out").unwrap(), t49);
}

#[test]
fn slow_symlink_multiblock_round_trips() {
    let mut fs = fresh_fs(256);
    let long = "very/long/component/".repeat(300); // 6000 B > 1 block
    fs.symlink(&long, "/s").unwrap();
    fs.commit().unwrap();

    let st = fs.stat("/s").unwrap();
    assert!(!st.is_fast_symlink());
    assert_eq!(st.size, long.len() as u64);
    assert_eq!(st.blocks, 2);

    let dev = fs.into_device().crash();
    let mut fs = Rfs2::mount(dev, IdentityTransform, tnow).unwrap();
    assert_eq!(fs.readlink("/s").unwrap(), long);
}

#[test]
fn symlink_traversal_policy_and_errno() {
    // Doc 07 §3 (resolved): the filesystem never follows symlinks. As a final
    // component the symlink inode itself resolves; as an intermediate
    // component the walk fails with ENOTDIR. Following is the VFS's job.
    let mut fs = fresh_fs(256);
    fs.mkdir("/real").unwrap();
    fs.create("/real/file").unwrap();
    let s = fs.symlink("/real", "/alias").unwrap();

    assert_eq!(fs.lookup("/alias").unwrap(), s); // resolves to the symlink itself
    assert_eq!(fs.lookup("/alias/file").unwrap_err(), Error::NotDir);

    // readlink on a non-symlink; empty target; duplicate name.
    assert_eq!(fs.readlink("/real").unwrap_err(), Error::Invalid);
    assert_eq!(fs.symlink("", "/e").unwrap_err(), Error::Invalid);
    assert_eq!(fs.symlink("t", "/alias").unwrap_err(), Error::Exists);
    // Data I/O on a symlink is refused (readlink is the only accessor).
    assert_eq!(fs.read_at(s, 0, &mut [0u8; 4]).unwrap_err(), Error::Invalid);
    assert_eq!(fs.write_at(s, 0, b"z").unwrap_err(), Error::Invalid);

    // unlink applies to symlinks like any non-directory.
    fs.unlink("/alias").unwrap();
    assert_eq!(fs.lookup("/alias").unwrap_err(), Error::NotFound);
    fs.commit().unwrap();
}
