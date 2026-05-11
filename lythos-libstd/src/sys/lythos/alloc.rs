// PAL — memory allocation.
//
// The global allocator is provided by lythos-std (a 4 MiB static free-list
// heap).  This module adds the ability to grow the heap at runtime by
// requesting additional pages from the kernel via SYS_MMAP.

use lythos_std::syscall::{SYS_MMAP, SYS_MUNMAP};

/// Page flags for a user read-write data page.
///
/// PRESENT(1) | WRITABLE(2) | USER(4) | NX(1<<63)
const RW_USER: u64 = (1 << 63) | 0x7;

/// Map `npages` anonymous pages at `virt` (must be page-aligned).
///
/// Returns `Ok(())` on success, `Err(raw_code)` on failure.
pub fn map_pages(virt: u64, npages: usize) -> Result<(), u64> {
    // Lythos SYS_MMAP takes (virt, phys, flags).  phys=0 means allocate a
    // fresh physical frame (anonymous mapping).
    for i in 0..npages as u64 {
        let v = virt + i * 4096;
        let ret = unsafe {
            lythos_std::syscall::syscall3(SYS_MMAP, v, 0, RW_USER)
        };
        if lythos_std::error::SysError::is_err(ret) {
            return Err(ret);
        }
    }
    Ok(())
}

/// Unmap a previously mapped page.
pub fn unmap_page(virt: u64) -> Result<(), u64> {
    let ret = unsafe { lythos_std::syscall::syscall1(SYS_MUNMAP, virt) };
    if lythos_std::error::SysError::is_err(ret) { Err(ret) } else { Ok(()) }
}
