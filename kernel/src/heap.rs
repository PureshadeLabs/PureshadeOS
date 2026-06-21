/// Kernel heap — linked-list `GlobalAlloc` backed by pre-mapped virtual pages.
///
/// ## Layout
///
/// ```text
/// 0xFFFF_C000_0000_0000  ← HEAP_START
///   ┌──────────────────────────────────────────────┐
///   │  FreeBlock header (HEADER bytes)             │  ← embedded at block start
///   │  usable bytes …                              │
///   └──────────────────────────────────────────────┘
///   ┌──────────────────────────────────────────────┐
///   │  next block …                                │
///   └──────────────────────────────────────────────┘
/// ```
///
/// ## Free-list structure
///
/// Each free block starts with a `FreeBlock` header (16 bytes):
/// - `size: usize` — usable bytes after the header
/// - `next: *mut FreeBlock` — singly-linked list pointer (null = end)
///
/// `alloc` does a first-fit walk.  If the matching block has enough leftover
/// (≥ `HEADER + ALIGN` bytes) it is split; otherwise the whole block is used.
///
/// `dealloc` reinserts the block at the head of the free list.  Coalescing is
/// omitted for now; the early kernel workload (a handful of long-lived `Box`
/// and `Vec` objects) does not create the fragmentation pattern that would
/// require it.
///
/// ## Alignment
///
/// All allocations are aligned to `ALIGN` (16 bytes).  The heap region starts
/// at a 4 KiB boundary, and every split point preserves alignment.
/// `layout.align() > ALIGN` is rejected with a panic (not needed in early
/// kernel code; revisit when page-table–aligned allocations are required).
///
/// ## Thread safety
///
/// Single-threaded at this stage.  `KernelAllocator` carries a raw `UnsafeCell`
/// and is marked `Sync` manually.  A spinlock wrapper will be added with the
/// scheduler (Step 7).

use core::alloc::{GlobalAlloc, Layout};
use core::cell::UnsafeCell;
use core::ptr;

use crate::pmm;
use crate::vmm::{PageFlags, VirtAddr, map_page};

// ── Heap layout constants ─────────────────────────────────────────────────────

/// Nominal (pre-KASLR) virtual base address of the kernel heap.
const HEAP_BASE_NOMINAL: u64 = 0xFFFF_C000_0000_0000;

/// Return the KASLR-adjusted heap base address.
///
/// Must be called after `kaslr::init()`.  All code that previously used the
/// `HEAP_START` constant should call this function instead.
#[inline]
pub fn heap_start() -> u64 {
    HEAP_BASE_NOMINAL + crate::kaslr::offset()
}

/// Number of 4 KiB pages pre-mapped at `init` time: 4 MiB = 1024 pages.
pub const HEAP_INIT_PAGES: usize = 1024;

// ── Free-list node ────────────────────────────────────────────────────────────

struct FreeBlock {
    /// Usable bytes immediately following this header.
    size: usize,
    /// Next free block, or null if this is the tail.
    next: *mut FreeBlock,
}

/// Size of the `FreeBlock` header in bytes.
const HEADER: usize = core::mem::size_of::<FreeBlock>(); // 16 bytes on 64-bit

/// Minimum alignment for all allocations (and all block-start addresses).
const ALIGN: usize = 16;

// ── Allocator ─────────────────────────────────────────────────────────────────

pub struct KernelAllocator {
    /// Head of the free list; null means the heap is empty or uninitialised.
    head: UnsafeCell<*mut FreeBlock>,
}

// SAFETY: single-threaded kernel; no concurrent alloc/dealloc at this stage.
unsafe impl Sync for KernelAllocator {}

impl KernelAllocator {
    pub const fn new() -> Self {
        Self { head: UnsafeCell::new(ptr::null_mut()) }
    }
}

/// Align `val` up to the nearest multiple of `align` (must be a power of 2).
#[inline]
fn align_up(val: usize, align: usize) -> usize {
    (val + align - 1) & !(align - 1)
}

unsafe impl GlobalAlloc for KernelAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        assert!(
            layout.align() <= ALIGN,
            "heap: requested alignment {} exceeds ALIGN {}",
            layout.align(), ALIGN,
        );

        // Round requested size up to a multiple of ALIGN.
        let size = align_up(layout.size().max(1), ALIGN);

        // `prev_next` always points to the list-link that holds `curr`'s address:
        // either the head pointer or a `next` field in a preceding block.
        let head_ptr: *mut *mut FreeBlock = self.head.get();
        let mut prev_next = head_ptr;
        let mut curr = unsafe { *head_ptr };

        while !curr.is_null() {
            let block_size = unsafe { (*curr).size };

            if block_size >= size {
                let leftover = block_size - size;

                if leftover >= HEADER + ALIGN {
                    // Split: carve `size` bytes from the front, keep the rest
                    // as a new free block immediately after.
                    let new_blk = (curr as usize + HEADER + size) as *mut FreeBlock;
                    unsafe {
                        (*new_blk).size = leftover - HEADER;
                        (*new_blk).next = (*curr).next;
                        *prev_next = new_blk;
                    }
                } else {
                    // No useful remainder — use the whole block.
                    unsafe { *prev_next = (*curr).next; }
                }

                // User data starts right after the header.
                return (curr as usize + HEADER) as *mut u8;
            }

            // Advance: prev_next now points to curr's `next` field.
            prev_next = unsafe { ptr::addr_of_mut!((*curr).next) };
            curr     = unsafe { (*curr).next };
        }

        // No suitable block found.
        ptr::null_mut()
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // The header lives at [ptr − HEADER .. ptr).  Since alloc always
        // returns (block + HEADER) with no alignment padding (layout.align ≤
        // ALIGN and blocks are ALIGN-aligned), this subtraction is exact.
        let block = (ptr as usize - HEADER) as *mut FreeBlock;
        let size  = align_up(layout.size().max(1), ALIGN);

        unsafe {
            (*block).size = size;
            // Insert at the head of the free list (O(1), no coalescing).
            (*block).next = *self.head.get();
            *self.head.get() = block;
        }
    }
}

// ── Global allocator registration ─────────────────────────────────────────────

#[global_allocator]
pub static ALLOCATOR: KernelAllocator = KernelAllocator::new();

// ── OOM handler ───────────────────────────────────────────────────────────────

/// Called by the `alloc` crate when an allocation cannot be satisfied.
#[unsafe(no_mangle)]
pub extern "C" fn __rust_alloc_error_handler(size: usize, align: usize) -> ! {
    crate::kprintln!(
        "[OOM] allocation failed: size={:#x} align={:#x}",
        size, align
    );
    loop { unsafe { core::arch::asm!("hlt") }; }
}

// ── Initialisation ────────────────────────────────────────────────────────────

/// Initialise the kernel heap.
///
/// Pre-maps `HEAP_INIT_PAGES` × 4 KiB pages at `HEAP_START` using physical
/// frames from the PMM, then sets up the initial free block covering the
/// entire region.
///
/// Must be called after `vmm::init()` (so that `map_page` is live) and before
/// any `alloc::` usage.
pub fn init() {
    let base = heap_start();
    for i in 0..HEAP_INIT_PAGES {
        let virt = VirtAddr(base + (i as u64) * pmm::FRAME_SIZE);
        let phys = pmm::alloc_frame().expect("heap::init: out of physical frames");
        map_page(virt, phys, PageFlags::KERNEL_RW);
    }

    // Carve the entire pre-mapped region into a single initial free block.
    let heap_bytes = HEAP_INIT_PAGES * pmm::FRAME_SIZE as usize;
    unsafe {
        let head_blk = base as *mut FreeBlock;
        (*head_blk).size = heap_bytes - HEADER;
        (*head_blk).next = ptr::null_mut();
        *ALLOCATOR.head.get() = head_blk;
    }
}
