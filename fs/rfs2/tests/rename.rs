//! rename (doc 07 §3, resolved): same-dir, cross-dir, target replacement,
//! no-ops, loop check, `..` retarget, nlink fixups, errno paths, and the
//! one-commit atomicity guarantee.

mod common;

use rfs2::layout::RO_COMPAT_DIR_HASH_INDEX;
use rfs2::{Error, IdentityTransform, Rfs2};

use common::{fresh_fs, tnow, MemDev};

#[test]
fn same_dir_rename_moves_the_name_only() {
    let mut fs = fresh_fs(256);
    let ino = fs.create("/a").unwrap();
    fs.write_at(ino, 0, b"payload").unwrap();
    fs.commit().unwrap();

    fs.rename("/a", "/b").unwrap();
    fs.commit().unwrap();

    assert_eq!(fs.lookup("/b").unwrap(), ino);
    assert_eq!(fs.lookup("/a").unwrap_err(), Error::NotFound);
    let mut buf = [0u8; 8];
    assert_eq!(fs.read_at(ino, 0, &mut buf).unwrap(), 7);
    assert_eq!(&buf[..7], b"payload");
    // A rename is pure dirent motion: same inode, nlink unchanged.
    assert_eq!(fs.stat("/b").unwrap().nlink, 1);
}

#[test]
fn cross_dir_rename_file() {
    let mut fs = fresh_fs(256);
    fs.mkdir("/src").unwrap();
    fs.mkdir("/dst").unwrap();
    let ino = fs.create("/src/f").unwrap();
    fs.write_at(ino, 0, b"x").unwrap();
    fs.commit().unwrap();

    fs.rename("/src/f", "/dst/g").unwrap();
    fs.commit().unwrap();

    assert_eq!(fs.lookup("/dst/g").unwrap(), ino);
    assert_eq!(fs.lookup("/src/f").unwrap_err(), Error::NotFound);
    // File moves don't touch directory nlink counts.
    assert_eq!(fs.stat("/src").unwrap().nlink, 2);
    assert_eq!(fs.stat("/dst").unwrap().nlink, 2);
}

#[test]
fn rename_replaces_existing_file_and_frees_it() {
    let mut fs = fresh_fs(256);
    let a = fs.create("/a").unwrap();
    fs.write_at(a, 0, b"AAAA").unwrap();
    let b = fs.create("/b").unwrap();
    fs.write_at(b, 0, b"BBBB").unwrap();
    fs.commit().unwrap();
    assert_eq!(fs.superblock().inode_count, 3);

    fs.rename("/a", "/b").unwrap();
    fs.commit().unwrap();

    assert_eq!(fs.lookup("/b").unwrap(), a);
    assert_eq!(fs.lookup("/a").unwrap_err(), Error::NotFound);
    let mut buf = [0u8; 4];
    fs.read_at(a, 0, &mut buf).unwrap();
    assert_eq!(&buf, b"AAAA");
    // The replaced target hit nlink 0 and was freed in the same transaction.
    assert_eq!(fs.superblock().inode_count, 2);
    assert_eq!(fs.read_at(b, 0, &mut buf).unwrap_err(), Error::NotFound);
}

#[test]
fn rename_dir_retargets_dotdot_and_fixes_nlinks() {
    let mut fs = fresh_fs(512);
    fs.mkdir("/d1").unwrap();
    fs.mkdir("/d2").unwrap();
    fs.mkdir("/d1/sub").unwrap();
    fs.commit().unwrap();
    assert_eq!(fs.stat("/d1").unwrap().nlink, 3); // ".", parent entry, sub's ".."
    assert_eq!(fs.stat("/d2").unwrap().nlink, 2);

    let d2 = fs.lookup("/d2").unwrap();
    fs.rename("/d1/sub", "/d2/sub2").unwrap();
    fs.commit().unwrap();

    // `..` of the moved directory now resolves to the new parent (doc 07 §3
    // rename step 5).
    assert_eq!(fs.lookup("/d2/sub2/..").unwrap(), d2);
    assert_eq!(fs.stat("/d1").unwrap().nlink, 2);
    assert_eq!(fs.stat("/d2").unwrap().nlink, 3);

    // Survives remount: the retarget went through the COW spine.
    let dev = fs.into_device().crash();
    let mut fs = Rfs2::mount(dev, IdentityTransform, tnow).unwrap();
    assert_eq!(fs.lookup("/d2/sub2/..").unwrap(), d2);
}

#[test]
fn rename_dir_onto_empty_dir_replaces_it() {
    let mut fs = fresh_fs(512);
    fs.mkdir("/src").unwrap();
    fs.mkdir("/dst").unwrap();
    fs.mkdir("/src/mv").unwrap();
    fs.mkdir("/dst/mv").unwrap(); // empty target, will be replaced
    fs.commit().unwrap();
    let moved = fs.lookup("/src/mv").unwrap();
    assert_eq!(fs.superblock().inode_count, 5);

    fs.rename("/src/mv", "/dst/mv").unwrap();
    fs.commit().unwrap();

    assert_eq!(fs.lookup("/dst/mv").unwrap(), moved);
    assert_eq!(fs.lookup("/src/mv").unwrap_err(), Error::NotFound);
    assert_eq!(fs.superblock().inode_count, 4); // replaced empty dir freed
    // dst lost the replaced dir's `..` but gained the moved dir's: net 2 + 1.
    assert_eq!(fs.stat("/dst").unwrap().nlink, 3);
    assert_eq!(fs.stat("/src").unwrap().nlink, 2);
}

#[test]
fn rename_onto_nonempty_dir_is_refused() {
    let mut fs = fresh_fs(512);
    fs.mkdir("/a").unwrap();
    fs.mkdir("/b").unwrap();
    fs.create("/b/occupant").unwrap();
    fs.commit().unwrap();
    assert_eq!(fs.rename("/a", "/b").unwrap_err(), Error::NotEmpty);
    // Failed rename left nothing staged behind.
    fs.commit().unwrap();
    fs.lookup("/a").unwrap();
    fs.lookup("/b/occupant").unwrap();
}

#[test]
fn rename_type_mismatch_errors() {
    let mut fs = fresh_fs(256);
    fs.create("/file").unwrap();
    fs.mkdir("/dir").unwrap();
    // source file, target dir → EISDIR; source dir, target file → ENOTDIR.
    assert_eq!(fs.rename("/file", "/dir").unwrap_err(), Error::IsDir);
    assert_eq!(fs.rename("/dir", "/file").unwrap_err(), Error::NotDir);
}

#[test]
fn rename_noops() {
    let mut fs = fresh_fs(256);
    let ino = fs.create("/a").unwrap();
    fs.link("/a", "/a2").unwrap();
    fs.commit().unwrap();
    let gen = fs.generation();

    // Same directory entry → no-op.
    fs.rename("/a", "/a").unwrap();
    // Destination is a hard link to the same inode → no-op; both names stay
    // (POSIX rename semantics, doc 07 §3 step 2).
    fs.rename("/a", "/a2").unwrap();
    assert!(!fs.has_staged_changes());
    fs.commit().unwrap();
    assert_eq!(fs.generation(), gen); // nothing staged, no generation burned
    assert_eq!(fs.lookup("/a").unwrap(), ino);
    assert_eq!(fs.lookup("/a2").unwrap(), ino);
    assert_eq!(fs.stat("/a").unwrap().nlink, 2);
}

#[test]
fn rename_loop_and_root_and_missing_errors() {
    let mut fs = fresh_fs(512);
    fs.mkdir("/a").unwrap();
    fs.mkdir("/a/b").unwrap();
    fs.mkdir("/a/b/c").unwrap();
    fs.commit().unwrap();

    // A directory cannot move into itself or a descendant (doc 07 §3 step 3).
    assert_eq!(fs.rename("/a", "/a/b/moved").unwrap_err(), Error::Invalid);
    assert_eq!(fs.rename("/a", "/a/b/c/moved").unwrap_err(), Error::Invalid);
    // Root cannot be renamed; `.`/`..` are invalid names.
    assert_eq!(fs.rename("/", "/x").unwrap_err(), Error::Invalid);
    assert_eq!(fs.rename("/a/.", "/x").unwrap_err(), Error::Invalid);
    // Missing source / missing destination parent.
    assert_eq!(fs.rename("/nope", "/x").unwrap_err(), Error::NotFound);
    assert_eq!(fs.rename("/a", "/nodir/x").unwrap_err(), Error::NotFound);
    // Moving *down* is fine (the loop check must not overfire).
    fs.rename("/a/b/c", "/c").unwrap();
    fs.lookup("/c").unwrap();
}

#[test]
fn rename_is_one_commit_and_atomic_across_crash() {
    let mut fs = fresh_fs(256);
    let ino = fs.create("/old").unwrap();
    fs.write_at(ino, 0, b"data").unwrap();
    fs.commit().unwrap();
    let gen = fs.generation();

    // Staged rename: invisible on disk until the single superblock flip.
    fs.rename("/old", "/new").unwrap();
    assert_eq!(fs.generation(), gen);
    let dev_before = fs.device().crash();
    let mut pre = Rfs2::mount(dev_before, IdentityTransform, tnow).unwrap();
    pre.lookup("/old").unwrap(); // crash before commit → old name intact
    assert_eq!(pre.lookup("/new").unwrap_err(), Error::NotFound);

    fs.commit().unwrap();
    assert_eq!(fs.generation(), gen + 1); // exactly one commit per rename

    let dev = fs.into_device().crash();
    let mut fs = Rfs2::mount(dev, IdentityTransform, tnow).unwrap();
    fs.lookup("/new").unwrap();
    assert_eq!(fs.lookup("/old").unwrap_err(), Error::NotFound);
}

#[test]
fn large_directory_linear_lookup_across_blocks() {
    // Doc 07 §4: the baseline is an unindexed linear list; lookup must be
    // correct (if O(n)) across many dirent blocks, including rename hits on
    // first/middle/last entries.
    let mut fs = fresh_fs(4096);
    for i in 0..300 {
        fs.create(&format!("/entry-with-a-longish-name-{i:04}")).unwrap();
        if i % 64 == 63 {
            fs.commit().unwrap();
        }
    }
    fs.commit().unwrap();

    fs.rename("/entry-with-a-longish-name-0000", "/renamed-first").unwrap();
    fs.rename("/entry-with-a-longish-name-0150", "/renamed-mid").unwrap();
    fs.rename("/entry-with-a-longish-name-0299", "/renamed-last").unwrap();
    fs.commit().unwrap();

    let dev = fs.into_device().crash();
    let mut fs = Rfs2::mount(dev, IdentityTransform, tnow).unwrap();
    fs.lookup("/renamed-first").unwrap();
    fs.lookup("/renamed-mid").unwrap();
    fs.lookup("/renamed-last").unwrap();
    assert_eq!(fs.lookup("/entry-with-a-longish-name-0150").unwrap_err(), Error::NotFound);
    assert_eq!(fs.readdir("/").unwrap().len(), 302);
}

#[test]
fn dir_hash_index_ro_compat_bit_forces_readonly() {
    // Doc 07 §4 / 10 §2 (resolved): the future hashed index is ro_compat
    // bit 1 — this baseline reader can scan linearly but must not write.
    use rfs2::{mkfs, BlockDevice};
    let mut dev = MemDev::new(64);
    mkfs(&mut dev, &IdentityTransform, &common::opts()).unwrap();
    let mut hdr = vec![0u8; 4096];
    dev.read_block(0, &mut hdr).unwrap();
    hdr[80] |= RO_COMPAT_DIR_HASH_INDEX as u8; // feature_ro_compat low byte
    dev.write_block(0, &hdr).unwrap();
    dev.flush().unwrap();

    let mut fs = Rfs2::mount(dev, IdentityTransform, tnow).unwrap();
    assert!(fs.is_read_only());
    assert_eq!(fs.readdir("/").unwrap().len(), 2); // reads still work
    assert_eq!(fs.rename("/a", "/b").unwrap_err(), Error::ReadOnly);
    assert_eq!(fs.create("/x").unwrap_err(), Error::ReadOnly);
}
