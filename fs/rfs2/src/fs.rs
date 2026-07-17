//! The filesystem proper: mkfs (doc 10), mount + recovery (doc 09), the COW
//! write path and commit (doc 04), inode map / block map (doc 06), and
//! directory operations (doc 07).
//!
//! Transaction model (doc 04 §3 baseline): mutating operations stage COW
//! writes eagerly — fresh blocks land on disk immediately, stamped with the
//! pending generation `K+1` — but nothing becomes visible until [`Rfs2::commit`]
//! writes the new superblock into the inactive slot. Any number of operations
//! share one commit. A crash (or drop) before commit recovers to generation
//! `K`; the staged blocks are unreferenced garbage reclaimed by mark-and-sweep
//! at the next mount. Blocks superseded twice within one transaction are
//! emitted twice (the second supersedes the first as garbage); the spec's
//! in-memory coalescing is an optimization, not a correctness requirement.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use crate::device::BlockDevice;
use crate::dirent;
use crate::inode::{Inode, FLAG_FAST_SYMLINK, FLAG_USED, S_IFDIR, S_IFLNK, S_IFREG};
use crate::layout::{
    BlockPtr, StaticHeader, BLOCK_PTR_SIZE, BLOCK_SIZE, FANOUT, FIRST_DATA_BLOCK,
    FIRST_GENERAL_INO, FORMAT_VERSION, GEN_LIMIT, HEADER_VERSION, INLINE_SYMLINK_MAX,
    INODES_PER_LEAF, INODE_SIZE, KNOWN_INCOMPAT, KNOWN_RO_COMPAT, RO_COMPAT_HARDLINKS,
    ROOT_INO, SB_SLOT_A, SB_SLOT_B,
};
use crate::space::{self, Bitmap};
use crate::superblock::{self, Superblock};
use crate::transform::{dyn_aad, BlockTransform};
use crate::{Error, Result};

// ── Radix geometry (doc 06) ──────────────────────────────────────────────────

/// Inodes addressable at inode-map height `h`: `32 × 128^h`.
fn imap_capacity(height: u64) -> u64 {
    128u64
        .checked_pow(height as u32)
        .and_then(|v| v.checked_mul(INODES_PER_LEAF as u64))
        .unwrap_or(u64::MAX)
}

/// Logical blocks addressable at block-map height `h` (doc 06 §4).
fn bmap_capacity(height: u64) -> u64 {
    if height == 0 {
        1
    } else {
        128u64.checked_pow(height as u32).unwrap_or(u64::MAX)
    }
}

fn imap_index(ino: u64, level: u64) -> usize {
    ((ino >> (5 + 7 * (level - 1))) & 0x7f) as usize
}

fn bmap_index(l: u64, level: u64) -> usize {
    ((l >> (7 * (level - 1))) & 0x7f) as usize
}

fn ptr_at(buf: &[u8], idx: usize) -> BlockPtr {
    BlockPtr::from_bytes(&buf[idx * BLOCK_PTR_SIZE..(idx + 1) * BLOCK_PTR_SIZE])
}

fn set_ptr_at(buf: &mut [u8], idx: usize, ptr: &BlockPtr) {
    buf[idx * BLOCK_PTR_SIZE..(idx + 1) * BLOCK_PTR_SIZE].copy_from_slice(&ptr.to_bytes());
}

// ── mkfs (doc 10 §3) ─────────────────────────────────────────────────────────

pub struct MkfsOptions<'a> {
    pub uuid: [u8; 16],
    pub label: &'a str,
    /// Wall-clock ns for `btime`/`commit_time` (informational).
    pub now: u64,
    /// Encrypted-volume header material (doc 08). `Some` ⇒ the static header
    /// records the KDF params + wrapped DEK and the volume is sealed with the
    /// (encrypting) `xform`; `None` ⇒ a plaintext volume. Must agree with the
    /// transform: `xform.incompat_features()` is written as `feature_incompat`,
    /// so an encrypting transform + `None` (or vice-versa) is a caller bug the
    /// mount-time feature gate will later reject.
    pub crypto: Option<crate::crypto::CryptoParams>,
}

/// Format a device: static header, root directory (inode 1) with `.`/`..`,
/// initial inode map, first superblock (`gen = 1`) in slot A, slot B blank.
pub fn mkfs<D: BlockDevice, T: BlockTransform>(
    dev: &mut D,
    xform: &T,
    opts: &MkfsOptions,
) -> Result<()> {
    let total = dev.total_blocks();
    if total < FIRST_DATA_BLOCK + 2 {
        return Err(Error::NoSpace);
    }
    let gen = 1u64;

    // Invalidate both slots first: stale bytes on a reused device must not
    // parse as a valid superblock.
    let zeros = vec![0u8; BLOCK_SIZE];
    dev.write_block(SB_SLOT_A, &zeros)?;
    dev.write_block(SB_SLOT_B, &zeros)?;

    // Root directory data block at 3.
    let dir_block = FIRST_DATA_BLOCK;
    let mut dirbuf = vec![0u8; BLOCK_SIZE];
    dirent::init_empty(&mut dirbuf);
    dirent::insert(&mut dirbuf, ROOT_INO, dirent::FT_DIR, b".")?;
    dirent::insert(&mut dirbuf, ROOT_INO, dirent::FT_DIR, b"..")?;
    let aad = dyn_aad(dir_block, gen);
    let tag = xform.encrypt_block(dir_block, gen, &aad, &mut dirbuf);
    dev.write_block(dir_block, &dirbuf)?;
    let dir_ptr = BlockPtr { block: dir_block, gen, tag };

    // Inode-map leaf (height 0 root) at 4, holding inode 1.
    let leaf_block = FIRST_DATA_BLOCK + 1;
    let root_inode = Inode {
        mode: S_IFDIR | 0o755,
        flags: FLAG_USED,
        nlink: 2,
        size: BLOCK_SIZE as u64,
        blocks: 1,
        mtime: opts.now,
        ctime: opts.now,
        atime: opts.now,
        btime: opts.now,
        inode_gen: gen,
        bmap_height: 0,
        bmap_root: dir_ptr,
        ..Inode::FREE
    };
    let mut leafbuf = vec![0u8; BLOCK_SIZE];
    let slot = ROOT_INO as usize * INODE_SIZE;
    leafbuf[slot..slot + INODE_SIZE].copy_from_slice(&root_inode.encode());
    let aad = dyn_aad(leaf_block, gen);
    let tag = xform.encrypt_block(leaf_block, gen, &aad, &mut leafbuf);
    dev.write_block(leaf_block, &leafbuf)?;
    let imap_root = BlockPtr { block: leaf_block, gen, tag };

    // Static header (block 0).
    let mut label = [0u8; 64];
    let lb = opts.label.as_bytes();
    if lb.len() > 64 {
        return Err(Error::Invalid);
    }
    label[..lb.len()].copy_from_slice(lb);
    let header = StaticHeader {
        format_version: FORMAT_VERSION,
        header_version: HEADER_VERSION,
        block_size: BLOCK_SIZE as u32,
        total_blocks: total,
        sb_slot_a: SB_SLOT_A,
        sb_slot_b: SB_SLOT_B,
        first_data_block: FIRST_DATA_BLOCK,
        uuid: opts.uuid,
        feature_compat: 0,
        // Honest labelling: the transform decides whether the volume is
        // encrypted. Identity → 0; the GCM transform → INCOMPAT_ENCRYPTION.
        // TODO(crypto): with doc 08 implemented this is always ENCRYPTION.
        feature_incompat: xform.incompat_features(),
        // Hard links are baseline in V2.0 (doc 10 §2): a reader without
        // hardlink-aware freeing may read but must not write.
        feature_ro_compat: RO_COMPAT_HARDLINKS,
        // Crypto header fields: from `opts.crypto` for an encrypted volume,
        // else the plaintext defaults (Argon2id id + baseline params recorded
        // but unused; salt + wrap all-zero). The identity path's on-disk bytes
        // are unchanged from before this field existed.
        kdf_algo: opts.crypto.as_ref().map_or(1, |c| c.kdf_algo),
        kdf_salt: opts.crypto.as_ref().map_or([0; 16], |c| c.kdf_salt),
        argon_m_cost: opts.crypto.as_ref().map_or(65536, |c| c.argon_m_cost),
        argon_t_cost: opts.crypto.as_ref().map_or(3, |c| c.argon_t_cost),
        argon_p: opts.crypto.as_ref().map_or(1, |c| c.argon_p),
        dek_wrap_nonce: opts.crypto.as_ref().map_or([0; 12], |c| c.dek_wrap_nonce),
        dek_wrapped: opts.crypto.as_ref().map_or([0; 32], |c| c.dek_wrapped),
        dek_wrap_tag: opts.crypto.as_ref().map_or([0; 16], |c| c.dek_wrap_tag),
        label,
    };
    let mut hbuf = vec![0u8; BLOCK_SIZE];
    header.encode(&mut hbuf);
    dev.write_block(0, &hbuf)?;

    // Barrier, then the first superblock — same ordering discipline as a
    // commit (COW-3): tree durable before the superblock that references it.
    dev.flush()?;
    let sb = Superblock {
        gen,
        total_blocks: total,
        inode_map_root: imap_root,
        inode_map_height: 0,
        // General-allocation high-water mark (doc 06 §3, doc 10 §3): starts
        // past the permanently-reserved band 2–9. Allocation is bump-only;
        // inode numbers are never recycled.
        next_inode: FIRST_GENERAL_INO,
        inode_count: 1,
        block_count: 2, // dir block + inode-map leaf
        commit_time: opts.now,
        uuid: opts.uuid,
    };
    superblock::write_slot(dev, xform, SB_SLOT_A, &sb)?;
    dev.flush()
}

// ── Mounted filesystem ───────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntryOut {
    pub ino: u64,
    pub file_type: u8,
    pub name: String,
}

struct OpSnapshot {
    root: BlockPtr,
    height: u64,
    next_inode: u64,
    inode_count: u64,
    dirty: bool,
    retired: BTreeSet<u64>,
}

pub struct Rfs2<D: BlockDevice, T: BlockTransform> {
    dev: D,
    xform: T,
    header: StaticHeader,
    clock: fn() -> u64,
    read_only: bool,

    // Committed state.
    sb: Superblock,
    cur_slot: u64,
    /// Blocks reachable from the current superblock (dynamic region only).
    live_cur: Bitmap,
    /// Blocks reachable from the previous still-valid superblock; empty
    /// bitmap if the other slot is invalid. Two-live-trees rule (doc 04 §6).
    live_prev: Bitmap,
    /// Advisory `block_count` disagreed with mark-and-sweep at mount
    /// (logged-not-fatal per doc 05 §2).
    block_count_mismatch: bool,

    // Staged (uncommitted) state for generation `sb.gen + 1`.
    staged_root: BlockPtr,
    staged_height: u64,
    staged_next_inode: u64,
    staged_inode_count: u64,
    dirty: bool,
    /// All blocks allocated+written in the pending generation.
    fresh: BTreeSet<u64>,
    /// Fresh blocks superseded/abandoned within the pending generation.
    /// Excluded from the next live set, but not reallocatable until commit —
    /// reusing one within the same generation would reuse a `(block, gen)`
    /// nonce (CRYPTO-1, doc 08 §4).
    dropped_fresh: BTreeSet<u64>,
    /// Committed (live_cur) blocks superseded by the pending generation.
    retired: BTreeSet<u64>,
    /// Allocations made by the operation in progress (rollback log).
    op_fresh: Vec<u64>,
    alloc_cursor: u64,
    /// Open-handle pins (doc 06 §5): ino → refcount. An inode whose `nlink`
    /// hits 0 while pinned becomes an on-disk orphan (`USED`, `nlink = 0`)
    /// and is freed at the last unpin — or reclaimed by the next rw mount.
    pins: BTreeMap<u64, u32>,
}

impl<D: BlockDevice, T: BlockTransform> Rfs2<D, T> {
    // ── Mount (doc 09 §4) ────────────────────────────────────────────────────

    pub fn mount(mut dev: D, xform: T, clock: fn() -> u64) -> Result<Self> {
        // 1. Static header (plaintext).
        let mut hbuf = vec![0u8; BLOCK_SIZE];
        dev.read_block(0, &mut hbuf)?;
        let header = StaticHeader::decode(&hbuf)?;
        if header.total_blocks > dev.total_blocks()
            || header.sb_slot_a != SB_SLOT_A
            || header.sb_slot_b != SB_SLOT_B
            || header.first_data_block != FIRST_DATA_BLOCK
        {
            return Err(Error::BadHeader);
        }

        // Feature gate (doc 10 §2). ENCRYPTION is understood but only honored
        // when the active transform provides it; a mismatch either way is a
        // refusal, not a silent misread.
        let provided = xform.incompat_features();
        if header.feature_incompat & !KNOWN_INCOMPAT != 0
            || header.feature_incompat != provided
        {
            return Err(Error::Unsupported);
        }
        let read_only = header.feature_ro_compat & !KNOWN_RO_COMPAT != 0;

        // 2. TODO(crypto): derive KEK (Argon2id) and unwrap the DEK here
        // (doc 08 §6). The identity transform needs no key material.

        // 3–4. Read both slots; current = valid slot with higher gen.
        let sa = superblock::read_slot(&mut dev, &xform, SB_SLOT_A, &header);
        let sbb = superblock::read_slot(&mut dev, &xform, SB_SLOT_B, &header);
        let (cur, cur_slot, prev) = match (sa, sbb) {
            (Some(a), Some(b)) => {
                if a.gen == b.gen {
                    // SB-2: gen never reused; two valid slots at the same gen
                    // is structurally impossible on a conformant volume.
                    return Err(Error::Corrupt);
                } else if a.gen > b.gen {
                    (a, SB_SLOT_A, Some(b))
                } else {
                    (b, SB_SLOT_B, Some(a))
                }
            }
            (Some(a), None) => (a, SB_SLOT_A, None),
            (None, Some(b)) => (b, SB_SLOT_B, None),
            (None, None) => return Err(Error::NoSuperblock),
        };

        // 5. Mark-and-sweep from both valid roots (SPACE-1).
        let mut live_cur = Bitmap::new(header.total_blocks);
        let marked =
            space::mark_tree(&mut dev, &xform, &cur.inode_map_root, cur.inode_map_height, &mut live_cur)?;
        let block_count_mismatch = marked != cur.block_count;
        let mut live_prev = Bitmap::new(header.total_blocks);
        if let Some(p) = &prev {
            space::mark_tree(&mut dev, &xform, &p.inode_map_root, p.inode_map_height, &mut live_prev)?;
        }

        let mut fs = Rfs2 {
            dev,
            xform,
            clock,
            read_only,
            staged_root: cur.inode_map_root,
            staged_height: cur.inode_map_height,
            staged_next_inode: cur.next_inode,
            staged_inode_count: cur.inode_count,
            header,
            sb: cur,
            cur_slot,
            live_cur,
            live_prev,
            block_count_mismatch,
            dirty: false,
            fresh: BTreeSet::new(),
            dropped_fresh: BTreeSet::new(),
            retired: BTreeSet::new(),
            op_fresh: Vec::new(),
            alloc_cursor: 0,
            pins: BTreeMap::new(),
        };

        // 6. Reclaim orphaned inodes (doc 09 §4): USED with nlink == 0, left
        // behind by a session that ended with unlinked-but-pinned files.
        if !fs.read_only && fs.sb.gen < GEN_LIMIT {
            fs.reclaim_orphans()?;
        }
        Ok(fs)
    }

    /// Free every orphan (`USED`, `nlink == 0`) in one immediately-committed
    /// transaction. No orphans → no transaction, no generation burned.
    fn reclaim_orphans(&mut self) -> Result<()> {
        let mut orphans = Vec::new();
        for ino in 1..self.staged_next_inode {
            if let Some(inode) = self.imap_get(ino)? {
                if inode.nlink == 0 {
                    orphans.push((ino, inode));
                }
            }
        }
        if orphans.is_empty() {
            return Ok(());
        }
        let snap = self.begin_op();
        for (ino, inode) in orphans {
            if let Err(e) = self.free_inode(ino, &inode) {
                self.fail_op(snap);
                return Err(e);
            }
        }
        self.op_fresh.clear();
        self.commit()
    }

    // ── Accessors (also used by tests) ───────────────────────────────────────

    pub fn generation(&self) -> u64 {
        self.sb.gen
    }
    pub fn current_slot(&self) -> u64 {
        self.cur_slot
    }
    pub fn superblock(&self) -> &Superblock {
        &self.sb
    }
    pub fn header(&self) -> &StaticHeader {
        &self.header
    }
    pub fn inode_map_height(&self) -> u64 {
        self.staged_height
    }
    pub fn is_read_only(&self) -> bool {
        self.read_only
    }
    pub fn block_count_mismatch(&self) -> bool {
        self.block_count_mismatch
    }
    pub fn live_current(&self) -> Vec<u64> {
        self.live_cur.indices()
    }
    pub fn live_previous(&self) -> Vec<u64> {
        self.live_prev.indices()
    }
    pub fn has_staged_changes(&self) -> bool {
        self.dirty
    }
    pub fn device(&self) -> &D {
        &self.dev
    }
    pub fn device_mut(&mut self) -> &mut D {
        &mut self.dev
    }
    pub fn into_device(self) -> D {
        self.dev
    }

    fn txn_gen(&self) -> u64 {
        self.sb.gen + 1
    }

    // ── Allocation (doc 05 §4) ───────────────────────────────────────────────

    fn alloc_block(&mut self) -> Result<u64> {
        let total = self.header.total_blocks;
        let span = total - FIRST_DATA_BLOCK;
        for i in 0..span {
            let b = FIRST_DATA_BLOCK + (self.alloc_cursor + i) % span;
            if !self.live_cur.get(b) && !self.live_prev.get(b) && !self.fresh.contains(&b) {
                // SPACE-3: never reachable from either valid superblock.
                debug_assert!(!self.live_cur.get(b) && !self.live_prev.get(b));
                self.alloc_cursor = (self.alloc_cursor + i + 1) % span;
                self.fresh.insert(b);
                self.op_fresh.push(b);
                return Ok(b);
            }
        }
        Err(Error::NoSpace)
    }

    /// Seal `buf` through the transform and write it to a fresh block in the
    /// pending generation. `buf` holds ciphertext afterwards.
    fn write_fresh(&mut self, buf: &mut [u8]) -> Result<BlockPtr> {
        let b = self.alloc_block()?;
        let g = self.txn_gen();
        let aad = dyn_aad(b, g);
        let tag = self.xform.encrypt_block(b, g, &aad, buf);
        self.dev.write_block(b, buf)?;
        self.dirty = true;
        Ok(BlockPtr { block: b, gen: g, tag })
    }

    /// A block is no longer referenced by the pending generation.
    fn retire(&mut self, ptr: &BlockPtr) {
        if ptr.is_null() {
            return;
        }
        if self.fresh.contains(&ptr.block) {
            self.dropped_fresh.insert(ptr.block);
        } else {
            self.retired.insert(ptr.block);
        }
    }

    fn read_ptr(&mut self, ptr: &BlockPtr) -> Result<Vec<u8>> {
        space::read_ptr(&mut self.dev, &self.xform, ptr)
    }

    // ── Commit: the pointer flip (doc 03 §4, doc 04 §4–5) ────────────────────

    /// Make all staged modifications durable and atomically visible. No-op if
    /// nothing is staged.
    pub fn commit(&mut self) -> Result<()> {
        if !self.dirty {
            return Ok(());
        }
        if self.sb.gen >= GEN_LIMIT {
            return Err(Error::GenExhausted);
        }
        // Step 2: barrier — every staged tree block durable before the
        // superblock that references it (COW-2 held per-write; COW-3 here).
        self.dev.flush()?;

        // Steps 3–4: new superblock, one generation higher, into the slot
        // that is NOT current (SB-1).
        let g = self.txn_gen();
        let target = if self.cur_slot == SB_SLOT_A { SB_SLOT_B } else { SB_SLOT_A };
        let mut live_new = self.live_cur.clone();
        for &b in &self.retired {
            live_new.clear(b);
        }
        for &b in &self.fresh {
            if !self.dropped_fresh.contains(&b) {
                live_new.set(b);
            }
        }
        let sb = Superblock {
            gen: g,
            total_blocks: self.header.total_blocks,
            inode_map_root: self.staged_root,
            inode_map_height: self.staged_height,
            next_inode: self.staged_next_inode,
            inode_count: self.staged_inode_count,
            block_count: live_new.count(),
            commit_time: (self.clock)(),
            uuid: self.header.uuid,
        };
        superblock::write_slot(&mut self.dev, &self.xform, target, &sb)?;
        // Step 5: barrier — the flip is durable.
        self.dev.flush()?;

        // Rotate the live trees: generation K becomes the fallback; the
        // generation that was in the overwritten slot is abandoned, so its
        // uniquely-owned blocks become free (doc 05 §5).
        self.live_prev = core::mem::replace(&mut self.live_cur, live_new);
        self.sb = sb;
        self.cur_slot = target;
        self.fresh.clear();
        self.dropped_fresh.clear();
        self.retired.clear();
        self.op_fresh.clear();
        self.dirty = false;
        Ok(())
    }

    // ── Per-operation rollback ───────────────────────────────────────────────

    fn begin_op(&mut self) -> OpSnapshot {
        self.op_fresh.clear();
        OpSnapshot {
            root: self.staged_root,
            height: self.staged_height,
            next_inode: self.staged_next_inode,
            inode_count: self.staged_inode_count,
            dirty: self.dirty,
            retired: self.retired.clone(),
        }
    }

    /// Undo a failed operation's staged effects. Its blocks stay burned
    /// (`dropped_fresh`) until the next commit so their `(block, gen)` nonces
    /// are never reissued within the same generation.
    fn fail_op(&mut self, snap: OpSnapshot) {
        self.staged_root = snap.root;
        self.staged_height = snap.height;
        self.staged_next_inode = snap.next_inode;
        self.staged_inode_count = snap.inode_count;
        self.dirty = snap.dirty;
        self.retired = snap.retired;
        for b in core::mem::take(&mut self.op_fresh) {
            self.dropped_fresh.insert(b);
        }
    }

    fn check_rw(&self) -> Result<()> {
        if self.read_only {
            return Err(Error::ReadOnly);
        }
        // Generation freeze (doc 03 §3): never reset, never wrap — the next
        // commit's gen must stay below GEN_LIMIT.
        if self.sb.gen >= GEN_LIMIT {
            return Err(Error::GenExhausted);
        }
        Ok(())
    }

    // ── Inode map (doc 06 §3) ────────────────────────────────────────────────

    fn imap_get(&mut self, ino: u64) -> Result<Option<Inode>> {
        if ino >= imap_capacity(self.staged_height) {
            return Ok(None);
        }
        let mut ptr = self.staged_root;
        let mut level = self.staged_height;
        while level > 0 {
            if ptr.is_null() {
                return Ok(None);
            }
            let buf = self.read_ptr(&ptr)?;
            ptr = ptr_at(&buf, imap_index(ino, level));
            level -= 1;
        }
        if ptr.is_null() {
            return Ok(None);
        }
        let buf = self.read_ptr(&ptr)?;
        let slot = (ino as usize & (INODES_PER_LEAF - 1)) * INODE_SIZE;
        let inode = Inode::decode(&buf[slot..slot + INODE_SIZE]);
        Ok(if inode.is_used() { Some(inode) } else { None })
    }

    fn get_inode_req(&mut self, ino: u64) -> Result<Inode> {
        self.imap_get(ino)?.ok_or(Error::NotFound)
    }

    /// Grow the inode map until `ino` is addressable: new root index node,
    /// old root as child 0 (doc 06 §3 "Growth").
    fn grow_imap(&mut self, ino: u64) -> Result<()> {
        while ino >= imap_capacity(self.staged_height) {
            let mut node = vec![0u8; BLOCK_SIZE];
            set_ptr_at(&mut node, 0, &self.staged_root);
            self.staged_root = self.write_fresh(&mut node)?;
            self.staged_height += 1;
        }
        Ok(())
    }

    /// COW-write inode `ino` (128 bytes; `Inode::FREE` clears the slot),
    /// rewriting the leaf and every index node up to a new root (doc 04 §2
    /// step 5).
    fn imap_set(&mut self, ino: u64, inode: &Inode) -> Result<()> {
        self.grow_imap(ino)?;
        let root = self.staged_root;
        let height = self.staged_height;
        let bytes = inode.encode();
        self.staged_root = self.imap_set_rec(root, height, ino, &bytes)?;
        Ok(())
    }

    fn imap_set_rec(
        &mut self,
        ptr: BlockPtr,
        level: u64,
        ino: u64,
        bytes: &[u8; INODE_SIZE],
    ) -> Result<BlockPtr> {
        let mut buf = if ptr.is_null() { vec![0u8; BLOCK_SIZE] } else { self.read_ptr(&ptr)? };
        self.retire(&ptr);
        if level == 0 {
            let slot = (ino as usize & (INODES_PER_LEAF - 1)) * INODE_SIZE;
            buf[slot..slot + INODE_SIZE].copy_from_slice(bytes);
        } else {
            let idx = imap_index(ino, level);
            let child = ptr_at(&buf, idx);
            let new_child = self.imap_set_rec(child, level - 1, ino, bytes)?;
            set_ptr_at(&mut buf, idx, &new_child);
        }
        self.write_fresh(&mut buf)
    }

    fn alloc_ino(&mut self) -> u64 {
        // Bump-only, no slot reuse (doc 06 §3, resolved): inode numbers are
        // unique over the volume lifetime. The max() guards the permanently
        // reserved band 2–9 against any legacy counter value.
        let ino = self.staged_next_inode.max(FIRST_GENERAL_INO);
        self.staged_next_inode = ino + 1;
        ino
    }

    /// Free an inode now: drop its block map (fast symlinks have none),
    /// clear the slot to all-zero (doc 06 §5).
    fn free_inode(&mut self, ino: u64, inode: &Inode) -> Result<()> {
        if !inode.is_fast_symlink() {
            let root = inode.bmap_root;
            let h = inode.bmap_height as u64;
            self.retire_bmap(root, h)?;
        }
        self.imap_set(ino, &Inode::FREE)?;
        self.staged_inode_count -= 1;
        Ok(())
    }

    /// Drop one directory reference to `inode` (unlink, or rename-replace).
    /// Frees at `nlink == 0` unless an open handle pins it, in which case the
    /// inode is written back as an on-disk orphan (doc 06 §5).
    fn drop_link(&mut self, ino: u64, mut inode: Inode, now: u64) -> Result<()> {
        inode.nlink -= 1;
        if inode.nlink == 0 && !self.pins.contains_key(&ino) {
            self.free_inode(ino, &inode)
        } else {
            inode.ctime = now;
            inode.inode_gen = self.txn_gen();
            self.imap_set(ino, &inode)
        }
    }

    // ── Per-file block map (doc 06 §4) ───────────────────────────────────────

    fn bmap_get(&mut self, inode: &Inode, l: u64) -> Result<BlockPtr> {
        let h = inode.bmap_height as u64;
        if inode.bmap_root.is_null() || l >= bmap_capacity(h) {
            return Ok(BlockPtr::NULL);
        }
        if h == 0 {
            return Ok(inode.bmap_root); // l == 0 by the capacity check
        }
        let mut ptr = inode.bmap_root;
        for level in (1..=h).rev() {
            if ptr.is_null() {
                return Ok(BlockPtr::NULL); // sparse hole
            }
            let buf = self.read_ptr(&ptr)?;
            ptr = ptr_at(&buf, bmap_index(l, level));
        }
        Ok(ptr)
    }

    /// Splice `new` (possibly NULL, for truncate) at logical block `l`,
    /// rewriting the spine COW (doc 04 §2 steps 2–3). Retires the replaced
    /// data pointer and every replaced spine node.
    fn bmap_set(&mut self, inode: &mut Inode, l: u64, new: BlockPtr) -> Result<()> {
        while l >= bmap_capacity(inode.bmap_height as u64) {
            // Add a level: fresh root index node, old root as child 0.
            let mut node = vec![0u8; BLOCK_SIZE];
            set_ptr_at(&mut node, 0, &inode.bmap_root);
            inode.bmap_root = self.write_fresh(&mut node)?;
            inode.bmap_height += 1;
        }
        if inode.bmap_height == 0 {
            let old = inode.bmap_root;
            self.retire(&old);
            inode.bmap_root = new;
            return Ok(());
        }
        let root = inode.bmap_root;
        let height = inode.bmap_height as u64;
        inode.bmap_root = self.bmap_set_rec(root, height, l, new)?;
        Ok(())
    }

    fn bmap_set_rec(&mut self, ptr: BlockPtr, level: u64, l: u64, new: BlockPtr) -> Result<BlockPtr> {
        let mut buf = if ptr.is_null() { vec![0u8; BLOCK_SIZE] } else { self.read_ptr(&ptr)? };
        self.retire(&ptr);
        let idx = bmap_index(l, level);
        if level == 1 {
            let old = ptr_at(&buf, idx);
            self.retire(&old);
            set_ptr_at(&mut buf, idx, &new);
        } else {
            let child = ptr_at(&buf, idx);
            let new_child = self.bmap_set_rec(child, level - 1, l, new)?;
            set_ptr_at(&mut buf, idx, &new_child);
        }
        self.write_fresh(&mut buf)
    }

    /// Retire an entire block-map subtree (unlink/rmdir/truncate-to-zero).
    fn retire_bmap(&mut self, ptr: BlockPtr, height: u64) -> Result<()> {
        if ptr.is_null() {
            return Ok(());
        }
        if height > 0 {
            let buf = self.read_ptr(&ptr)?;
            for i in 0..FANOUT {
                let child = ptr_at(&buf, i);
                if !child.is_null() {
                    self.retire_bmap(child, height - 1)?;
                }
            }
        }
        self.retire(&ptr);
        Ok(())
    }

    // ── File I/O ─────────────────────────────────────────────────────────────

    pub fn read_at(&mut self, ino: u64, off: u64, out: &mut [u8]) -> Result<usize> {
        let inode = self.get_inode_req(ino)?;
        if inode.is_dir() {
            return Err(Error::IsDir);
        }
        if !inode.is_reg() {
            return Err(Error::Invalid); // symlinks: use readlink
        }
        self.read_blocks(&inode, off, out)
    }

    fn read_blocks(&mut self, inode: &Inode, off: u64, out: &mut [u8]) -> Result<usize> {
        if off >= inode.size {
            return Ok(0);
        }
        let end = inode.size.min(off + out.len() as u64);
        let mut pos = off;
        while pos < end {
            let l = pos / BLOCK_SIZE as u64;
            let boff = (pos % BLOCK_SIZE as u64) as usize;
            let n = ((BLOCK_SIZE - boff) as u64).min(end - pos) as usize;
            let dst = &mut out[(pos - off) as usize..(pos - off) as usize + n];
            let ptr = self.bmap_get(inode, l)?;
            if ptr.is_null() {
                dst.fill(0); // sparse hole reads as zeros (INODE-2)
            } else {
                let buf = self.read_ptr(&ptr)?;
                dst.copy_from_slice(&buf[boff..boff + n]);
            }
            pos += n as u64;
        }
        Ok((end - off) as usize)
    }

    pub fn write_at(&mut self, ino: u64, off: u64, data: &[u8]) -> Result<()> {
        self.check_rw()?;
        let snap = self.begin_op();
        match self.write_at_inner(ino, off, data) {
            Ok(()) => {
                self.op_fresh.clear();
                Ok(())
            }
            Err(e) => {
                self.fail_op(snap);
                Err(e)
            }
        }
    }

    fn write_at_inner(&mut self, ino: u64, off: u64, data: &[u8]) -> Result<()> {
        let mut inode = self.get_inode_req(ino)?;
        if inode.is_dir() {
            return Err(Error::IsDir);
        }
        if !inode.is_reg() {
            return Err(Error::Invalid);
        }
        self.write_blocks(&mut inode, off, data)?;
        let now = (self.clock)();
        inode.size = inode.size.max(off + data.len() as u64);
        inode.mtime = now;
        inode.ctime = now;
        inode.inode_gen = self.txn_gen();
        self.imap_set(ino, &inode)
    }

    /// The COW write path for one file (doc 04 §2 steps 1–3): materialize each
    /// affected block (read-modify for partial writes, skip the read for full
    /// overwrites), write it fresh, splice via the block-map spine.
    fn write_blocks(&mut self, inode: &mut Inode, off: u64, data: &[u8]) -> Result<()> {
        let mut pos = 0usize;
        while pos < data.len() {
            let l = (off + pos as u64) / BLOCK_SIZE as u64;
            let boff = ((off + pos as u64) % BLOCK_SIZE as u64) as usize;
            let n = (BLOCK_SIZE - boff).min(data.len() - pos);
            let old = self.bmap_get(inode, l)?;
            let mut buf = if n < BLOCK_SIZE && !old.is_null() {
                self.read_ptr(&old)?
            } else {
                vec![0u8; BLOCK_SIZE]
            };
            buf[boff..boff + n].copy_from_slice(&data[pos..pos + n]);
            let ptr = self.write_fresh(&mut buf)?;
            if old.is_null() {
                inode.blocks += 1;
            }
            self.bmap_set(inode, l, ptr)?; // retires `old` and the old spine
            pos += n;
        }
        Ok(())
    }

    pub fn truncate(&mut self, ino: u64, new_size: u64) -> Result<()> {
        self.check_rw()?;
        let snap = self.begin_op();
        match self.truncate_inner(ino, new_size) {
            Ok(()) => {
                self.op_fresh.clear();
                Ok(())
            }
            Err(e) => {
                self.fail_op(snap);
                Err(e)
            }
        }
    }

    fn truncate_inner(&mut self, ino: u64, new_size: u64) -> Result<()> {
        let mut inode = self.get_inode_req(ino)?;
        if inode.is_dir() {
            return Err(Error::IsDir);
        }
        if !inode.is_reg() {
            return Err(Error::Invalid);
        }
        if new_size < inode.size {
            let keep = new_size.div_ceil(BLOCK_SIZE as u64);
            let span = inode.size.div_ceil(BLOCK_SIZE as u64);
            if new_size == 0 {
                let root = inode.bmap_root;
                let h = inode.bmap_height as u64;
                self.retire_bmap(root, h)?;
                inode.bmap_root = BlockPtr::NULL;
                inode.bmap_height = 0;
                inode.blocks = 0;
            } else {
                for l in keep..span {
                    let old = self.bmap_get(&inode, l)?;
                    if !old.is_null() {
                        self.bmap_set(&mut inode, l, BlockPtr::NULL)?;
                        inode.blocks -= 1;
                    }
                }
                // Zero the cut tail of the (partial) last kept block so a
                // later size grow exposes zeros, not stale bytes — the last
                // block is zero-padded past `size` (doc 06 §4).
                let tail = (new_size % BLOCK_SIZE as u64) as usize;
                if tail != 0 {
                    let l = new_size / BLOCK_SIZE as u64;
                    let old = self.bmap_get(&inode, l)?;
                    if !old.is_null() {
                        let mut buf = self.read_ptr(&old)?;
                        buf[tail..].fill(0);
                        let ptr = self.write_fresh(&mut buf)?;
                        self.bmap_set(&mut inode, l, ptr)?;
                    }
                }
            }
        }
        // Growth is sparse: the new tail reads as zeros (doc 04 §2).
        inode.size = new_size;
        let now = (self.clock)();
        inode.mtime = now;
        inode.ctime = now;
        inode.inode_gen = self.txn_gen();
        self.imap_set(ino, &inode)
    }

    // ── Directories (doc 07) ─────────────────────────────────────────────────

    fn dir_lookup(&mut self, dinode: &Inode, name: &[u8]) -> Result<Option<(u64, u8)>> {
        let nblocks = dinode.size / BLOCK_SIZE as u64;
        for l in 0..nblocks {
            let ptr = self.bmap_get(dinode, l)?;
            if ptr.is_null() {
                return Err(Error::Corrupt); // directory blocks are never sparse
            }
            let buf = self.read_ptr(&ptr)?;
            if let Some((_, ino, ft)) = dirent::find(&buf, name)? {
                return Ok(Some((ino, ft)));
            }
        }
        Ok(None)
    }

    /// Rewrite (COW) the directory data block at logical `l` with `buf`.
    fn dir_write_block(&mut self, dinode: &mut Inode, l: u64, buf: &mut [u8]) -> Result<()> {
        let ptr = self.write_fresh(buf)?;
        self.bmap_set(dinode, l, ptr)
    }

    /// Add a dirent (doc 07 §3 create steps 3–4): first hole/slack that fits,
    /// else append a new block. Mutates `dinode` (block map, size, times);
    /// the caller writes it back via `imap_set`.
    fn dir_add_entry(
        &mut self,
        dinode: &mut Inode,
        name: &[u8],
        ino: u64,
        ftype: u8,
    ) -> Result<()> {
        let nblocks = dinode.size / BLOCK_SIZE as u64;
        for l in 0..nblocks {
            let ptr = self.bmap_get(dinode, l)?;
            if ptr.is_null() {
                return Err(Error::Corrupt);
            }
            let mut buf = self.read_ptr(&ptr)?;
            if dirent::insert(&mut buf, ino, ftype, name)? {
                return self.dir_write_block(dinode, l, &mut buf);
            }
        }
        // All blocks full: grow the directory by one block.
        let mut buf = vec![0u8; BLOCK_SIZE];
        dirent::init_empty(&mut buf);
        let ok = dirent::insert(&mut buf, ino, ftype, name)?;
        debug_assert!(ok);
        self.dir_write_block(dinode, nblocks, &mut buf)?;
        dinode.size += BLOCK_SIZE as u64;
        dinode.blocks += 1;
        Ok(())
    }

    /// Turn the named dirent into a hole (doc 07 §3 unlink step 2).
    fn dir_remove_entry(&mut self, dinode: &mut Inode, name: &[u8]) -> Result<()> {
        let nblocks = dinode.size / BLOCK_SIZE as u64;
        for l in 0..nblocks {
            let ptr = self.bmap_get(dinode, l)?;
            if ptr.is_null() {
                return Err(Error::Corrupt);
            }
            let mut buf = self.read_ptr(&ptr)?;
            if dirent::remove(&mut buf, name)? {
                return self.dir_write_block(dinode, l, &mut buf);
            }
        }
        Err(Error::NotFound)
    }

    // ── Path resolution ──────────────────────────────────────────────────────

    pub fn lookup(&mut self, path: &str) -> Result<u64> {
        if !path.starts_with('/') {
            return Err(Error::Invalid);
        }
        let mut ino = ROOT_INO;
        for comp in path.split('/').filter(|c| !c.is_empty()) {
            let inode = self.get_inode_req(ino)?;
            if !inode.is_dir() {
                return Err(Error::NotDir);
            }
            match self.dir_lookup(&inode, comp.as_bytes())? {
                Some((child, _)) => ino = child,
                None => return Err(Error::NotFound),
            }
        }
        Ok(ino)
    }

    pub fn stat(&mut self, path: &str) -> Result<Inode> {
        let ino = self.lookup(path)?;
        self.get_inode_req(ino)
    }

    pub fn readdir(&mut self, path: &str) -> Result<Vec<DirEntryOut>> {
        let ino = self.lookup(path)?;
        let dinode = self.get_inode_req(ino)?;
        if !dinode.is_dir() {
            return Err(Error::NotDir);
        }
        let mut out = Vec::new();
        let nblocks = dinode.size / BLOCK_SIZE as u64;
        for l in 0..nblocks {
            let ptr = self.bmap_get(&dinode, l)?;
            if ptr.is_null() {
                return Err(Error::Corrupt);
            }
            let buf = self.read_ptr(&ptr)?;
            for d in dirent::parse_block(&buf)? {
                if d.inode != 0 {
                    out.push(DirEntryOut {
                        ino: d.inode,
                        file_type: d.file_type,
                        name: String::from_utf8(d.name).map_err(|_| Error::Corrupt)?,
                    });
                }
            }
        }
        Ok(out)
    }

    fn split_parent(path: &str) -> Result<(&str, &str)> {
        if !path.starts_with('/') || path.ends_with('/') {
            return Err(Error::Invalid);
        }
        let idx = path.rfind('/').unwrap();
        let (parent, name) = (&path[..idx], &path[idx + 1..]);
        if name.is_empty() || name.len() > dirent::NAME_MAX || name == "." || name == ".." {
            return Err(Error::Invalid);
        }
        Ok((if parent.is_empty() { "/" } else { parent }, name))
    }

    // ── create / mkdir / unlink / rmdir (doc 07 §3, doc 06 §5) ───────────────

    pub fn create(&mut self, path: &str) -> Result<u64> {
        self.check_rw()?;
        let snap = self.begin_op();
        match self.create_inner(path, false) {
            Ok(v) => {
                self.op_fresh.clear();
                Ok(v)
            }
            Err(e) => {
                self.fail_op(snap);
                Err(e)
            }
        }
    }

    pub fn mkdir(&mut self, path: &str) -> Result<u64> {
        self.check_rw()?;
        let snap = self.begin_op();
        match self.create_inner(path, true) {
            Ok(v) => {
                self.op_fresh.clear();
                Ok(v)
            }
            Err(e) => {
                self.fail_op(snap);
                Err(e)
            }
        }
    }

    fn create_inner(&mut self, path: &str, dir: bool) -> Result<u64> {
        let (ppath, name) = Self::split_parent(path)?;
        let parent = self.lookup(ppath)?;
        let mut pinode = self.get_inode_req(parent)?;
        if !pinode.is_dir() {
            return Err(Error::NotDir);
        }
        if self.dir_lookup(&pinode, name.as_bytes())?.is_some() {
            return Err(Error::Exists);
        }

        let ino = self.alloc_ino();
        let now = (self.clock)();
        let g = self.txn_gen();
        let mut child = Inode {
            flags: FLAG_USED,
            mtime: now,
            ctime: now,
            atime: now,
            btime: now,
            inode_gen: g,
            ..Inode::FREE
        };
        if dir {
            // Child block 0: `.` and `..` (doc 07 §1).
            let mut buf = vec![0u8; BLOCK_SIZE];
            dirent::init_empty(&mut buf);
            dirent::insert(&mut buf, ino, dirent::FT_DIR, b".")?;
            dirent::insert(&mut buf, parent, dirent::FT_DIR, b"..")?;
            child.mode = S_IFDIR | 0o755;
            child.nlink = 2;
            child.size = BLOCK_SIZE as u64;
            child.blocks = 1;
            child.bmap_root = self.write_fresh(&mut buf)?;
        } else {
            child.mode = S_IFREG | 0o644;
            child.nlink = 1;
        }
        self.imap_set(ino, &child)?;

        let ftype = if dir { dirent::FT_DIR } else { dirent::FT_REG };
        self.dir_add_entry(&mut pinode, name.as_bytes(), ino, ftype)?;
        if dir {
            pinode.nlink += 1; // the child's `..`
        }
        pinode.mtime = now;
        pinode.ctime = now;
        pinode.inode_gen = g;
        self.imap_set(parent, &pinode)?;
        self.staged_inode_count += 1;
        Ok(ino)
    }

    pub fn unlink(&mut self, path: &str) -> Result<()> {
        self.check_rw()?;
        let snap = self.begin_op();
        match self.unlink_inner(path) {
            Ok(()) => {
                self.op_fresh.clear();
                Ok(())
            }
            Err(e) => {
                self.fail_op(snap);
                Err(e)
            }
        }
    }

    fn unlink_inner(&mut self, path: &str) -> Result<()> {
        let (ppath, name) = Self::split_parent(path)?;
        let parent = self.lookup(ppath)?;
        let mut pinode = self.get_inode_req(parent)?;
        let (cino, _) = self.dir_lookup(&pinode, name.as_bytes())?.ok_or(Error::NotFound)?;
        let cinode = self.get_inode_req(cino)?;
        if cinode.is_dir() {
            return Err(Error::IsDir);
        }

        self.dir_remove_entry(&mut pinode, name.as_bytes())?;
        let now = (self.clock)();
        pinode.mtime = now;
        pinode.ctime = now;
        self.imap_set(parent, &pinode)?;

        // Freed at nlink 0 unless pinned by an open handle (doc 06 §5).
        self.drop_link(cino, cinode, now)
    }

    // ── fsync / open-handle pins ─────────────────────────────────────────────

    /// Per-file fsync = full commit (doc 04 §3, resolved): on return, every
    /// staged modification — to `ino` *and* every other file — is durable as
    /// one generation. Strictly stronger than POSIX asks; never weaker.
    pub fn fsync(&mut self, ino: u64) -> Result<()> {
        self.get_inode_req(ino)?;
        self.commit()
    }

    /// Register an open handle on `ino` (the kernel VFS's open table drives
    /// this). While pinned, `nlink == 0` does not free the inode.
    pub fn pin(&mut self, ino: u64) -> Result<()> {
        self.get_inode_req(ino)?;
        *self.pins.entry(ino).or_insert(0) += 1;
        Ok(())
    }

    /// Release an open handle. Dropping the last pin of an orphaned inode
    /// (`nlink == 0`) stages its freeing (doc 06 §5); commit as usual. On a
    /// read-only mount the orphan stays for the next rw mount to reclaim.
    pub fn unpin(&mut self, ino: u64) -> Result<()> {
        let count = self.pins.get_mut(&ino).ok_or(Error::Invalid)?;
        *count -= 1;
        if *count > 0 {
            return Ok(());
        }
        self.pins.remove(&ino);
        if self.read_only || self.sb.gen >= GEN_LIMIT {
            return Ok(());
        }
        if let Some(inode) = self.imap_get(ino)? {
            if inode.nlink == 0 {
                let snap = self.begin_op();
                match self.free_inode(ino, &inode) {
                    Ok(()) => self.op_fresh.clear(),
                    Err(e) => {
                        self.fail_op(snap);
                        return Err(e);
                    }
                }
            }
        }
        Ok(())
    }

    pub fn rmdir(&mut self, path: &str) -> Result<()> {
        self.check_rw()?;
        let snap = self.begin_op();
        match self.rmdir_inner(path) {
            Ok(()) => {
                self.op_fresh.clear();
                Ok(())
            }
            Err(e) => {
                self.fail_op(snap);
                Err(e)
            }
        }
    }

    fn rmdir_inner(&mut self, path: &str) -> Result<()> {
        let (ppath, name) = Self::split_parent(path)?;
        let parent = self.lookup(ppath)?;
        let mut pinode = self.get_inode_req(parent)?;
        let (cino, _) = self.dir_lookup(&pinode, name.as_bytes())?.ok_or(Error::NotFound)?;
        if cino == ROOT_INO {
            return Err(Error::Invalid);
        }
        let cinode = self.get_inode_req(cino)?;
        if !cinode.is_dir() {
            return Err(Error::NotDir);
        }
        // Empty = only `.` and `..` (doc 07 §3; ENOTEMPTY errno value is
        // TODO(open) in the syscall spec — surfaced here as Error::NotEmpty).
        let nblocks = cinode.size / BLOCK_SIZE as u64;
        for l in 0..nblocks {
            let ptr = self.bmap_get(&cinode, l)?;
            if ptr.is_null() {
                return Err(Error::Corrupt);
            }
            let buf = self.read_ptr(&ptr)?;
            if !dirent::only_dots(&buf)? {
                return Err(Error::NotEmpty);
            }
        }

        self.dir_remove_entry(&mut pinode, name.as_bytes())?;
        let now = (self.clock)();
        pinode.nlink -= 1; // the child's `..`
        pinode.mtime = now;
        pinode.ctime = now;
        self.imap_set(parent, &pinode)?;

        let root = cinode.bmap_root;
        let h = cinode.bmap_height as u64;
        self.retire_bmap(root, h)?;
        self.imap_set(cino, &Inode::FREE)?;
        self.staged_inode_count -= 1;
        Ok(())
    }

    // ── link / symlink / readlink (doc 07 §3, doc 06 §1) ─────────────────────

    /// Hard link: a new dirent for an existing non-directory inode.
    pub fn link(&mut self, existing: &str, newpath: &str) -> Result<()> {
        self.check_rw()?;
        let snap = self.begin_op();
        match self.link_inner(existing, newpath) {
            Ok(()) => {
                self.op_fresh.clear();
                Ok(())
            }
            Err(e) => {
                self.fail_op(snap);
                Err(e)
            }
        }
    }

    fn link_inner(&mut self, existing: &str, newpath: &str) -> Result<()> {
        let src_ino = self.lookup(existing)?;
        let mut src = self.get_inode_req(src_ino)?;
        if src.is_dir() {
            // Directory hard links would create cycles the `..`-based loop
            // check and nlink accounting cannot represent (doc 07 §3).
            return Err(Error::Perm);
        }
        let (ppath, name) = Self::split_parent(newpath)?;
        let parent = self.lookup(ppath)?;
        let mut pinode = self.get_inode_req(parent)?;
        if !pinode.is_dir() {
            return Err(Error::NotDir);
        }
        if self.dir_lookup(&pinode, name.as_bytes())?.is_some() {
            return Err(Error::Exists);
        }
        let ftype = if src.is_symlink() { dirent::FT_SYMLINK } else { dirent::FT_REG };
        self.dir_add_entry(&mut pinode, name.as_bytes(), src_ino, ftype)?;
        let now = (self.clock)();
        pinode.mtime = now;
        pinode.ctime = now;
        self.imap_set(parent, &pinode)?;
        src.nlink += 1;
        src.ctime = now;
        src.inode_gen = self.txn_gen();
        self.imap_set(src_ino, &src)
    }

    /// Create a symlink. Targets ≤ 48 bytes are stored inline in the inode
    /// (fast symlink); longer targets spill to file data (doc 06 §1).
    pub fn symlink(&mut self, target: &str, linkpath: &str) -> Result<u64> {
        self.check_rw()?;
        let snap = self.begin_op();
        match self.symlink_inner(target, linkpath) {
            Ok(v) => {
                self.op_fresh.clear();
                Ok(v)
            }
            Err(e) => {
                self.fail_op(snap);
                Err(e)
            }
        }
    }

    fn symlink_inner(&mut self, target: &str, linkpath: &str) -> Result<u64> {
        if target.is_empty() {
            return Err(Error::Invalid);
        }
        let (ppath, name) = Self::split_parent(linkpath)?;
        let parent = self.lookup(ppath)?;
        let mut pinode = self.get_inode_req(parent)?;
        if !pinode.is_dir() {
            return Err(Error::NotDir);
        }
        if self.dir_lookup(&pinode, name.as_bytes())?.is_some() {
            return Err(Error::Exists);
        }

        let ino = self.alloc_ino();
        let now = (self.clock)();
        let tbytes = target.as_bytes();
        let mut child = Inode {
            mode: S_IFLNK | 0o777,
            flags: FLAG_USED,
            nlink: 1,
            size: tbytes.len() as u64,
            mtime: now,
            ctime: now,
            atime: now,
            btime: now,
            inode_gen: self.txn_gen(),
            ..Inode::FREE
        };
        if tbytes.len() <= INLINE_SYMLINK_MAX {
            child.flags |= FLAG_FAST_SYMLINK;
            child.inline[..tbytes.len()].copy_from_slice(tbytes);
        } else {
            self.write_blocks(&mut child, 0, tbytes)?;
        }
        self.imap_set(ino, &child)?;

        self.dir_add_entry(&mut pinode, name.as_bytes(), ino, dirent::FT_SYMLINK)?;
        pinode.mtime = now;
        pinode.ctime = now;
        self.imap_set(parent, &pinode)?;
        self.staged_inode_count += 1;
        Ok(ino)
    }

    /// Read a symlink's target. Never resolves — path traversal does not
    /// follow symlinks in this layer (doc 07 §3).
    pub fn readlink(&mut self, path: &str) -> Result<String> {
        let ino = self.lookup(path)?;
        let inode = self.get_inode_req(ino)?;
        if !inode.is_symlink() {
            return Err(Error::Invalid);
        }
        let len = inode.size as usize;
        let mut buf = vec![0u8; len];
        if inode.is_fast_symlink() {
            buf.copy_from_slice(&inode.inline[..len]);
        } else {
            let n = self.read_blocks(&inode, 0, &mut buf)?;
            if n != len {
                return Err(Error::Corrupt);
            }
        }
        String::from_utf8(buf).map_err(|_| Error::Corrupt)
    }

    // ── rename (doc 07 §3, resolved) ─────────────────────────────────────────

    /// Atomic rename: every effect (dirents, `..`, nlink fixups, freed
    /// replaced target) is staged in this one transaction and becomes visible
    /// at a single superblock commit.
    pub fn rename(&mut self, old: &str, new: &str) -> Result<()> {
        self.check_rw()?;
        let snap = self.begin_op();
        match self.rename_inner(old, new) {
            Ok(()) => {
                self.op_fresh.clear();
                Ok(())
            }
            Err(e) => {
                self.fail_op(snap);
                Err(e)
            }
        }
    }

    /// The inode `dir`'s parent, via its `..` entry.
    fn dotdot_of(&mut self, dir: u64) -> Result<u64> {
        let dinode = self.get_inode_req(dir)?;
        match self.dir_lookup(&dinode, b"..")? {
            Some((ino, _)) => Ok(ino),
            None => Err(Error::Corrupt),
        }
    }

    fn rename_inner(&mut self, old: &str, new: &str) -> Result<()> {
        let (oppath, oname) = Self::split_parent(old)?;
        let (nppath, nname) = Self::split_parent(new)?;
        let op_ino = self.lookup(oppath)?;
        let np_ino = self.lookup(nppath)?;
        let opinode = self.get_inode_req(op_ino)?;
        if !opinode.is_dir() {
            return Err(Error::NotDir);
        }
        let (src_ino, src_ft) =
            self.dir_lookup(&opinode, oname.as_bytes())?.ok_or(Error::NotFound)?;
        // Same directory entry → no-op (POSIX).
        if op_ino == np_ino && oname == nname {
            return Ok(());
        }
        let mut src_inode = self.get_inode_req(src_ino)?;

        let npinode = if np_ino == op_ino { opinode } else { self.get_inode_req(np_ino)? };
        if !npinode.is_dir() {
            return Err(Error::NotDir);
        }

        // Existing target checks (doc 07 §3 rename step 4).
        let target = self.dir_lookup(&npinode, nname.as_bytes())?;
        let target = match target {
            Some((tgt_ino, _)) if tgt_ino == src_ino => return Ok(()), // same inode → no-op
            Some((tgt_ino, _)) => {
                let tgt = self.get_inode_req(tgt_ino)?;
                if src_inode.is_dir() {
                    if !tgt.is_dir() {
                        return Err(Error::NotDir);
                    }
                    self.check_dir_empty(&tgt)?;
                } else if tgt.is_dir() {
                    return Err(Error::IsDir);
                }
                Some((tgt_ino, tgt))
            }
            None => None,
        };

        // Loop check (step 3): a directory may not move under itself.
        if src_inode.is_dir() {
            let mut cur = np_ino;
            loop {
                if cur == src_ino {
                    return Err(Error::Invalid);
                }
                if cur == ROOT_INO {
                    break;
                }
                cur = self.dotdot_of(cur)?;
            }
        }

        let now = (self.clock)();
        let cross = op_ino != np_ino;
        if cross {
            let mut op = self.get_inode_req(op_ino)?;
            let mut np = npinode;
            if let Some((_, tgt)) = &target {
                self.dir_remove_entry(&mut np, nname.as_bytes())?;
                if tgt.is_dir() {
                    np.nlink -= 1; // the replaced dir's `..` on np
                }
            }
            self.dir_remove_entry(&mut op, oname.as_bytes())?;
            self.dir_add_entry(&mut np, nname.as_bytes(), src_ino, src_ft)?;
            if src_inode.is_dir() {
                // Retarget the moved directory's `..` (step 5): COW rewrite
                // of its block 0 through its own spine.
                op.nlink -= 1;
                np.nlink += 1;
                let ptr = self.bmap_get(&src_inode, 0)?;
                if ptr.is_null() {
                    return Err(Error::Corrupt);
                }
                let mut buf = self.read_ptr(&ptr)?;
                if !dirent::retarget(&mut buf, b"..", np_ino)? {
                    return Err(Error::Corrupt);
                }
                self.dir_write_block(&mut src_inode, 0, &mut buf)?;
            }
            op.mtime = now;
            op.ctime = now;
            np.mtime = now;
            np.ctime = now;
            self.imap_set(op_ino, &op)?;
            self.imap_set(np_ino, &np)?;
        } else {
            let mut p = self.get_inode_req(op_ino)?;
            if let Some((_, tgt)) = &target {
                self.dir_remove_entry(&mut p, nname.as_bytes())?;
                if tgt.is_dir() {
                    p.nlink -= 1;
                }
            }
            self.dir_remove_entry(&mut p, oname.as_bytes())?;
            self.dir_add_entry(&mut p, nname.as_bytes(), src_ino, src_ft)?;
            p.mtime = now;
            p.ctime = now;
            self.imap_set(op_ino, &p)?;
        }

        // Free / unlink the replaced target (after the dirent churn so a
        // failed step earlier leaves it untouched by rollback).
        if let Some((tgt_ino, tgt)) = target {
            if tgt.is_dir() {
                self.free_inode(tgt_ino, &tgt)?;
            } else {
                self.drop_link(tgt_ino, tgt, now)?;
            }
        }

        src_inode.ctime = now;
        src_inode.inode_gen = self.txn_gen();
        self.imap_set(src_ino, &src_inode)
    }

    /// `ENOTEMPTY` unless the directory holds only `.` and `..`.
    fn check_dir_empty(&mut self, dinode: &Inode) -> Result<()> {
        let nblocks = dinode.size / BLOCK_SIZE as u64;
        for l in 0..nblocks {
            let ptr = self.bmap_get(dinode, l)?;
            if ptr.is_null() {
                return Err(Error::Corrupt);
            }
            let buf = self.read_ptr(&ptr)?;
            if !dirent::only_dots(&buf)? {
                return Err(Error::NotEmpty);
            }
        }
        Ok(())
    }
}
