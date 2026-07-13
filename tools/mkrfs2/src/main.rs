//! mkrfs2 — build an RFS V2 disk image from a directory tree (host tool).
//!
//! Links the `fs/rfs2` crate — the exact code the kernel mounts — so the
//! image is produced by the reference implementation, not a reimplementation
//! of the on-disk format (docs/rfs-v2/10 §3 mkfs).
//!
//! Usage:
//!   mkrfs2 <image> <size>[K|M|G] [rootfs-dir]   format; populate from dir
//!   mkrfs2 --verify <image>                     mount, walk the whole tree,
//!                                               read every file, report
//!
//! `--verify` exits non-zero if the image does not mount or any live block
//! fails to read — used by the crash-consistency harness to prove an
//! interrupted commit recovers to the last-good generation.

extern crate rfs2;

use rfs2::{mkfs, BlockDevice, Error, IdentityTransform, MkfsOptions, Rfs2};
use std::path::Path;
use std::process::exit;
use std::time::{SystemTime, UNIX_EPOCH};

const BLOCK_SIZE: usize = 4096;

// ── In-memory block device ────────────────────────────────────────────────────

struct MemDisk {
    data: Vec<u8>,
}

impl BlockDevice for MemDisk {
    fn total_blocks(&self) -> u64 {
        (self.data.len() / BLOCK_SIZE) as u64
    }

    fn read_block(&mut self, block: u64, buf: &mut [u8]) -> rfs2::Result<()> {
        let off = block as usize * BLOCK_SIZE;
        if off + BLOCK_SIZE > self.data.len() {
            return Err(Error::Io);
        }
        buf.copy_from_slice(&self.data[off..off + BLOCK_SIZE]);
        Ok(())
    }

    fn write_block(&mut self, block: u64, buf: &[u8]) -> rfs2::Result<()> {
        let off = block as usize * BLOCK_SIZE;
        if off + BLOCK_SIZE > self.data.len() {
            return Err(Error::Io);
        }
        self.data[off..off + BLOCK_SIZE].copy_from_slice(buf);
        Ok(())
    }

    fn flush(&mut self) -> rfs2::Result<()> {
        Ok(()) // memory image; durability is the final file write
    }
}

// ── Crash-injection device ────────────────────────────────────────────────────
//
// Models power loss during the commit pointer-flip. Writes before the crash
// point persist in full; the write that crosses the crash point is *torn* —
// only its first `tear_bytes` reach the medium (the rest keep their prior
// contents, exactly as a partial multi-sector device write leaves them); every
// write after is dropped. This reproduces the doc 09 §3 Stage-3 scenarios
// against the real `commit()` ordering, not a hand-forged image.

struct CrashDisk {
    data: Vec<u8>,
    /// Tear the write targeting this block number; `u64::MAX` = never crash.
    /// The superblock write is the only write to a slot block (1 or 2); data
    /// and spine writes all target blocks ≥ 3, so this pinpoints the
    /// pointer-flip write unambiguously.
    crash_block: u64,
    /// Bytes of the torn write that reach the medium (rest keep old content).
    tear_bytes: usize,
    crashed: bool,
}

impl CrashDisk {
    fn new(data: Vec<u8>, crash_block: u64, tear_bytes: usize) -> Self {
        CrashDisk { data, crash_block, tear_bytes, crashed: false }
    }
}

impl BlockDevice for CrashDisk {
    fn total_blocks(&self) -> u64 {
        (self.data.len() / BLOCK_SIZE) as u64
    }

    fn read_block(&mut self, block: u64, buf: &mut [u8]) -> rfs2::Result<()> {
        let off = block as usize * BLOCK_SIZE;
        buf.copy_from_slice(&self.data[off..off + BLOCK_SIZE]);
        Ok(())
    }

    fn write_block(&mut self, block: u64, buf: &[u8]) -> rfs2::Result<()> {
        // Post-crash writes are lost (the device is powered off). The FS still
        // sees Ok — a real device acks into a cache that never reaches the
        // platter; that is precisely the failure the dual superblock defends.
        if self.crashed {
            return Ok(());
        }
        let off = block as usize * BLOCK_SIZE;
        if block == self.crash_block {
            // Torn write: only the leading `tear_bytes` land. The trailer
            // (gen_copy at 4072) sits in the last sector, so a sub-4072 tear
            // leaves the payload's new gen disagreeing with the stale trailer
            // → read_slot rejects the slot (superblock.rs validity check).
            self.data[off..off + self.tear_bytes].copy_from_slice(&buf[..self.tear_bytes]);
            self.crashed = true;
            return Ok(());
        }
        self.data[off..off + BLOCK_SIZE].copy_from_slice(buf);
        Ok(())
    }

    fn flush(&mut self) -> rfs2::Result<()> {
        Ok(())
    }
}

fn now_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

fn parse_size(s: &str) -> Option<u64> {
    let (num, mult) = match s.chars().last()? {
        'K' | 'k' => (&s[..s.len() - 1], 1u64 << 10),
        'M' | 'm' => (&s[..s.len() - 1], 1u64 << 20),
        'G' | 'g' => (&s[..s.len() - 1], 1u64 << 30),
        _ => (s, 1),
    };
    num.parse::<u64>().ok().map(|n| n * mult)
}

fn die(msg: &str) -> ! {
    eprintln!("mkrfs2: {msg}");
    exit(1);
}

// ── Population ────────────────────────────────────────────────────────────────

type Fs = Rfs2<MemDisk, IdentityTransform>;

/// Recursively mirror `host` under `guest` ("" = root). One commit per
/// filesystem object keeps the pending generation's dropped-block garbage
/// bounded, so a small image doesn't run out of staging space.
fn populate(fs: &mut Fs, host: &Path, guest: &str) {
    let mut entries: Vec<_> = std::fs::read_dir(host)
        .unwrap_or_else(|e| die(&format!("readdir {}: {e}", host.display())))
        .map(|e| e.unwrap())
        .collect();
    entries.sort_by_key(|e| e.file_name());

    for entry in entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        let hpath = entry.path();
        let gpath = format!("{guest}/{name}");
        let meta = std::fs::symlink_metadata(&hpath)
            .unwrap_or_else(|e| die(&format!("stat {}: {e}", hpath.display())));

        if meta.file_type().is_symlink() {
            let target = std::fs::read_link(&hpath)
                .unwrap_or_else(|e| die(&format!("readlink {}: {e}", hpath.display())));
            fs.symlink(&target.to_string_lossy(), &gpath)
                .unwrap_or_else(|e| die(&format!("symlink {gpath}: {e:?}")));
        } else if meta.is_dir() {
            fs.mkdir(&gpath)
                .unwrap_or_else(|e| die(&format!("mkdir {gpath}: {e:?}")));
            fs.commit()
                .unwrap_or_else(|e| die(&format!("commit {gpath}: {e:?}")));
            populate(fs, &hpath, &gpath);
            continue;
        } else {
            let data = std::fs::read(&hpath)
                .unwrap_or_else(|e| die(&format!("read {}: {e}", hpath.display())));
            let ino = fs
                .create(&gpath)
                .unwrap_or_else(|e| die(&format!("create {gpath}: {e:?}")));
            if !data.is_empty() {
                fs.write_at(ino, 0, &data)
                    .unwrap_or_else(|e| die(&format!("write {gpath}: {e:?}")));
            }
        }
        fs.commit()
            .unwrap_or_else(|e| die(&format!("commit {gpath}: {e:?}")));
    }
}

// ── Verify ────────────────────────────────────────────────────────────────────

/// Walk the mounted tree, reading every file end to end. Any structural or
/// device error aborts (non-zero exit).
fn verify_walk(fs: &mut Fs, path: &str, files: &mut u64, dirs: &mut u64, bytes: &mut u64) {
    let entries = fs
        .readdir(path)
        .unwrap_or_else(|e| die(&format!("verify readdir {path}: {e:?}")));
    for e in entries {
        if e.name == "." || e.name == ".." {
            continue;
        }
        let child = if path == "/" { format!("/{}", e.name) } else { format!("{path}/{}", e.name) };
        match e.file_type {
            2 => {
                *dirs += 1;
                verify_walk(fs, &child, files, dirs, bytes);
            }
            3 => {
                fs.readlink(&child)
                    .unwrap_or_else(|er| die(&format!("verify readlink {child}: {er:?}")));
                *files += 1;
            }
            _ => {
                let inode = fs
                    .stat(&child)
                    .unwrap_or_else(|er| die(&format!("verify stat {child}: {er:?}")));
                let mut buf = vec![0u8; inode.size as usize];
                let ino = fs.lookup(&child).unwrap();
                let n = fs
                    .read_at(ino, 0, &mut buf)
                    .unwrap_or_else(|er| die(&format!("verify read {child}: {er:?}")));
                if n != inode.size as usize {
                    die(&format!("verify short read {child}: {n} != {}", inode.size));
                }
                *files += 1;
                *bytes += n as u64;
            }
        }
    }
}

fn verify(img_path: &str) {
    let data = std::fs::read(img_path)
        .unwrap_or_else(|e| die(&format!("read {img_path}: {e}")));
    if data.len() % BLOCK_SIZE != 0 {
        die("image size not a multiple of 4096");
    }
    // Raw slot trailer gens (plaintext gen_copy at payload offset 4072), shown
    // before mount so a torn/invalid slot is still reported.
    for (slot, name) in [(1usize, "A"), (2usize, "B")] {
        let off = slot * BLOCK_SIZE + 4072;
        let gen = u64::from_le_bytes(data[off..off + 8].try_into().unwrap());
        println!("slot {name}: trailer gen_copy={gen}");
    }
    let disk = MemDisk { data };
    let mut fs = match Rfs2::mount(disk, IdentityTransform, now_ns) {
        Ok(fs) => fs,
        Err(e) => die(&format!("mount failed: {e:?}")),
    };
    println!(
        "mounted: gen={} slot={} blocks={} inodes={} live_cur={} live_prev={}{}",
        fs.generation(),
        if fs.current_slot() == 1 { "A" } else { "B" },
        fs.superblock().block_count,
        fs.superblock().inode_count,
        fs.live_current().len(),
        fs.live_previous().len(),
        if fs.block_count_mismatch() { " (block_count mismatch)" } else { "" },
    );
    let (mut files, mut dirs, mut bytes) = (0u64, 0u64, 0u64);
    verify_walk(&mut fs, "/", &mut files, &mut dirs, &mut bytes);
    println!("verify OK: {dirs} dirs, {files} files, {bytes} bytes readable");
}

// ── Crash-consistency gate ────────────────────────────────────────────────────
//
// Acceptance test (doc 09 §3): interrupt the commit pointer-flip mid-write and
// confirm remount recovers to exactly one of {K, K+1} — never a blend. Runs
// the real `create` + `commit` against a device that loses the superblock
// write, then remounts with the same `Rfs2::mount` the kernel uses.

fn slot_gens(data: &[u8]) -> (u64, u64) {
    let a = u64::from_le_bytes(data[1 * BLOCK_SIZE + 4072..1 * BLOCK_SIZE + 4080].try_into().unwrap());
    let b = u64::from_le_bytes(data[2 * BLOCK_SIZE + 4072..2 * BLOCK_SIZE + 4080].try_into().unwrap());
    (a, b)
}

fn crash_test(img_path: &str) {
    let base = std::fs::read(img_path).unwrap_or_else(|e| die(&format!("read {img_path}: {e}")));

    // Establish the pre-crash committed state.
    let fs0 = Rfs2::mount(MemDisk { data: base.clone() }, IdentityTransform, now_ns)
        .unwrap_or_else(|e| die(&format!("baseline mount: {e:?}")));
    let k = fs0.generation();
    let cur = fs0.current_slot();
    let target_slot = if cur == 1 { 2u64 } else { 1u64 }; // inactive slot the flip targets
    println!("baseline: gen={k} current_slot={} → commit K+1 targets slot {target_slot}",
        if cur == 1 { "A" } else { "B" });
    drop(fs0);

    let mut failures = 0;
    let probe = "/crash_probe";

    // ── Scenario 1: torn superblock write ────────────────────────────────────
    // Only the first sector of the new superblock lands; the trailer gen_copy
    // stays at K's neighbour. read_slot must reject it → recover to K.
    {
        let dev = CrashDisk::new(base.clone(), target_slot, 512);
        let mut fs = Rfs2::mount(dev, IdentityTransform, now_ns)
            .unwrap_or_else(|e| die(&format!("torn: mount: {e:?}")));
        let ino = fs.create(probe).unwrap_or_else(|e| die(&format!("torn: create: {e:?}")));
        fs.write_at(ino, 0, b"uncommitted").unwrap_or_else(|e| die(&format!("torn: write: {e:?}")));
        // This commit's superblock write is torn by CrashDisk.
        let _ = fs.commit();
        let torn = fs.into_device().data;

        let (ga, gb) = slot_gens(&torn);
        let mut r = Rfs2::mount(MemDisk { data: torn }, IdentityTransform, now_ns)
            .unwrap_or_else(|e| die(&format!("torn: remount failed (should recover, not fail): {e:?}")));
        let recovered = r.generation();
        let probe_present = r.lookup(probe).is_ok();
        let ok = recovered == k && !probe_present;
        println!(
            "  [torn commit]  slot gens after crash: A={ga} B={gb} → remount gen={recovered} probe_present={probe_present}  {}",
            if ok { "PASS (recovered to last-good K)" } else { failures += 1; "FAIL" },
        );
    }

    // ── Scenario 2: clean commit (no crash) ──────────────────────────────────
    // The complement: a fully-persisted flip must advance to K+1 and expose the
    // change. Proves the flip is not merely "always falls back."
    {
        let dev = MemDisk { data: base.clone() };
        let mut fs = Rfs2::mount(dev, IdentityTransform, now_ns)
            .unwrap_or_else(|e| die(&format!("clean: mount: {e:?}")));
        let ino = fs.create(probe).unwrap_or_else(|e| die(&format!("clean: create: {e:?}")));
        fs.write_at(ino, 0, b"committed").unwrap_or_else(|e| die(&format!("clean: write: {e:?}")));
        fs.commit().unwrap_or_else(|e| die(&format!("clean: commit: {e:?}")));
        let good = fs.into_device().data;

        let (ga, gb) = slot_gens(&good);
        let mut r = Rfs2::mount(MemDisk { data: good }, IdentityTransform, now_ns)
            .unwrap_or_else(|e| die(&format!("clean: remount: {e:?}")));
        let recovered = r.generation();
        let probe_present = r.lookup(probe).is_ok();
        let landed_slot = r.current_slot();
        let ok = recovered == k + 1 && probe_present && landed_slot == target_slot;
        println!(
            "  [clean commit] slot gens: A={ga} B={gb} → remount gen={recovered} slot={} probe_present={probe_present}  {}",
            if landed_slot == 1 { "A" } else { "B" },
            if ok { "PASS (advanced to K+1)" } else { failures += 1; "FAIL" },
        );
    }

    // ── Scenario 3: torn write, then a *further* clean commit ────────────────
    // After recovering to K, the volume must still be writable and commit
    // normally (the abandoned torn slot is reclaimable, not poisoned).
    {
        let dev = CrashDisk::new(base.clone(), target_slot, 512);
        let mut fs = Rfs2::mount(dev, IdentityTransform, now_ns).unwrap();
        let ino = fs.create(probe).unwrap();
        fs.write_at(ino, 0, b"lost").unwrap();
        let _ = fs.commit(); // torn
        let torn = fs.into_device().data;

        // Remount the recovered volume on a clean device and commit for real.
        let mut r = Rfs2::mount(MemDisk { data: torn }, IdentityTransform, now_ns)
            .unwrap_or_else(|e| die(&format!("recover-then-write: remount: {e:?}")));
        let base_gen = r.generation();
        let ino = r.create("/after_recovery").unwrap_or_else(|e| die(&format!("post-recovery create: {e:?}")));
        r.write_at(ino, 0, b"ok").unwrap();
        r.commit().unwrap_or_else(|e| die(&format!("post-recovery commit: {e:?}")));
        let after = r.into_device().data;

        let mut r2 = Rfs2::mount(MemDisk { data: after }, IdentityTransform, now_ns)
            .unwrap_or_else(|e| die(&format!("post-recovery remount: {e:?}")));
        let ok = r2.generation() == base_gen + 1 && r2.lookup("/after_recovery").is_ok();
        println!(
            "  [recover+write] recovered gen={base_gen} → new commit gen={} file_present={}  {}",
            r2.generation(),
            r2.lookup("/after_recovery").is_ok(),
            if ok { "PASS (volume writable after recovery)" } else { failures += 1; "FAIL" },
        );
    }

    if failures == 0 {
        println!("crash-consistency gate: PASS — pointer-flip atomic on torn write; recovers to last-good gen");
    } else {
        eprintln!("crash-consistency gate: FAIL ({failures} scenario(s))");
        exit(1);
    }
}

// ── main ──────────────────────────────────────────────────────────────────────

fn main() {
    let args: Vec<String> = std::env::args().collect();

    if args.len() == 3 && args[1] == "--verify" {
        verify(&args[2]);
        return;
    }
    if args.len() == 3 && args[1] == "--crash-test" {
        crash_test(&args[2]);
        return;
    }
    if args.len() < 3 || args.len() > 4 {
        die("usage: mkrfs2 <image> <size>[K|M|G] [rootfs-dir] | mkrfs2 --verify <image> | mkrfs2 --crash-test <image>");
    }

    let img_path = &args[1];
    let size = parse_size(&args[2]).unwrap_or_else(|| die("bad size"));
    if size % BLOCK_SIZE as u64 != 0 {
        die("size must be a multiple of 4096");
    }

    // Pseudo-random per-volume uuid (no external deps): time + pid mix.
    let mut uuid = [0u8; 16];
    let seed = now_ns() ^ (std::process::id() as u64).rotate_left(32);
    uuid[..8].copy_from_slice(&seed.to_le_bytes());
    uuid[8..].copy_from_slice(&seed.wrapping_mul(0x9E37_79B9_7F4A_7C15).to_le_bytes());

    let mut disk = MemDisk { data: vec![0u8; size as usize] };
    mkfs(
        &mut disk,
        &IdentityTransform,
        &MkfsOptions { uuid, label: "pureshade-root", now: now_ns() },
    )
    .unwrap_or_else(|e| die(&format!("mkfs: {e:?}")));

    let mut fs = Rfs2::mount(disk, IdentityTransform, now_ns)
        .unwrap_or_else(|e| die(&format!("mount after mkfs: {e:?}")));

    if let Some(rootfs) = args.get(3) {
        populate(&mut fs, Path::new(rootfs), "");
    }
    fs.commit().unwrap_or_else(|e| die(&format!("final commit: {e:?}")));
    let gen = fs.generation();
    let blocks = fs.superblock().block_count;
    let inodes = fs.superblock().inode_count;

    let disk = fs.into_device();
    std::fs::write(img_path, &disk.data)
        .unwrap_or_else(|e| die(&format!("write {img_path}: {e}")));
    println!(
        "mkrfs2: {img_path} ({} MiB): gen={gen} blocks={blocks} inodes={inodes}",
        size >> 20,
    );
}
