//! On-disk layout: block model, static header, `BlockPtr` (doc 02), feature
//! flags (doc 10). Byte-exact per the offset tables; all integers little-endian.

use crate::{Error, Result};

pub const BLOCK_SIZE: usize = 4096;
pub const SECTOR_SIZE: usize = 512;

/// Volume magic, static header offset 0 (doc 02 §3).
pub const MAGIC: [u8; 8] = *b"RFS_V2\0\0";
/// Superblock magic, payload offset 0 (doc 03 §2).
pub const SB_MAGIC: [u8; 8] = *b"RFSSB\0\0\0";

pub const FORMAT_VERSION: u16 = 2;
pub const HEADER_VERSION: u16 = 1;

/// Fixed region (doc 02 §2). Recorded in the header, but V2 fixes the values.
pub const SB_SLOT_A: u64 = 1;
pub const SB_SLOT_B: u64 = 2;
pub const FIRST_DATA_BLOCK: u64 = 3;

/// `BlockPtr`s per index node (doc 06).
pub const FANOUT: usize = 128;
/// Inodes per inode-map leaf block (doc 06 §3).
pub const INODES_PER_LEAF: usize = 32;
pub const INODE_SIZE: usize = 128;
pub const BLOCK_PTR_SIZE: usize = 32;
/// Superblock AEAD payload size; the 24-byte plaintext trailer follows
/// (doc 03 §2): `gen_copy` (u64) then the 16-byte tag.
pub const SB_PAYLOAD_SIZE: usize = 4072;
/// Plaintext copy of the superblock's generation — read *before* decryption
/// to form the nonce/AAD (resolves the former gen-before-decrypt gap).
pub const SB_GEN_COPY_OFFSET: usize = 4072;
pub const SB_TAG_OFFSET: usize = 4080;

/// Generation freeze threshold (doc 03 §3): once the current generation
/// reaches this, all mutation is refused with `GenExhausted`. Never reset —
/// reusing `(block, gen)` under one DEK would break GCM (CRYPTO-1).
pub const GEN_LIMIT: u64 = u64::MAX - 255;

/// Max fast-symlink target length: inode bytes 80..128 (doc 06 §1).
pub const INLINE_SYMLINK_MAX: usize = 48;

pub const ROOT_INO: u64 = 1;
/// Inodes 2–9 reserved for future fixed roles (doc 06 §3); general alloc 10+.
pub const FIRST_GENERAL_INO: u64 = 10;

// Feature flags (doc 10 §2).
pub const INCOMPAT_ENCRYPTION: u64 = 1 << 0;
pub const INCOMPAT_COMPRESSION: u64 = 1 << 1;
// incompat bit 2 retired (was DIR_HASH_INDEX, now ro_compat bit 1); never reuse.
pub const RO_COMPAT_HARDLINKS: u64 = 1 << 0;
/// Hashed dir index: readable via linear scan, not writable without index
/// support → unknown-to-us bit forces a read-only mount (doc 07 §4, 10 §2).
pub const RO_COMPAT_DIR_HASH_INDEX: u64 = 1 << 1;

/// Incompat bits this implementation understands structurally. ENCRYPTION is
/// honored only if the active `BlockTransform` provides it (see `fs::mount`).
pub const KNOWN_INCOMPAT: u64 = INCOMPAT_ENCRYPTION;
/// ro_compat features we fully implement (read-write safe).
pub const KNOWN_RO_COMPAT: u64 = RO_COMPAT_HARDLINKS;

// ── LE helpers ───────────────────────────────────────────────────────────────

pub fn rd_u16(b: &[u8]) -> u16 {
    u16::from_le_bytes([b[0], b[1]])
}
pub fn rd_u32(b: &[u8]) -> u32 {
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}
pub fn rd_u64(b: &[u8]) -> u64 {
    u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]])
}
pub fn wr_u16(b: &mut [u8], v: u16) {
    b[..2].copy_from_slice(&v.to_le_bytes());
}
pub fn wr_u32(b: &mut [u8], v: u32) {
    b[..4].copy_from_slice(&v.to_le_bytes());
}
pub fn wr_u64(b: &mut [u8], v: u64) {
    b[..8].copy_from_slice(&v.to_le_bytes());
}

// ── BlockPtr (doc 02 §4) ─────────────────────────────────────────────────────

/// Self-validating pointer: locates (`block`) and authenticates (`gen` + `tag`)
/// its target. 32 bytes on disk. `block == 0` is the null pointer; readers test
/// only `block`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockPtr {
    pub block: u64,
    pub gen: u64,
    /// AES-256-GCM tag over the target's 4096-byte ciphertext under nonce
    /// `block ‖ gen`. TODO(crypto): zeroed by the identity transform; real
    /// values arrive with doc 08 once the SP 800-38D KAT gate clears.
    pub tag: [u8; 16],
}

impl BlockPtr {
    pub const NULL: BlockPtr = BlockPtr { block: 0, gen: 0, tag: [0; 16] };

    pub fn is_null(&self) -> bool {
        self.block == 0
    }

    pub fn to_bytes(&self) -> [u8; BLOCK_PTR_SIZE] {
        let mut out = [0u8; BLOCK_PTR_SIZE];
        wr_u64(&mut out[0..], self.block);
        wr_u64(&mut out[8..], self.gen);
        out[16..32].copy_from_slice(&self.tag);
        out
    }

    pub fn from_bytes(b: &[u8]) -> BlockPtr {
        let mut tag = [0u8; 16];
        tag.copy_from_slice(&b[16..32]);
        BlockPtr { block: rd_u64(&b[0..]), gen: rd_u64(&b[8..]), tag }
    }
}

// ── Static header (doc 02 §3) ────────────────────────────────────────────────

/// Block 0: plaintext, immutable after mkfs. Geometry, feature masks, KDF
/// parameters, wrapped DEK.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaticHeader {
    pub format_version: u16,
    pub header_version: u16,
    pub block_size: u32,
    pub total_blocks: u64,
    pub sb_slot_a: u64,
    pub sb_slot_b: u64,
    pub first_data_block: u64,
    pub uuid: [u8; 16],
    pub feature_compat: u64,
    pub feature_incompat: u64,
    pub feature_ro_compat: u64,
    pub kdf_algo: u8,
    pub kdf_salt: [u8; 16],
    pub argon_m_cost: u32,
    pub argon_t_cost: u32,
    pub argon_p: u32,
    // TODO(crypto): the three DEK-wrap fields below are zero until doc 08 is
    // implemented (KAT-gated). mkfs then generates a fresh DEK + salt and fills
    // them; mount unwraps via the KEK. AAD binding per doc 08 §7.
    pub dek_wrap_nonce: [u8; 12],
    pub dek_wrapped: [u8; 32],
    pub dek_wrap_tag: [u8; 16],
    pub label: [u8; 64],
}

impl StaticHeader {
    pub fn encode(&self, out: &mut [u8]) {
        assert_eq!(out.len(), BLOCK_SIZE);
        out.fill(0);
        out[0..8].copy_from_slice(&MAGIC);
        wr_u16(&mut out[8..], self.format_version);
        wr_u16(&mut out[10..], self.header_version);
        wr_u32(&mut out[12..], self.block_size);
        wr_u64(&mut out[16..], self.total_blocks);
        wr_u64(&mut out[24..], self.sb_slot_a);
        wr_u64(&mut out[32..], self.sb_slot_b);
        wr_u64(&mut out[40..], self.first_data_block);
        out[48..64].copy_from_slice(&self.uuid);
        wr_u64(&mut out[64..], self.feature_compat);
        wr_u64(&mut out[72..], self.feature_incompat);
        wr_u64(&mut out[80..], self.feature_ro_compat);
        out[88] = self.kdf_algo;
        out[96..112].copy_from_slice(&self.kdf_salt);
        wr_u32(&mut out[112..], self.argon_m_cost);
        wr_u32(&mut out[116..], self.argon_t_cost);
        wr_u32(&mut out[120..], self.argon_p);
        out[128..140].copy_from_slice(&self.dek_wrap_nonce);
        out[144..176].copy_from_slice(&self.dek_wrapped);
        out[176..192].copy_from_slice(&self.dek_wrap_tag);
        out[192..256].copy_from_slice(&self.label);
    }

    pub fn decode(buf: &[u8]) -> Result<StaticHeader> {
        assert_eq!(buf.len(), BLOCK_SIZE);
        if buf[0..8] != MAGIC {
            return Err(Error::BadHeader);
        }
        let format_version = rd_u16(&buf[8..]);
        if format_version != FORMAT_VERSION {
            // Major version is a hard gate (doc 10 §1).
            return Err(Error::Unsupported);
        }
        let header_version = rd_u16(&buf[10..]);
        if header_version < 1 {
            return Err(Error::BadHeader);
        }
        let block_size = rd_u32(&buf[12..]);
        if block_size as usize != BLOCK_SIZE {
            // 4096 is the only defined value in V2 (doc 02 §1).
            return Err(Error::Unsupported);
        }
        let mut uuid = [0u8; 16];
        uuid.copy_from_slice(&buf[48..64]);
        let mut kdf_salt = [0u8; 16];
        kdf_salt.copy_from_slice(&buf[96..112]);
        let mut dek_wrap_nonce = [0u8; 12];
        dek_wrap_nonce.copy_from_slice(&buf[128..140]);
        let mut dek_wrapped = [0u8; 32];
        dek_wrapped.copy_from_slice(&buf[144..176]);
        let mut dek_wrap_tag = [0u8; 16];
        dek_wrap_tag.copy_from_slice(&buf[176..192]);
        let mut label = [0u8; 64];
        label.copy_from_slice(&buf[192..256]);
        Ok(StaticHeader {
            format_version,
            header_version,
            block_size,
            total_blocks: rd_u64(&buf[16..]),
            sb_slot_a: rd_u64(&buf[24..]),
            sb_slot_b: rd_u64(&buf[32..]),
            first_data_block: rd_u64(&buf[40..]),
            uuid,
            feature_compat: rd_u64(&buf[64..]),
            feature_incompat: rd_u64(&buf[72..]),
            feature_ro_compat: rd_u64(&buf[80..]),
            kdf_algo: buf[88],
            kdf_salt,
            argon_m_cost: rd_u32(&buf[112..]),
            argon_t_cost: rd_u32(&buf[116..]),
            argon_p: rd_u32(&buf[120..]),
            dek_wrap_nonce,
            dek_wrapped,
            dek_wrap_tag,
            label,
        })
    }
}
