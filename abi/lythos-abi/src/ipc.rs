//! IPC ring constants and BootInfo — transcribed from `docs/spec/ipc.md`.
//!
//! Cross-checked against `kernel/src/ipc.rs` and `userspace/lib/lythos-rt/src/boot.rs`.

// ── Ring constants ────────────────────────────────────────────────────────────
//
// Spec § "Ring buffer layout":
//   4 KiB page layout:
//     [0..4]   head : u32  (free-running consume counter)
//     [4..8]   tail : u32  (free-running produce counter)
//     [8..)    data : [u8; RING_DATA_BYTES]  (circular buffer)
//
// MSG_SIZE=64, RING_DATA_BYTES=4088, RING_CAPACITY=floor(4088/64)=63.
//
// Kernel ipc.rs:
//   RING_DATA_BYTES: usize = 4096 - 8   ← 4088 ✓
//   MSG_SIZE: usize = 64                 ← 64   ✓
//   RING_CAPACITY = RING_DATA_BYTES / MSG_SIZE = 63 ✓
//
// Full/empty test per spec:
//   empty: head == tail
//   full:  tail.wrapping_sub(head) >= RING_CAPACITY
// Kernel (ipc.rs): `used < RING_CAPACITY` to gate write → full = used >= CAPACITY ✓

/// Total size of one IPC ring page in bytes.
pub const RING_PAGE_SIZE: usize = 4096;

/// Byte offset of the head counter within the ring page.
pub const RING_HEAD_OFFSET: usize = 0;

/// Byte offset of the tail counter within the ring page.
pub const RING_TAIL_OFFSET: usize = 4;

/// Byte offset of the message data region within the ring page.
pub const RING_DATA_OFFSET: usize = 8;

/// Bytes available for message data within one ring page.
pub const RING_DATA_BYTES: usize = RING_PAGE_SIZE - 8;   // 4088

/// Fixed size of one message slot in bytes.
pub const MSG_SIZE: usize = 64;

/// Maximum number of messages in-flight simultaneously.
pub const RING_CAPACITY: usize = RING_DATA_BYTES / MSG_SIZE;  // 63

const _MSG_SIZE_CHECK:     () = assert!(MSG_SIZE == 64);
const _RING_CAPACITY_CHECK:() = assert!(RING_CAPACITY == 63);

// ── IPC kernel virtual base ───────────────────────────────────────────────────
//
// Spec § "Kernel mapping":
//   0xFFFF_D000_0000_0000 + endpoint_index * 4096

/// Nominal (pre-KASLR) base virtual address of the IPC endpoint page array
/// in the kernel's address space.
///
/// FINDING F7: The kernel adds `kaslr::offset()` to this at runtime
/// (`kernel/src/ipc.rs::ipc_kern_base()`).  The spec omits the KASLR
/// adjustment.  This constant is kernel-internal; userspace never accesses
/// endpoint pages directly (all access is via IPC syscalls).
pub const IPC_KERN_BASE_NOMINAL: u64 = 0xFFFF_D000_0000_0000;

// ── BootInfo — 64 bytes ───────────────────────────────────────────────────────
//
// Spec § "Message format: BootInfo":
//   [0..8]   u64    signature   = BOOT_SIGNATURE
//   [8..16]  u64    mem_bytes
//   [16..24] u64    free_frames
//   [24..36] [u8;12] vendor     (CPUID leaf 0: EBX||EDX||ECX)
//   [36..64] [u8;28] _pad       = zeroed
//
// lythos-rt/src/boot.rs BootInfo: #[repr(C)], size asserted == 64,
// same field layout — match ✓
//
// Kernel (main.rs): fills with signature=0xB007_1000_B007_1000,
// same as BOOT_SIGNATURE below — match ✓

/// Signature value in the first 8 bytes of a BootInfo message.
pub const BOOT_SIGNATURE: u64 = 0xB007_1000_B007_1000;

/// Boot message pre-queued by the kernel on capability handle 2.
///
/// Exactly one IPC message slot (64 bytes).  Received via `SYS_IPC_RECV` on
/// the boot IPC cap (handle 2) at process startup.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct BootInfo {
    /// Must equal `BOOT_SIGNATURE`.
    pub signature:   u64,
    /// Total free physical memory at boot in bytes (`free_frames * 4096`).
    pub mem_bytes:   u64,
    /// Free 4 KiB frame count at boot time.
    pub free_frames: u64,
    /// CPUID leaf 0 vendor string (EBX || EDX || ECX, 12 bytes).
    pub vendor:      [u8; 12],
    pub _pad:        [u8; 28],
}

const _BOOTINFO_SIZE:           () = assert!(core::mem::size_of::<BootInfo>()              == 64);
const _BOOTINFO_OFF_SIGNATURE:  () = assert!(core::mem::offset_of!(BootInfo, signature)    ==  0);
const _BOOTINFO_OFF_MEM_BYTES:  () = assert!(core::mem::offset_of!(BootInfo, mem_bytes)    ==  8);
const _BOOTINFO_OFF_FREE_FRAMES:() = assert!(core::mem::offset_of!(BootInfo, free_frames)  == 16);
const _BOOTINFO_OFF_VENDOR:     () = assert!(core::mem::offset_of!(BootInfo, vendor)       == 24);
const _BOOTINFO_OFF_PAD:        () = assert!(core::mem::offset_of!(BootInfo, _pad)         == 36);

impl BootInfo {
    /// Parse a 64-byte IPC frame as a `BootInfo`.
    ///
    /// Returns `None` if the signature does not match `BOOT_SIGNATURE`.
    pub fn from_bytes(buf: &[u8; 64]) -> Option<Self> {
        // SAFETY: BootInfo is repr(C), size == 64, no invalid bit-patterns for any field.
        let info: Self = unsafe { core::ptr::read_unaligned(buf.as_ptr() as *const Self) };
        if info.signature == BOOT_SIGNATURE { Some(info) } else { None }
    }

    /// CPU vendor string, trimmed to the first null byte.
    pub fn vendor_str(&self) -> &str {
        let n = self.vendor.iter().position(|&b| b == 0).unwrap_or(12);
        core::str::from_utf8(&self.vendor[..n]).unwrap_or("unknown")
    }
}
