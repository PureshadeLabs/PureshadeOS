/// Boot-info message pre-queued by the kernel on cap handle 2.
///
/// Exactly 64 bytes — one IPC message slot.

pub const BOOT_SIGNATURE: u64 = 0xB007_1000_B007_1000;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct BootInfo {
    pub signature:   u64,       // offset  0: must equal BOOT_SIGNATURE
    pub mem_bytes:   u64,       // offset  8: total free RAM in bytes
    pub free_frames: u64,       // offset 16: free 4 KiB frame count at boot
    pub vendor:      [u8; 12],  // offset 24: CPUID leaf 0 vendor string
    pub _pad:        [u8; 28],  // offset 36: zeroed
}

const _: () = assert!(core::mem::size_of::<BootInfo>() == 64);

impl BootInfo {
    /// Parse a 64-byte IPC frame as a `BootInfo`.
    ///
    /// Returns `None` if the signature field does not match `BOOT_SIGNATURE`.
    pub fn from_bytes(buf: &[u8; 64]) -> Option<Self> {
        // SAFETY: BootInfo is repr(C), size == 64, no invalid bit-patterns.
        let info: Self = unsafe { core::ptr::read_unaligned(buf.as_ptr() as *const Self) };
        if info.signature == BOOT_SIGNATURE { Some(info) } else { None }
    }

    /// Return the vendor string as a `&str`, trimming trailing null bytes.
    pub fn vendor_str(&self) -> &str {
        let n = self.vendor.iter().position(|&b| b == 0).unwrap_or(12);
        core::str::from_utf8(&self.vendor[..n]).unwrap_or("unknown")
    }
}
