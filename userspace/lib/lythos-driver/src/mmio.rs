//! Volatile MMIO register accessor over a mapped BAR region.
//!
//! All accesses are `read_volatile`/`write_volatile` at `base + offset`. The
//! BAR must have been mapped uncacheable via `sys_dev_mmio_map` before use.

/// A memory-mapped register window starting at virtual address `base`.
#[derive(Clone, Copy)]
pub struct Mmio {
    base: u64,
}

impl Mmio {
    /// Wrap the region mapped at virtual address `base`.
    #[inline]
    pub const fn new(base: u64) -> Self { Self { base } }

    /// A sub-window `off` bytes into this one (for structures within a BAR).
    #[inline]
    pub const fn offset(self, off: u64) -> Self { Self { base: self.base + off } }

    #[inline]
    pub fn read8(self, off: u64) -> u8 {
        unsafe { ((self.base + off) as *const u8).read_volatile() }
    }
    #[inline]
    pub fn read16(self, off: u64) -> u16 {
        unsafe { ((self.base + off) as *const u16).read_volatile() }
    }
    #[inline]
    pub fn read32(self, off: u64) -> u32 {
        unsafe { ((self.base + off) as *const u32).read_volatile() }
    }
    #[inline]
    pub fn read64(self, off: u64) -> u64 {
        unsafe { ((self.base + off) as *const u64).read_volatile() }
    }

    #[inline]
    pub fn write8(self, off: u64, v: u8) {
        unsafe { ((self.base + off) as *mut u8).write_volatile(v) }
    }
    #[inline]
    pub fn write16(self, off: u64, v: u16) {
        unsafe { ((self.base + off) as *mut u16).write_volatile(v) }
    }
    #[inline]
    pub fn write32(self, off: u64, v: u32) {
        unsafe { ((self.base + off) as *mut u32).write_volatile(v) }
    }
    #[inline]
    pub fn write64(self, off: u64, v: u64) {
        unsafe { ((self.base + off) as *mut u64).write_volatile(v) }
    }
}
