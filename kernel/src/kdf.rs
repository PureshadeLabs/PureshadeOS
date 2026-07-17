//! Mount-time key derivation scratch for encrypted RFS2 volumes (doc 08 §5–6).
//!
//! Argon2id needs a large memory buffer (baseline 64 MiB) that must NOT come
//! from the 2 MiB kernel heap. [`Argon2Scratch`] carves it from the PMM as one
//! physically-contiguous run and hands `rfs2::crypto` a `&mut [Argon2Block]`.
//!
//! Three disciplines, all load-bearing:
//!
//! 1. **Direct-map only.** The frames are reached through the kernel's identity
//!    / direct map (`pa == va` below [`IDENTITY_MAP_LIMIT`]). We verify the
//!    *entire* run lies inside that window **before touching a byte** — the
//!    exact `RamDisk` lesson. A run outside it is refused cleanly, never a
//!    page fault.
//! 2. **Fail loud.** If the PMM cannot supply the contiguous run, or it falls
//!    outside the direct map, allocation returns `None` and the caller aborts
//!    the mount with an error — encryption never silently downgrades.
//! 3. **Wipe on free.** The buffer holds passphrase-derived KDF state. Before
//!    the frames return to the general pool (whence they may be handed to a
//!    build or to userspace) the whole region is zeroed.

use alloc::vec::Vec;

use rfs2::crypto::Argon2Block;
use rfs2::StaticHeader;

use crate::pmm::{self, PhysAddr, FRAME_SIZE};
use crate::vmm::IDENTITY_MAP_LIMIT;

/// One Argon2 block is 1 KiB (`[u64; 128]`).
const ARGON2_BLOCK_BYTES: usize = 1024;

/// A transient, PMM-backed, direct-map-reachable Argon2id memory buffer.
pub struct Argon2Scratch {
    /// Physical == virtual base (identity map).
    base: u64,
    nframes: usize,
    /// Usable `Argon2Block` count handed to the KDF.
    blocks: usize,
}

impl Argon2Scratch {
    /// Reserve contiguous PMM frames for `blocks` Argon2 blocks. `None` (loud,
    /// no fault) if the pool cannot satisfy the run or it would fall outside
    /// the direct map.
    pub fn alloc(blocks: usize) -> Option<Argon2Scratch> {
        let bytes = blocks.checked_mul(ARGON2_BLOCK_BYTES)?;
        let frame = FRAME_SIZE as usize;
        let nframes = bytes.div_ceil(frame);

        let pa = pmm::alloc_frames_contiguous(nframes).or_else(|| {
            crate::kprintln!(
                "[kdf] argon2 scratch: PMM cannot supply {} contiguous frames ({} MiB) — \
                 mount refused (no silent downgrade)",
                nframes,
                (nframes * frame) >> 20,
            );
            None
        })?;
        let base = pa.as_u64();

        // Whole-run direct-map check, BEFORE any access (the RamDisk lesson).
        let end = base.checked_add((nframes as u64) * FRAME_SIZE);
        if end.is_none_or(|e| e > IDENTITY_MAP_LIMIT) {
            crate::kprintln!(
                "[kdf] argon2 scratch {:#x}..+{:#x} outside the {} MiB direct map — \
                 refusing cleanly (mount fails, no fault)",
                base,
                (nframes as u64) * FRAME_SIZE,
                IDENTITY_MAP_LIMIT >> 20,
            );
            pmm::free_frames_contiguous(pa, nframes);
            return None;
        }

        // Safe to touch: the whole span is inside the direct map. Start clean.
        unsafe { core::ptr::write_bytes(base as *mut u8, 0, nframes * frame) };
        Some(Argon2Scratch { base, nframes, blocks })
    }

    /// The KDF memory as `rfs2::crypto` expects it. Aliased over the direct map;
    /// the whole run was validated in [`alloc`](Self::alloc).
    pub fn as_blocks(&mut self) -> &mut [Argon2Block] {
        unsafe { core::slice::from_raw_parts_mut(self.base as *mut Argon2Block, self.blocks) }
    }
}

impl Drop for Argon2Scratch {
    fn drop(&mut self) {
        // Wipe-on-free: this held passphrase-derived key-derivation state. Once
        // the frames rejoin the general pool they can be handed to a build or
        // to userspace, so key-adjacent material must not linger.
        unsafe { core::ptr::write_bytes(self.base as *mut u8, 0, self.nframes * FRAME_SIZE as usize) };
        pmm::free_frames_contiguous(PhysAddr(self.base), self.nframes);
    }
}

// ── Passphrase ceremony (doc 08 §6) ──────────────────────────────────────────

/// Read a passphrase from the console, echoed masked. Polls the i8042 directly
/// (`keyboard::try_read` polls hardware), so it works this early in boot before
/// IRQs drive input. Returns the raw bytes; the caller must zeroize them.
pub fn read_passphrase(prompt: &str) -> Vec<u8> {
    crate::kprint!("{}", prompt);
    let mut buf: Vec<u8> = Vec::new();
    loop {
        match crate::keyboard::try_read() {
            Some(b'\n') | Some(b'\r') => {
                crate::kprintln!("");
                break;
            }
            // Backspace / delete: drop the last byte and erase the mask glyph.
            Some(0x08) | Some(0x7f) => {
                if buf.pop().is_some() {
                    crate::kprint!("\u{8} \u{8}");
                }
            }
            Some(0x1b) => {} // swallow bare ESC / escape-sequence lead-in
            Some(c) => {
                buf.push(c);
                crate::kprint!("*");
            }
            None => unsafe { core::arch::asm!("hlt") },
        }
    }
    buf
}

/// Derive the volume DEK from `passphrase` + the volume's stored KDF params
/// (doc 08 §6). Allocates the transient Argon2 scratch, runs Argon2id, and
/// unwraps the DEK. `None` on a wrong passphrase, a tampered header, or if the
/// scratch cannot be allocated (loud). The scratch is wiped + freed on return.
pub fn open_volume_dek(passphrase: &[u8], header: &StaticHeader) -> Option<[u8; 32]> {
    let blocks = rfs2::crypto::argon2_block_count(
        header.argon_m_cost,
        header.argon_t_cost,
        header.argon_p,
    )
    .ok()?;
    let mut scratch = Argon2Scratch::alloc(blocks)?;
    rfs2::open_dek(passphrase, header, scratch.as_blocks()).ok()
}

// ── Randomness for freshly-minted volume keys (doc 08 §6) ────────────────────

/// Whether the CPU supports RDRAND (CPUID.01H:ECX bit 30). Executing `rdrand`
/// without this check faults `#UD` on CPUs that lack it (e.g. QEMU's default
/// `qemu64` model) — the guard `kaslr.rs` also uses.
fn rdrand_supported() -> bool {
    let ecx: u32;
    unsafe {
        core::arch::asm!(
            "push rbx",           // CPUID clobbers rbx (LLVM reserves it)
            "mov eax, 1",
            "cpuid",
            "pop rbx",
            out("ecx") ecx,
            lateout("eax") _,
            lateout("edx") _,
            options(nostack),
        );
    }
    ecx & (1 << 30) != 0
}

/// One 64-bit RDRAND value, retried per Intel's guidance. `None` if the CPU
/// never returns a good value (RDRAND absent or persistently failing).
fn rdrand64() -> Option<u64> {
    if !rdrand_supported() {
        return None;
    }
    for _ in 0..10u32 {
        let val: u64;
        let cf: u8;
        unsafe {
            core::arch::asm!(
                "rdrand {val}",
                "setc {cf}",
                val = out(reg) val,
                cf = out(reg_byte) cf,
                options(nostack, nomem),
            );
        }
        if cf != 0 {
            return Some(val);
        }
    }
    None
}

/// A fresh random 256-bit key from RDRAND (a minted store DEK). `None` if the
/// hardware RNG is unavailable — the caller fails the mount loud rather than
/// fabricating a weak key.
pub fn rand_key() -> Option<[u8; 32]> {
    let mut key = [0u8; 32];
    for chunk in key.chunks_mut(8) {
        let v = rdrand64()?;
        chunk.copy_from_slice(&v.to_le_bytes());
    }
    Some(key)
}
