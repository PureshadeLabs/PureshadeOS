//! DMA buffer allocation over the framework's `sys_dev_dma_alloc`.
//!
//! Each [`DmaBuf`] is a physically-contiguous, kernel-zeroed region mapped into
//! the driver at a caller-chosen virtual address, with its physical address
//! exposed (to program into device descriptors). Buffers are handed out at
//! successive virtual addresses from a fixed per-driver DMA window so distinct
//! allocations never overlap. An IOMMU, when added, would be programmed inside
//! `SYS_DEV_DMA_ALLOC` — drivers need no change.

use lythos_rt::{sys_dev_dma_alloc, SysError};

/// Base of the driver DMA virtual window (above code/heap, below stacks).
pub const DMA_WINDOW_BASE: u64 = 0x0000_0003_0000_0000;

/// A single framework-minted DMA buffer.
#[derive(Clone, Copy)]
pub struct DmaBuf {
    /// Virtual address the buffer is mapped at (CPU access).
    pub virt: u64,
    /// Physical address to program into the device (DMA target).
    pub phys: u64,
    /// Size in bytes (rounded up to a whole number of pages by the kernel).
    pub size: u64,
}

impl DmaBuf {
    /// Byte slice view for CPU-side reads/writes.
    #[inline]
    pub fn as_mut_slice(&self) -> &'static mut [u8] {
        unsafe { core::slice::from_raw_parts_mut(self.virt as *mut u8, self.size as usize) }
    }
}

/// Sequential DMA allocator: hands out buffers at increasing virtual addresses.
pub struct DmaPool {
    dev_cap:   u64,
    next_virt: u64,
}

impl DmaPool {
    pub fn new(dev_cap: u64) -> Self {
        Self { dev_cap, next_virt: DMA_WINDOW_BASE }
    }

    /// Allocate a `size`-byte DMA buffer (rounded up to pages). Returns the
    /// buffer with its physical address filled in.
    pub fn alloc(&mut self, size: u64) -> Result<DmaBuf, SysError> {
        let pages = (size + 0xFFF) & !0xFFF;
        let virt = self.next_virt;
        let phys = sys_dev_dma_alloc(self.dev_cap, virt, pages)?;
        self.next_virt += pages;
        Ok(DmaBuf { virt, phys, size: pages })
    }
}
