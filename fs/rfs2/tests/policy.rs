//! Resolved policy items: noatime (doc 06 §5 — reads are commit-free), fsync
//! = full commit (doc 04 §3), and the generation freeze at GEN_LIMIT
//! (doc 03 §3).

mod common;

use rfs2::layout::{GEN_LIMIT, SB_SLOT_A, SB_SLOT_B, BLOCK_SIZE};
use rfs2::{mkfs, superblock, BlockDevice, Error, IdentityTransform, Rfs2, StaticHeader};

use common::{fresh_fs, opts, tnow, MemDev};

// ── atime: reads never touch the disk ────────────────────────────────────────

#[test]
fn reads_are_commit_free_and_write_nothing() {
    let mut fs = fresh_fs(256);
    let ino = fs.create("/f").unwrap();
    fs.write_at(ino, 0, b"data").unwrap();
    fs.mkdir("/d").unwrap();
    fs.commit().unwrap();
    let gen = fs.generation();
    let atime = fs.stat("/f").unwrap().atime;

    // Byte-identical device before/after every read-path operation: no
    // atime bump, no staged spine rewrite, no commit (doc 06 §5 noatime).
    let before = fs.device().vol.clone();
    let mut buf = [0u8; 4];
    fs.read_at(ino, 0, &mut buf).unwrap();
    fs.readdir("/").unwrap();
    fs.readdir("/d").unwrap();
    fs.lookup("/f").unwrap();
    fs.stat("/d").unwrap();
    assert!(!fs.has_staged_changes());
    assert_eq!(fs.device().vol, before, "a read path wrote to the device");
    assert_eq!(fs.generation(), gen);
    // atime is set at creation and never advanced by reads.
    assert_eq!(fs.stat("/f").unwrap().atime, atime);

    // commit() after pure reads is a no-op: no generation burned.
    fs.commit().unwrap();
    assert_eq!(fs.generation(), gen);
}

// ── fsync: the commit is the durability unit ─────────────────────────────────

#[test]
fn fsync_commits_the_whole_dirty_set() {
    let mut fs = fresh_fs(256);
    let a = fs.create("/a").unwrap();
    let b = fs.create("/b").unwrap();
    fs.commit().unwrap();
    let gen = fs.generation();

    fs.write_at(a, 0, b"AAAA").unwrap();
    fs.write_at(b, 0, b"BBBB").unwrap();
    assert!(fs.has_staged_changes());
    fs.fsync(a).unwrap();
    assert!(!fs.has_staged_changes());
    assert_eq!(fs.generation(), gen + 1); // one generation for the whole set

    // Guarantee (doc 04 §3): after fsync(a) returns, a crash loses nothing
    // staged before the call — including the unrelated write to /b.
    let dev = fs.into_device().crash();
    let mut fs = Rfs2::mount(dev, IdentityTransform, tnow).unwrap();
    let mut buf = [0u8; 4];
    fs.read_at(a, 0, &mut buf).unwrap();
    assert_eq!(&buf, b"AAAA");
    fs.read_at(b, 0, &mut buf).unwrap();
    assert_eq!(&buf, b"BBBB");
}

#[test]
fn fsync_clean_is_free_and_unknown_ino_errors() {
    let mut fs = fresh_fs(256);
    let ino = fs.create("/f").unwrap();
    fs.commit().unwrap();
    let gen = fs.generation();
    fs.fsync(ino).unwrap(); // nothing staged → no superblock write
    assert_eq!(fs.generation(), gen);
    assert_eq!(fs.fsync(9999).unwrap_err(), Error::NotFound);
}

// ── Generation freeze (gen wrap) ─────────────────────────────────────────────

/// Remount `dev` with its committed generation forged to `gen` (same tree,
/// same fields otherwise) — the only way to approach GEN_LIMIT in a test.
fn forge_generation(mut dev: MemDev, gen: u64) -> MemDev {
    let mut hbuf = vec![0u8; BLOCK_SIZE];
    dev.read_block(0, &mut hbuf).unwrap();
    let header = StaticHeader::decode(&hbuf).unwrap();
    let cur = superblock::read_slot(&mut dev, &IdentityTransform, SB_SLOT_A, &header)
        .or_else(|| superblock::read_slot(&mut dev, &IdentityTransform, SB_SLOT_B, &header))
        .unwrap();
    let forged = superblock::Superblock { gen, ..cur };
    // Higher gen wins slot selection regardless of which slot it sits in.
    superblock::write_slot(&mut dev, &IdentityTransform, SB_SLOT_B, &forged).unwrap();
    dev.flush().unwrap();
    dev
}

#[test]
fn generation_freeze_at_gen_limit() {
    // Doc 03 §3 (resolved): at GEN_LIMIT the volume freezes read-only —
    // mutation and commit refused, reads fine, gen never wraps or resets.
    let mut dev = MemDev::new(64);
    mkfs(&mut dev, &IdentityTransform, &opts()).unwrap();
    let dev = forge_generation(dev, GEN_LIMIT);

    let mut fs = Rfs2::mount(dev, IdentityTransform, tnow).unwrap();
    assert_eq!(fs.generation(), GEN_LIMIT);
    assert_eq!(fs.create("/x").unwrap_err(), Error::GenExhausted);
    assert_eq!(fs.mkdir("/d").unwrap_err(), Error::GenExhausted);
    assert_eq!(fs.rename("/a", "/b").unwrap_err(), Error::GenExhausted);
    fs.readdir("/").unwrap(); // reads unaffected
    fs.commit().unwrap(); // clean commit is a no-op, not an error
    assert_eq!(fs.generation(), GEN_LIMIT);
}

#[test]
fn last_usable_generation_commits_then_freezes() {
    let mut dev = MemDev::new(64);
    mkfs(&mut dev, &IdentityTransform, &opts()).unwrap();
    let dev = forge_generation(dev, GEN_LIMIT - 1);

    let mut fs = Rfs2::mount(dev, IdentityTransform, tnow).unwrap();
    assert_eq!(fs.generation(), GEN_LIMIT - 1);
    // One mutation still fits: it commits as GEN_LIMIT...
    fs.create("/last").unwrap();
    fs.commit().unwrap();
    assert_eq!(fs.generation(), GEN_LIMIT);
    // ...and the volume is frozen from then on. gen is never reused: a
    // rewrap under the same DEK would repeat (block, gen) nonces (CRYPTO-1).
    assert_eq!(fs.create("/one-too-many").unwrap_err(), Error::GenExhausted);
    fs.lookup("/last").unwrap();

    // Freeze survives remount.
    let dev = fs.into_device().crash();
    let mut fs = Rfs2::mount(dev, IdentityTransform, tnow).unwrap();
    assert_eq!(fs.generation(), GEN_LIMIT);
    assert_eq!(fs.unlink("/last").unwrap_err(), Error::GenExhausted);
}
