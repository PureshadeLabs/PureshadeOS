// PAL — memory allocation.
//
// The global allocator is provided by lythos-std (a 4 MiB static free-list
// heap).  This module adds the ability to map additional pages from the kernel
// via SYS_MMAP for larger allocations.

/// Map `npages` anonymous pages at `virt` (must be page-aligned).
pub fn map_pages(virt: u64, npages: usize) -> Result<(), lythos_rt::SysError> {
    // phys=0 → allocate a fresh physical frame (anonymous mapping).
    // flags: PRESENT(1) | WRITABLE(2) | USER(4) | NX(1<<63)
    const RW_USER: u64 = (1 << 63) | 0x7;
    for i in 0..npages as u64 {
        lythos_rt::sys_mmap(virt + i * 4096, 0, RW_USER)?;
    }
    Ok(())
}

/// Unmap a previously mapped page.
pub fn unmap_page(virt: u64) -> Result<(), lythos_rt::SysError> {
    lythos_rt::sys_munmap(virt)
}
