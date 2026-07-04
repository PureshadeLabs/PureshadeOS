/// Physical Memory Manager — bitmap allocator, one bit per 4 KiB frame.
///
/// Initialisation flow:
///   1. All frames start marked *used*.
///   2. Walk the Limine memory-map entries; mark type 0 (USABLE) regions free.
///   3. Re-mark frames occupied by the kernel image as *used*.
///   4. Re-mark physical frame 0 (real-mode IVT / BIOS data area) as *used*.
///
/// Convention: bit = 0 → free, bit = 1 → used.
///
/// `alloc_frame` / `free_frame` are intentionally not locked — at this stage
/// the kernel is single-threaded.  A SpinLock wrapper will be added once the
/// scheduler exists.

use core::ptr;
use core::sync::atomic::{AtomicUsize, Ordering};

// ── Constants ────────────────────────────────────────────────────────────────

pub const FRAME_SIZE: u64 = 4096;

/// Maximum supported physical address space: 4 GiB.
const MAX_FRAMES: usize = (4 * 1024 * 1024 * 1024u64 / FRAME_SIZE) as usize; // 1 M frames

/// One bit per frame; 1 M frames / 64 bits = 16384 u64 words = 128 KiB.
const BITMAP_WORDS: usize = MAX_FRAMES / 64;

// Limine memory-map type constants (per Limine protocol spec §5.4).
const LIMINE_MMAP_USABLE: u64 = 0;

// ── PhysAddr ─────────────────────────────────────────────────────────────────

/// A physical address.  Newtype ensures it is never confused with a virtual one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct PhysAddr(pub u64);

impl PhysAddr {
    pub fn as_u64(self) -> u64 {
        self.0
    }
}

// ── Bitmap storage ───────────────────────────────────────────────────────────

/// Placed in .bss; zero-initialised (all frames appear free before init).
/// init_from_limine() overwrites this to all-ones before parsing the memory map.
static mut BITMAP: [u64; BITMAP_WORDS] = [0u64; BITMAP_WORDS];

/// Number of currently free frames.
static FREE_FRAMES: AtomicUsize = AtomicUsize::new(0);

// ── Bit-level helpers ────────────────────────────────────────────────────────
//
// All three use addr_of! / addr_of_mut! to obtain raw pointers to the
// static mut BITMAP without creating any (mutable or shared) reference,
// which Rust 2024 forbids for mutable statics.

#[inline]
unsafe fn set_used(frame: usize) {
    unsafe {
        let word = ptr::addr_of_mut!(BITMAP) as *mut u64;
        let word = word.add(frame / 64);
        *word |= 1u64 << (frame % 64);
    }
}

#[inline]
unsafe fn set_free(frame: usize) {
    unsafe {
        let word = ptr::addr_of_mut!(BITMAP) as *mut u64;
        let word = word.add(frame / 64);
        *word &= !(1u64 << (frame % 64));
    }
}

#[inline]
unsafe fn is_used(frame: usize) -> bool {
    unsafe {
        let word = ptr::addr_of!(BITMAP) as *const u64;
        let word = word.add(frame / 64);
        *word & (1u64 << (frame % 64)) != 0
    }
}

// ── Range helpers ────────────────────────────────────────────────────────────

/// Mark frames that are *wholly inside* [start, end) as free.
/// `start` is rounded up; `end` is rounded down — a partially covered frame
/// stays used.
unsafe fn mark_range_free(start: u64, end: u64) {
    if end <= start {
        return;
    }
    let first = ((start + FRAME_SIZE - 1) / FRAME_SIZE) as usize;
    let last = (end / FRAME_SIZE) as usize;
    for i in first..last.min(MAX_FRAMES) {
        unsafe { set_free(i) };
    }
}

/// Mark every frame that overlaps [start, end) as used.
/// `start` is rounded down; `end` is rounded up — a partially covered frame
/// becomes used.
unsafe fn mark_range_used(start: u64, end: u64) {
    if end <= start {
        return;
    }
    let first = (start / FRAME_SIZE) as usize;
    let last = ((end + FRAME_SIZE - 1) / FRAME_SIZE) as usize;
    for i in first..last.min(MAX_FRAMES) {
        unsafe { set_used(i) };
    }
}

// ── Public API ───────────────────────────────────────────────────────────────

/// Initialise the PMM from the Limine memory map.
///
/// `entries` is a slice of `(base, length, type)` tuples copied from the
/// Limine memory-map response before `vmm::init()` replaced CR3.
/// Only type `0` (USABLE) entries are freed; all others remain marked used.
///
/// `kernel_phys_base`/`kernel_phys_end` are the physical address range of the
/// kernel image (from Limine's KernelAddressResponse).  These frames are
/// re-marked used even if they fall inside a USABLE mmap region.
///
/// Must be called exactly once, before any `alloc_frame` / `free_frame`.
pub fn init_from_limine(
    entries: &[(u64, u64, u64)],
    kernel_phys_base: u64,
    kernel_phys_end: u64,
) {
    // 1. Mark all frames used.
    unsafe {
        let p = ptr::addr_of_mut!(BITMAP) as *mut u64;
        for i in 0..BITMAP_WORDS {
            *p.add(i) = !0u64;
        }
    }

    // 2. Free only USABLE regions (type 0).
    for &(base, length, typ) in entries {
        if typ == LIMINE_MMAP_USABLE {
            unsafe { mark_range_free(base, base + length) };
        }
    }

    // 3. Re-mark the kernel image as used (it sits inside usable RAM).
    unsafe { mark_range_used(kernel_phys_base, kernel_phys_end) };

    // 4. Always reserve physical frame 0 (real-mode IVT / BIOS data area).
    unsafe { set_used(0) };

    // 5. Count free frames.
    let free: usize = unsafe {
        let p = ptr::addr_of!(BITMAP) as *const u64;
        (0..BITMAP_WORDS).map(|i| (*p.add(i)).count_zeros() as usize).sum()
    };
    FREE_FRAMES.store(free, Ordering::Relaxed);
}

/// Allocate one 4 KiB physical frame.  Returns `None` if out of memory.
pub fn alloc_frame() -> Option<PhysAddr> {
    unsafe {
        let p = ptr::addr_of_mut!(BITMAP) as *mut u64;
        for wi in 0..BITMAP_WORDS {
            let word = p.add(wi);
            if *word == !0u64 {
                continue; // all bits used
            }
            let bit = (*word).trailing_ones() as usize;
            *word |= 1u64 << bit;
            FREE_FRAMES.fetch_sub(1, Ordering::Relaxed);
            let frame = wi * 64 + bit;
            return Some(PhysAddr(frame as u64 * FRAME_SIZE));
        }
    }
    None
}

/// Return a previously allocated frame to the free pool.
///
/// Panics on double-free or out-of-range / unaligned address.
pub fn free_frame(addr: PhysAddr) {
    assert!(
        addr.0 % FRAME_SIZE == 0,
        "pmm::free_frame: address {:#x} is not frame-aligned",
        addr.0
    );
    let frame = (addr.0 / FRAME_SIZE) as usize;
    assert!(frame < MAX_FRAMES, "pmm::free_frame: address {:#x} out of range", addr.0);
    unsafe {
        assert!(
            is_used(frame),
            "pmm::free_frame: double free of frame {:#x}",
            addr.0
        );
        set_free(frame);
    }
    FREE_FRAMES.fetch_add(1, Ordering::Relaxed);
}

/// Number of free 4 KiB frames currently available.
pub fn free_frame_count() -> usize {
    FREE_FRAMES.load(Ordering::Relaxed)
}

/// Allocate `n` physically-contiguous 4 KiB frames.
///
/// Returns the physical address of the first frame, or `None` if no
/// contiguous run of `n` free frames exists.  The caller is responsible
/// for zeroing the allocation before use.
///
/// O(MAX_FRAMES × n) scan — acceptable for small `n` at boot time.
pub fn alloc_frames_contiguous(n: usize) -> Option<PhysAddr> {
    if n == 0 { return None; }
    unsafe {
        let mut start = 0usize;
        while start + n <= MAX_FRAMES {
            // Check whether n consecutive frames starting at `start` are free.
            let mut all_free = true;
            for i in 0..n {
                if is_used(start + i) {
                    // Skip past the used frame — no run can start before it.
                    start = start + i + 1;
                    all_free = false;
                    break;
                }
            }
            if all_free {
                for i in 0..n {
                    set_used(start + i);
                }
                FREE_FRAMES.fetch_sub(n, Ordering::Relaxed);
                return Some(PhysAddr(start as u64 * FRAME_SIZE));
            }
        }
    }
    None
}

/// Free `n` physically-contiguous frames starting at `addr`.
///
/// `addr` must be page-aligned and must have been allocated by
/// `alloc_frames_contiguous(n)` or `n` consecutive `alloc_frame` calls.
pub fn free_frames_contiguous(addr: PhysAddr, n: usize) {
    for i in 0..n {
        free_frame(PhysAddr(addr.0 + i as u64 * FRAME_SIZE));
    }
}
