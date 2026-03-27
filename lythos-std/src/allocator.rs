/// Bump allocator backed by `SYS_MMAP`.
///
/// The heap grows upward from `HEAP_BASE` one 4 KiB page at a time.
/// `dealloc` is a no-op — freed memory is never reclaimed.  This is
/// sufficient for long-running daemons like lythd whose heap only grows.
///
/// ## Heap virtual address
///
/// `HEAP_BASE = 0x0000_0010_0000_0000` (64 GiB).  This sits well above the
/// ELF load region (`0x0000_0001_0000_0000`) and well below the user stack
/// region (`0x0000_7FFF_0000_0000`), so there is no overlap.

use core::alloc::{GlobalAlloc, Layout};
use core::sync::atomic::{AtomicUsize, Ordering};

use crate::syscall::{SYS_MMAP, syscall3};

const HEAP_BASE: usize = 0x0000_0010_0000_0000;
const PAGE_SIZE: usize = 4096;

// USER_RW: PRESENT | WRITABLE | USER | NX
const FLAGS_RW: u64 = (1u64 << 63) | (1 << 2) | (1 << 1) | 1;

/// Pointer to the next free byte in the bump heap.
static HEAP_NEXT: AtomicUsize = AtomicUsize::new(HEAP_BASE);
/// End of the last mapped page (exclusive).
static HEAP_END:  AtomicUsize = AtomicUsize::new(HEAP_BASE);

pub struct BumpAllocator;

unsafe impl GlobalAlloc for BumpAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let align = layout.align();
        let size  = layout.size();

        loop {
            let cur     = HEAP_NEXT.load(Ordering::Relaxed);
            let aligned = (cur + align - 1) & !(align - 1);
            let new_end = aligned + size;

            // Grow the mapped region if needed.
            let mut heap_end = HEAP_END.load(Ordering::Relaxed);
            while heap_end < new_end {
                let va = heap_end as u64;
                // SYS_MMAP(va, /*ignored phys*/0, flags)
                let r = unsafe { syscall3(SYS_MMAP, va, 0, FLAGS_RW) };
                if r != 0 {
                    // Syscall returned an error — out of memory.
                    return core::ptr::null_mut();
                }
                heap_end += PAGE_SIZE;
            }
            // Publish the new heap end before advancing the bump pointer.
            HEAP_END.store(heap_end, Ordering::Relaxed);

            // Advance bump pointer (single-threaded; CAS is defensive).
            match HEAP_NEXT.compare_exchange(
                cur, new_end, Ordering::Relaxed, Ordering::Relaxed,
            ) {
                Ok(_)  => return aligned as *mut u8,
                Err(_) => continue,
            }
        }
    }

    unsafe fn dealloc(&self, _ptr: *mut u8, _layout: Layout) {
        // Bump allocator: no-op.
    }
}

#[global_allocator]
static ALLOCATOR: BumpAllocator = BumpAllocator;
