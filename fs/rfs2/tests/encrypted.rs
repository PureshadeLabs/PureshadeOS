//! End-to-end encrypted volume (doc 08): the same RFS2 used with a real
//! `GcmTransform` instead of `IdentityTransform`. Proves the volume-level
//! encryption mode round-trips, the passphrase ceremony works, nonce (`gen`)
//! monotonicity survives a power cycle (the primary CRYPTO-1 requirement),
//! tamper is a loud auth failure, and on-disk blocks are actually ciphertext.

mod common;

use rfs2::crypto::{argon2_block_count, Argon2Block};
use rfs2::layout::{INCOMPAT_ENCRYPTION, RO_COMPAT_HARDLINKS};
use rfs2::{
    mkfs, open_dek, seal_dek, BlockDevice, Error, GcmTransform, IdentityTransform, MkfsOptions,
    Rfs2, StaticHeader, WrapGeometry,
};

use common::{tnow, MemDev, UUID};

const PASS: &[u8] = b"correct horse battery staple";
// Tiny Argon2 params: tests must be fast. Production sizing (64 MiB) is the
// kernel's concern; the crypto is identical.
const M: u32 = 16;
const T: u32 = 1;
const P: u32 = 1;

fn dek_bytes() -> [u8; 32] {
    let mut d = [0u8; 32];
    for (i, b) in d.iter_mut().enumerate() {
        *b = (i as u8).wrapping_mul(37).wrapping_add(11);
    }
    d
}

fn argon_mem() -> Vec<Argon2Block> {
    vec![Argon2Block::new(); argon2_block_count(M, T, P).unwrap()]
}

/// Seal a DEK under PASS into CryptoParams matching what `mkfs` will write.
fn params_for(dek: &[u8; 32], total_blocks: u64) -> rfs2::CryptoParams {
    let geom = WrapGeometry {
        total_blocks,
        uuid: UUID,
        feature_compat: 0,
        feature_incompat: INCOMPAT_ENCRYPTION,
        feature_ro_compat: RO_COMPAT_HARDLINKS,
    };
    seal_dek(PASS, dek, &geom, &[0x5A; 16], M, T, P, &[0x11; 12], &mut argon_mem()).unwrap()
}

/// Format an encrypted volume and return the mounted fs.
fn fresh_encrypted(blocks: u64) -> (Rfs2<MemDev, GcmTransform>, [u8; 32]) {
    let dek = dek_bytes();
    let params = params_for(&dek, blocks);
    let mut dev = MemDev::new(blocks);
    mkfs(
        &mut dev,
        &GcmTransform::new(&dek),
        &MkfsOptions { uuid: UUID, label: "enc", now: tnow(), crypto: Some(params) },
    )
    .unwrap();
    let fs = Rfs2::mount(dev, GcmTransform::new(&dek), tnow).unwrap();
    (fs, dek)
}

#[test]
fn encrypted_roundtrip_across_remount() {
    let (mut fs, dek) = fresh_encrypted(256);
    assert_eq!(fs.generation(), 1);

    let ino = fs.create("/secret.txt").unwrap();
    fs.write_at(ino, 0, b"attack at dawn").unwrap();
    fs.commit().unwrap();

    // Power cycle: only durable state survives; remount under the same DEK.
    let powered = fs.into_device().crash();
    let mut fs = Rfs2::mount(powered, GcmTransform::new(&dek), tnow).unwrap();
    assert_eq!(fs.generation(), 2);
    let ino = fs.lookup("/secret.txt").unwrap();
    let mut buf = [0u8; 32];
    let n = fs.read_at(ino, 0, &mut buf).unwrap();
    assert_eq!(&buf[..n], b"attack at dawn");
}

/// The primary requirement: `gen` is strictly monotonic and **persists across
/// remounts — never reset at mount** (CRYPTO-1). A repeated (block, gen) breaks
/// GCM; this proves the nonce space only ever moves forward across power cycles.
#[test]
fn gen_monotonic_across_power_cycles() {
    let (mut fs, dek) = fresh_encrypted(256);
    let mut gens = vec![fs.generation()];
    for i in 0..5 {
        let ino = fs.create(&format!("/f{i}")).unwrap();
        fs.write_at(ino, 0, format!("data-{i}").as_bytes()).unwrap();
        fs.commit().unwrap();
        let g = fs.generation();
        // Simulate a full power cut, then cold-mount.
        let powered = fs.into_device().crash();
        fs = Rfs2::mount(powered, GcmTransform::new(&dek), tnow).unwrap();
        assert_eq!(fs.generation(), g, "gen must persist across the power cycle, not reset");
        gens.push(g);
    }
    for w in gens.windows(2) {
        assert!(w[1] > w[0], "gen must be strictly increasing: {:?}", gens);
    }
}

#[test]
fn passphrase_ceremony_open_and_wrong() {
    let (fs, dek) = fresh_encrypted(64);
    let mut dev = fs.into_device();
    let mut hbuf = vec![0u8; 4096];
    <MemDev as rfs2::BlockDevice>::read_block(&mut dev, 0, &mut hbuf).unwrap();
    let header = StaticHeader::decode(&hbuf).unwrap();

    // Right passphrase recovers the exact DEK.
    let got = open_dek(PASS, &header, &mut argon_mem()).unwrap();
    assert_eq!(got, dek, "correct passphrase unwraps the DEK");

    // Wrong passphrase fails loud (auth), never a wrong/zero key.
    let bad = open_dek(b"hunter2", &header, &mut argon_mem());
    assert!(bad.is_err(), "wrong passphrase must fail the DEK unwrap");
}

#[test]
fn wrong_dek_cannot_mount() {
    let (fs, _dek) = fresh_encrypted(64);
    let powered = fs.into_device();
    let mut wrong = dek_bytes();
    wrong[0] ^= 0xFF;
    // Feature gate passes (both provide ENCRYPTION); the superblock GCM tag
    // fails under the wrong DEK → no valid superblock → mount denied.
    assert!(Rfs2::mount(powered, GcmTransform::new(&wrong), tnow).is_err());
}

#[test]
fn tampered_ciphertext_block_is_loud() {
    let (mut fs, dek) = fresh_encrypted(64);
    let ino = fs.create("/f").unwrap();
    fs.write_at(ino, 0, b"payload").unwrap();
    fs.commit().unwrap();
    // The inode-map leaf is a known dynamic block; flip a ciphertext byte.
    let leaf = fs.superblock().inode_map_root.block;
    let mut dev = fs.into_device();
    dev.corrupt(leaf, 17);
    // Remount walks the tree from the root; the tampered block fails its parent
    // BlockPtr tag → auth error, never silent stale/garbage plaintext.
    let r = Rfs2::mount(dev, GcmTransform::new(&dek), tnow);
    assert!(r.is_err(), "a tampered ciphertext block must surface as an error");
}

#[test]
fn on_disk_blocks_are_ciphertext() {
    // Same logical volume, encrypted vs plaintext: the root directory block
    // holds the dirent names in the clear under identity, but not under GCM.
    let blocks = 64;
    let dek = dek_bytes();

    let mut enc = MemDev::new(blocks);
    mkfs(
        &mut enc,
        &GcmTransform::new(&dek),
        &MkfsOptions {
            uuid: UUID,
            label: "enc",
            now: tnow(),
            crypto: Some(params_for(&dek, blocks)),
        },
    )
    .unwrap();

    let mut plain = MemDev::new(blocks);
    mkfs(
        &mut plain,
        &IdentityTransform,
        &MkfsOptions { uuid: UUID, label: "plain", now: tnow(), crypto: None },
    )
    .unwrap();

    // Block 3 = root directory data. Plaintext contains "." / ".." dirents;
    // ciphertext must differ and must not leak the raw dirent bytes.
    assert_ne!(enc.block(3), plain.block(3), "encrypted root dir must not equal plaintext");
}

// ── Crash consistency with encryption on (doc 09) ────────────────────────────

/// The dual-superblock atomic commit gives crash consistency for an encrypted
/// volume exactly as for a plaintext one: a crash before the pointer flip
/// recovers the last committed generation, and the GCM tags stay consistent
/// with that committed state (no tag references a torn/uncommitted block).
#[test]
fn encrypted_crash_before_flip_recovers_committed_gen() {
    let (mut fs, dek) = fresh_encrypted(256);
    let a = fs.create("/a").unwrap();
    fs.write_at(a, 0, b"committed").unwrap();
    fs.commit().unwrap();
    assert_eq!(fs.generation(), 2);

    // Stage a second op: fresh (ciphertext) blocks written and even flushed,
    // but crash before the superblock flip.
    let b = fs.create("/b").unwrap();
    fs.write_at(b, 0, b"uncommitted").unwrap();
    fs.device_mut().flush().unwrap();
    assert!(fs.has_staged_changes());

    let dev = fs.into_device().crash();
    let mut fs = Rfs2::mount(dev, GcmTransform::new(&dek), tnow).unwrap();
    assert_eq!(fs.generation(), 2, "recovers the last committed gen, not the torn one");
    let ino = fs.lookup("/a").unwrap();
    let mut buf = [0u8; 16];
    assert_eq!(fs.read_at(ino, 0, &mut buf).unwrap(), 9);
    assert_eq!(&buf[..9], b"committed", "committed ciphertext decrypts cleanly");
    assert_eq!(fs.lookup("/b").unwrap_err(), Error::NotFound, "torn op is absent");
}

/// Store-shape probe with encryption: one sealed entry committed durably, a
/// second entry staged then torn at the superblock write. A remount must show a
/// PRE- or POST-commit state, never a partial one — first entry wholly present
/// and decryptable, second wholly absent — proving the encrypted store keeps
/// crash consistency (both volumes use this same seam).
#[test]
fn encrypted_torn_second_commit_is_atomic() {
    let (mut fs, dek) = fresh_encrypted(256);
    fs.mkdir("/digest-a").unwrap();
    let x = fs.create("/digest-a/x").unwrap();
    fs.write_at(x, 0, b"realized-a").unwrap();
    fs.commit().unwrap();
    let committed_gen = fs.generation();

    // Second realize staged + tree flushed, superblock flip lost to the crash.
    fs.mkdir("/digest-b").unwrap();
    let y = fs.create("/digest-b/y").unwrap();
    fs.write_at(y, 0, b"realized-b").unwrap();
    fs.device_mut().flush().unwrap();

    let dev = fs.into_device().crash();
    let mut fs = Rfs2::mount(dev, GcmTransform::new(&dek), tnow).unwrap();
    assert_eq!(fs.generation(), committed_gen);
    let x = fs.lookup("/digest-a/x").unwrap();
    let mut buf = [0u8; 16];
    assert_eq!(fs.read_at(x, 0, &mut buf).unwrap(), 10);
    assert_eq!(&buf[..10], b"realized-a");
    assert!(fs.lookup("/digest-b").is_err(), "the torn second entry is wholly absent");
}
