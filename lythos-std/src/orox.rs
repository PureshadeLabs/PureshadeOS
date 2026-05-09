//! OROX binary manifest format — self-describing OROS executable header.
//!
//! An OROX file is an ELF64 binary with a 264-byte prefix prepended:
//!
//! ```text
//! [0..4]   magic:   b"OROX"
//! [4]      version: 1
//! [5..8]   _pad:    [0; 3]
//! [8..264] body:    OroxBody (256 bytes)
//! [264..]  elf:     standard ELF64 binary
//! ```
//!
//! Body layout (256 bytes at prefix offset 8):
//!
//! ```text
//! offset  size  field
//!  0       1    restart        (RESTART_* constant)
//!  1       1    restart_max    (OnFailure retry limit; 0 → default 3)
//!  2       1    cap_count      (0..=8)
//!  3       1    dep_count      (0..=4)
//!  4       8    caps           (CAP_* per slot; unused slots = 0)
//!  12      32   name           (service name, null-terminated ASCII)
//!  44     128   deps           (4 × 32-byte null-terminated dep names)
//! 172      84   _pad           (zeroed)
//! ```

pub const OROX_MAGIC:       [u8; 4] = *b"OROX";
pub const OROX_VERSION:     u8      = 1;
/// Total size of the OROX prefix (8-byte header + 256-byte body).
pub const OROX_PREFIX_SIZE: usize   = 264;

// ── Restart policy codes ──────────────────────────────────────────────────────

pub const RESTART_NEVER:      u8 = 0;
pub const RESTART_ON_FAILURE: u8 = 1;
pub const RESTART_ALWAYS:     u8 = 2;

// ── Capability kind codes ─────────────────────────────────────────────────────

pub const CAP_MEMORY:   u8 = 0;
pub const CAP_ROLLBACK: u8 = 1;
pub const CAP_IPC:      u8 = 2;
pub const CAP_REGISTRY: u8 = 3;

// ── Body ──────────────────────────────────────────────────────────────────────

/// Parsed OROX manifest body.
#[derive(Clone, Debug)]
pub struct OroxBody {
    pub restart:     u8,
    pub restart_max: u8,
    pub cap_count:   u8,
    pub dep_count:   u8,
    pub caps:        [u8; 8],
    pub name:        [u8; 32],
    pub deps:        [[u8; 32]; 4],
}

impl OroxBody {
    /// Service name as `&str` (null-terminated, UTF-8).
    pub fn name_str(&self) -> &str {
        let n = self.name.iter().position(|&b| b == 0).unwrap_or(32);
        core::str::from_utf8(&self.name[..n]).unwrap_or("")
    }

    /// Dependency name at index `i` as `&str`.
    pub fn dep_str(&self, i: usize) -> &str {
        if i >= self.dep_count as usize || i >= 4 { return ""; }
        let d = &self.deps[i];
        let n = d.iter().position(|&b| b == 0).unwrap_or(32);
        core::str::from_utf8(&d[..n]).unwrap_or("")
    }
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Try to parse an OROX prefix from `data`.
///
/// Returns `Some(OroxBody)` if `data` starts with a valid OROX header,
/// `None` if it is a plain ELF or any other non-OROX file.
pub fn parse_orox(data: &[u8]) -> Option<OroxBody> {
    if data.len() < OROX_PREFIX_SIZE { return None; }
    if data[0..4] != OROX_MAGIC      { return None; }
    if data[4]    != OROX_VERSION    { return None; }

    let b = &data[8..264];

    let mut caps = [0u8; 8];
    caps.copy_from_slice(&b[4..12]);

    let mut name = [0u8; 32];
    name.copy_from_slice(&b[12..44]);

    let mut deps = [[0u8; 32]; 4];
    for i in 0..4 {
        deps[i].copy_from_slice(&b[44 + i * 32..44 + (i + 1) * 32]);
    }

    Some(OroxBody {
        restart:     b[0],
        restart_max: b[1],
        cap_count:   b[2],
        dep_count:   b[3],
        caps,
        name,
        deps,
    })
}

/// Return the ELF slice from a raw file buffer, stripping any OROX prefix.
///
/// If `data` starts with `b"OROX"` the first 264 bytes are the OROX header
/// and the ELF begins at offset 264.  Otherwise the slice is returned unchanged.
#[inline]
pub fn elf_slice(data: &[u8]) -> &[u8] {
    if data.len() >= OROX_PREFIX_SIZE && data[0..4] == OROX_MAGIC {
        &data[OROX_PREFIX_SIZE..]
    } else {
        data
    }
}

/// Build a 264-byte OROX prefix from a body.
///
/// Used by the `orox-pack` host tool; available here for potential in-OROS use.
pub fn build_prefix(body: &OroxBody) -> [u8; OROX_PREFIX_SIZE] {
    let mut prefix = [0u8; OROX_PREFIX_SIZE];
    prefix[0..4].copy_from_slice(&OROX_MAGIC);
    prefix[4] = OROX_VERSION;
    // [5..8] = 0 (pad)
    let b = &mut prefix[8..264];
    b[0] = body.restart;
    b[1] = body.restart_max;
    b[2] = body.cap_count;
    b[3] = body.dep_count;
    b[4..12].copy_from_slice(&body.caps);
    b[12..44].copy_from_slice(&body.name);
    for i in 0..4 {
        b[44 + i * 32..44 + (i + 1) * 32].copy_from_slice(&body.deps[i]);
    }
    prefix
}
