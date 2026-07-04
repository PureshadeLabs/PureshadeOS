//! The crypto seam (doc 08, stubbed): the no-op transform round-trips blocks
//! unchanged with zero tags, and a byte-changing transform slots in with no
//! structural change — same call sites, same BlockPtr layout, same offsets.

mod common;

use rfs2::layout::{rd_u64, BLOCK_SIZE, SB_GEN_COPY_OFFSET};
use rfs2::transform::{dyn_aad, TAG_SIZE};
use rfs2::{mkfs, BlockTransform, Error, IdentityTransform, Rfs2};

use common::{opts, tnow, MemDev, XorTransform};

#[test]
fn identity_round_trips_block_unchanged_with_zero_tag() {
    // Proves the seam: encrypt is a no-op, tag is zeroed (as the BlockPtr
    // spec requires of the stub), decrypt verifies and returns the bytes.
    let xf = IdentityTransform;
    let mut buf: Vec<u8> = (0..BLOCK_SIZE).map(|i| (i * 7 % 256) as u8).collect();
    let orig = buf.clone();
    let aad = dyn_aad(42, 7);
    let tag = xf.encrypt_block(42, 7, &aad, &mut buf);
    assert_eq!(tag, [0u8; TAG_SIZE]);
    assert_eq!(buf, orig, "no-op transform must not alter the block");
    xf.decrypt_block(42, 7, &aad, &tag, &mut buf).unwrap();
    assert_eq!(buf, orig);
    // A non-zero recorded tag means the plumbing is broken → refuse.
    assert!(matches!(
        xf.decrypt_block(42, 7, &aad, &[1u8; TAG_SIZE], &mut buf),
        Err(Error::Auth { block: 42 })
    ));
}

#[test]
fn identity_volume_blocks_are_plaintext_with_zero_tags() {
    // End-to-end: with the identity transform, dynamic-region bytes on the
    // device are the plaintext we wrote (offset-stable for the real cipher).
    let mut dev = MemDev::new(128);
    mkfs(&mut dev, &IdentityTransform, &opts()).unwrap();
    let mut fs = Rfs2::mount(dev, IdentityTransform, tnow).unwrap();
    let ino = fs.create("/x").unwrap();
    fs.write_at(ino, 0, b"FIND-ME-ON-DISK").unwrap();
    fs.commit().unwrap();
    let found = fs
        .live_current()
        .iter()
        .any(|&b| fs.device().block(b).windows(15).any(|w| w == b"FIND-ME-ON-DISK"));
    assert!(found, "identity transform must leave plaintext on the device");
}

#[test]
fn xor_transform_swaps_in_with_no_structural_change() {
    // Same mkfs, same mount, same ops — only the transform differs. Data
    // round-trips; the plaintext is NOT on the device; tags are carried in
    // parents and verified on every read.
    let xf = || XorTransform { key: 0x77 };
    let mut dev = MemDev::new(128);
    mkfs(&mut dev, &xf(), &opts()).unwrap();
    let mut fs = Rfs2::mount(dev, xf(), tnow).unwrap();
    let ino = fs.create("/x").unwrap();
    fs.write_at(ino, 0, b"FIND-ME-ON-DISK").unwrap();
    fs.commit().unwrap();

    let mut buf = [0u8; 15];
    fs.read_at(ino, 0, &mut buf).unwrap();
    assert_eq!(&buf, b"FIND-ME-ON-DISK");
    let leaked = fs
        .live_current()
        .iter()
        .any(|&b| fs.device().block(b).windows(15).any(|w| w == b"FIND-ME-ON-DISK"));
    assert!(!leaked, "transformed volume must not contain the plaintext");

    // Remount round-trip with the same transform.
    let dev = fs.into_device().crash();
    let mut fs = Rfs2::mount(dev, xf(), tnow).unwrap();
    let ino = fs.lookup("/x").unwrap();
    let mut buf = [0u8; 15];
    fs.read_at(ino, 0, &mut buf).unwrap();
    assert_eq!(&buf, b"FIND-ME-ON-DISK");
}

#[test]
fn superblock_gen_copy_is_plaintext_under_xor() {
    // Doc 03 §2 (resolved gen-before-decrypt gap): the trailer `gen_copy` is
    // readable on the raw device even when the payload is ciphertext — a real
    // cipher can form the nonce/AAD before decrypting. The XorTransform here
    // uses the *default* `open_superblock`, the same code path GCM will use.
    let xf = || XorTransform { key: 0x3C };
    let mut dev = MemDev::new(128);
    mkfs(&mut dev, &xf(), &opts()).unwrap();
    let mut fs = Rfs2::mount(dev, xf(), tnow).unwrap();
    fs.create("/f").unwrap();
    fs.commit().unwrap(); // gen 2 into slot B
    assert_eq!((fs.generation(), fs.current_slot()), (2, 2));

    let dev = fs.into_device();
    // Raw trailer bytes: plaintext gen, no key needed.
    assert_eq!(rd_u64(&dev.block(2)[SB_GEN_COPY_OFFSET..]), 2);
    assert_eq!(rd_u64(&dev.block(1)[SB_GEN_COPY_OFFSET..]), 1);
    // Payload is NOT plaintext (magic must not appear at offset 0).
    assert_ne!(&dev.block(2)[0..8], b"RFSSB\0\0\0");

    // Round-trip: the trailer-driven open path mounts the volume.
    let fs = Rfs2::mount(dev, xf(), tnow).unwrap();
    assert_eq!(fs.generation(), 2);
}

#[test]
fn tampered_gen_copy_invalidates_the_slot() {
    // A forged trailer gen feeds a wrong nonce/AAD → authentication fails →
    // the slot is invalid and mount falls back (doc 03 §2: gen_copy needs no
    // separate MAC).
    let xf = || XorTransform { key: 0x3C };
    let mut dev = MemDev::new(128);
    mkfs(&mut dev, &xf(), &opts()).unwrap();
    let mut fs = Rfs2::mount(dev, xf(), tnow).unwrap();
    let ino = fs.create("/f").unwrap();
    fs.write_at(ino, 0, b"gen2").unwrap();
    fs.commit().unwrap(); // gen 2 in slot B; gen 1 fallback in slot A

    let mut dev = fs.into_device().crash();
    dev.corrupt(2, SB_GEN_COPY_OFFSET); // flip a byte of slot B's gen_copy
    let mut fs = Rfs2::mount(dev, xf(), tnow).unwrap();
    assert_eq!(fs.generation(), 1); // rolled back exactly one commit
    assert_eq!(fs.lookup("/f").unwrap_err(), Error::NotFound);
}

#[test]
fn wrong_transform_cannot_open_the_volume() {
    // An XOR volume presented to the identity transform (or a wrong key)
    // fails validation instead of yielding garbage — CONSIST-2's "correct
    // data or a reported error, never plausible wrong data".
    let mut dev = MemDev::new(64);
    mkfs(&mut dev, &XorTransform { key: 0x77 }, &opts()).unwrap();
    assert!(Rfs2::mount(dev.clone(), IdentityTransform, tnow).is_err());
    assert!(Rfs2::mount(dev, XorTransform { key: 0x13 }, tnow).is_err());
}
