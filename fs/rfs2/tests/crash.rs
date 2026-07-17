//! Crash consistency (docs 03 §5, 04 §4–5, 09): the mountable state is always
//! exactly the last committed generation; the flip is atomic.
//!
//! Crash simulation is at whole-block-write granularity (writes since the
//! last barrier are lost). Sub-block torn-write *detection* is the real GCM
//! transform's job (tag mismatch) and is exercised here via the XOR transform,
//! which carries real tags; the identity transform cannot detect tears.

mod common;

use rfs2::{mkfs, BlockDevice, Error, IdentityTransform, Rfs2};

use common::{fresh_fs, opts, tnow, MemDev, XorTransform};

#[test]
fn crash_before_flip_recovers_prior_committed_gen() {
    // Commit gen 2 with file A; stage file B (blocks written + even flushed)
    // but crash before the superblock flip. Mount must see gen 2, A only.
    let mut fs = fresh_fs(256);
    let a = fs.create("/a").unwrap();
    fs.write_at(a, 0, b"committed").unwrap();
    fs.commit().unwrap();
    assert_eq!(fs.generation(), 2);

    let b = fs.create("/b").unwrap();
    fs.write_at(b, 0, b"uncommitted").unwrap();
    // Stage-2 crash point: staged tree blocks durable, superblock not written.
    fs.device_mut().flush().unwrap();
    assert!(fs.has_staged_changes());

    let dev = fs.into_device().crash();
    let mut fs = Rfs2::mount(dev, IdentityTransform, tnow).unwrap();
    assert_eq!(fs.generation(), 2);
    let ino = fs.lookup("/a").unwrap();
    let mut buf = [0u8; 16];
    assert_eq!(fs.read_at(ino, 0, &mut buf).unwrap(), 9);
    assert_eq!(&buf[..9], b"committed");
    assert_eq!(fs.lookup("/b").unwrap_err(), Error::NotFound);
    // The staged garbage blocks are unreferenced → free again (doc 09 §3
    // stage 1: "no leak beyond transient free-space until remount").
    assert_eq!(fs.superblock().block_count as usize, fs.live_current().len());
}

#[test]
fn store_realize_survives_torn_superblock_atomically() {
    // Persistent /shade/store guarantee (Task 5): the store is backed by a
    // block device (VirtioDisk on store.img) implementing the SAME `BlockDevice`
    // contract as this `MemDev`, so RFS2's dual-superblock atomic commit gives
    // the store crash consistency for free. Model a realize: commit a whole
    // "sealed" store entry tree (durable), then stage a SECOND realize and tear
    // the superblock write (crash before the pointer flip). A remount must show
    // the store in a PRE- or POST-commit state, never a corrupt/partial one:
    // the first entry wholly present, the second wholly absent.
    let mut fs = fresh_fs(256);
    // First realize: /<digest-a>/bin/x — committed & durable (the sealed entry).
    fs.mkdir("/digest-a").unwrap();
    fs.mkdir("/digest-a/bin").unwrap();
    let x = fs.create("/digest-a/bin/x").unwrap();
    fs.write_at(x, 0, b"realized-a").unwrap();
    fs.commit().unwrap();
    assert_eq!(fs.generation(), 2);

    // Second realize staged (blocks written and even flushed) but the commit's
    // superblock flip never lands — the torn write.
    fs.mkdir("/digest-b").unwrap();
    fs.mkdir("/digest-b/bin").unwrap();
    let y = fs.create("/digest-b/bin/y").unwrap();
    fs.write_at(y, 0, b"realized-b").unwrap();
    fs.device_mut().flush().unwrap();
    assert!(fs.has_staged_changes());

    // Crash and remount: pre-commit state only.
    let dev = fs.into_device().crash();
    let mut fs = Rfs2::mount(dev, IdentityTransform, tnow).unwrap();
    assert_eq!(fs.generation(), 2, "remount pins the last committed gen");
    let a = fs.lookup("/digest-a/bin/x").unwrap();
    let mut buf = [0u8; 16];
    assert_eq!(fs.read_at(a, 0, &mut buf).unwrap(), 10);
    assert_eq!(&buf[..10], b"realized-a", "sealed entry survives intact");
    // The torn second realize left nothing behind — not a partial tree.
    assert_eq!(fs.lookup("/digest-b").unwrap_err(), Error::NotFound);
    assert_eq!(fs.lookup("/digest-b/bin/y").unwrap_err(), Error::NotFound);
    // No block leak: staged garbage is free again after remount.
    assert_eq!(fs.superblock().block_count as usize, fs.live_current().len());
}

#[test]
fn crash_after_flip_keeps_the_transaction() {
    let mut fs = fresh_fs(256);
    let a = fs.create("/a").unwrap();
    fs.write_at(a, 0, b"payload").unwrap();
    fs.commit().unwrap(); // gen 2 durable (commit ends with a barrier)

    let dev = fs.into_device().crash();
    let mut fs = Rfs2::mount(dev, IdentityTransform, tnow).unwrap();
    assert_eq!(fs.generation(), 2);
    let ino = fs.lookup("/a").unwrap();
    let mut buf = [0u8; 8];
    assert_eq!(fs.read_at(ino, 0, &mut buf).unwrap(), 7);
    assert_eq!(&buf[..7], b"payload");
}

#[test]
fn commits_alternate_slots_and_gens_increase() {
    // Doc 03 §4: commit targets the inactive slot; gen strictly increases.
    let mut fs = fresh_fs(256);
    assert_eq!((fs.generation(), fs.current_slot()), (1, 1));
    fs.create("/f1").unwrap();
    fs.commit().unwrap();
    assert_eq!((fs.generation(), fs.current_slot()), (2, 2));
    fs.create("/f2").unwrap();
    fs.commit().unwrap();
    assert_eq!((fs.generation(), fs.current_slot()), (3, 1));
    fs.create("/f3").unwrap();
    fs.commit().unwrap();
    assert_eq!((fs.generation(), fs.current_slot()), (4, 2));
    // Empty commit is a no-op: no gen burn, no slot write.
    fs.commit().unwrap();
    assert_eq!((fs.generation(), fs.current_slot()), (4, 2));
}

#[test]
fn corrupted_current_slot_falls_back_to_previous_gen() {
    // Torn/corrupt superblock write detection needs real tags → XOR transform.
    // After gen 2 commits into slot B, corrupt slot B: mount must fall back
    // to gen 1 in slot A (doc 03 §5), losing exactly the gen-2 transaction.
    let mut dev = MemDev::new(256);
    let xf = || XorTransform { key: 0x5A };
    mkfs(&mut dev, &xf(), &opts()).unwrap();
    let mut fs = Rfs2::mount(dev, xf(), tnow).unwrap();
    let a = fs.create("/a").unwrap();
    fs.write_at(a, 0, b"gen2 data").unwrap();
    fs.commit().unwrap();
    assert_eq!((fs.generation(), fs.current_slot()), (2, 2));

    let mut dev = fs.into_device().crash();
    dev.corrupt(2, 3000); // one flipped byte in slot B's payload → tag mismatch
    let mut fs = Rfs2::mount(dev, xf(), tnow).unwrap();
    assert_eq!(fs.generation(), 1);
    assert_eq!(fs.lookup("/a").unwrap_err(), Error::NotFound);
}

#[test]
fn both_slots_invalid_refuses_mount() {
    let mut dev = MemDev::new(64);
    let xf = || XorTransform { key: 0x5A };
    mkfs(&mut dev, &xf(), &opts()).unwrap();
    let mut dev = dev.crash();
    dev.corrupt(1, 100); // only valid slot (A) corrupted; B is blank
    assert!(matches!(
        Rfs2::mount(dev, xf(), tnow),
        Err(Error::NoSuperblock)
    ));
}

#[test]
fn tampered_live_block_is_detected_not_tolerated() {
    // CRYPTO-2 / CONSIST-2: corruption inside the live tree surfaces as
    // Error::Auth at mount (mark-and-sweep authenticates every dereference).
    let mut dev = MemDev::new(256);
    let xf = || XorTransform { key: 0x21 };
    mkfs(&mut dev, &xf(), &opts()).unwrap();
    let mut fs = Rfs2::mount(dev, xf(), tnow).unwrap();
    let a = fs.create("/a").unwrap();
    fs.write_at(a, 0, &[9u8; 5000]).unwrap();
    fs.commit().unwrap();
    let victim = *fs.live_current().last().unwrap();

    let mut dev = fs.into_device().crash();
    dev.corrupt(victim, 1234);
    assert!(matches!(
        Rfs2::mount(dev, xf(), tnow),
        Err(Error::Auth { block }) if block == victim
    ));
}

#[test]
fn uncommitted_drop_loses_nothing_committed() {
    // Dropping the fs (unmount without sync) = stage-0/1 crash.
    let mut fs = fresh_fs(256);
    fs.create("/keep").unwrap();
    fs.commit().unwrap();
    fs.create("/lose").unwrap(); // staged only
    let dev = fs.into_device().crash();
    let mut fs = Rfs2::mount(dev, IdentityTransform, tnow).unwrap();
    fs.lookup("/keep").unwrap();
    assert_eq!(fs.lookup("/lose").unwrap_err(), Error::NotFound);
}
