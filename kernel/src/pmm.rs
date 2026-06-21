/// Physical Memory Manager — bitmap allocator, one bit per 4 KiB frame.
///
/// Initialisation flow:
///   1. All frames start marked *used*.
///   2. Parse the Multiboot1 or Multiboot2 memory map; mark available
///      (type = 1) regions as *free*.
///   3. Re-mark frames occupied by the kernel image as *used*.
///   4. Re-mark physical frame 0 (BIOS data area) as *used*.
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

/// Physical address where `run.sh` loads the lythd ELF via QEMU's
/// `-device loader,addr=LYTHD_MODULE_ADDR,force-raw=on`.
/// The PMM reserves this range so no allocator can reclaim the frames before
/// `kmain` copies the ELF to the heap.
pub const LYTHD_MODULE_ADDR: u64   = 0x0040_0000; // 4 MiB
/// Maximum size reserved for the lythd ELF.  Currently ~92 KiB; 512 KiB
/// gives generous headroom.
pub const LYTHD_MODULE_MAX:  usize = 512 * 1024;  // 512 KiB

/// Maximum supported physical address space: 4 GiB.
const MAX_FRAMES: usize = (4 * 1024 * 1024 * 1024u64 / FRAME_SIZE) as usize; // 1 M frames

/// One bit per frame; 1 M frames / 64 bits = 16384 u64 words = 128 KiB.
const BITMAP_WORDS: usize = MAX_FRAMES / 64;

// Multiboot boot-loader magics.
const MB1_MAGIC: u32 = 0x2BADB002;
const MB2_MAGIC: u32 = 0x36D76289;

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
/// init() overwrites this to all-ones before parsing the memory map.
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

// ── Multiboot1 parser ────────────────────────────────────────────────────────

/// MB1 mmap entry field offsets (relative to the entry base, which IS the
/// `size` field itself).
const MB1_E_BASE: u64 = 4;  // u64
const MB1_E_LEN:  u64 = 12; // u64
const MB1_E_TYPE: u64 = 20; // u32

unsafe fn parse_mb1(mb_info: u64) {
    let flags = unsafe { *(mb_info as *const u32) };

    // Try the detailed mmap first (flag bit 6).  QEMU's a.out-kludge MB1 path
    // sets bit 6 but may leave mmap_length = 0; treat that as "no mmap".
    let mut used_mmap = false;
    if flags & (1 << 6) != 0 {
        let mmap_len  = unsafe { *((mb_info + 40) as *const u32) } as u64;
        let mmap_addr = unsafe { *((mb_info + 44) as *const u32) } as u64;

        if mmap_len > 0 {
            let mut offset = 0u64;
            while offset < mmap_len {
                let entry = mmap_addr + offset;
                // `size` does not count itself; total entry size = size + 4.
                let size      = unsafe { *(entry as *const u32) } as u64;
                let base_addr = unsafe { *((entry + MB1_E_BASE) as *const u64) };
                let length    = unsafe { *((entry + MB1_E_LEN)  as *const u64) };
                let typ       = unsafe { *((entry + MB1_E_TYPE) as *const u32) };

                if typ == 1 {
                    unsafe { mark_range_free(base_addr, base_addr + length) };
                }

                offset += size + 4;
            }
            used_mmap = true;
        }
    }

    // Fall back to mem_lower / mem_upper (flag bit 0) if the mmap was absent
    // or empty.  mem_lower is conventional memory (< 1 MiB); mem_upper is
    // extended memory starting at 1 MiB — both reported in KiB.
    if !used_mmap && flags & 1 != 0 {
        let mem_lower_kb = unsafe { *((mb_info + 4) as *const u32) } as u64;
        let mem_upper_kb = unsafe { *((mb_info + 8) as *const u32) } as u64;

        // Conventional memory: 0x1000..mem_lower*1024 (skip frame 0).
        if mem_lower_kb > 0 {
            unsafe { mark_range_free(0x1000, mem_lower_kb * 1024) };
        }
        // Extended memory: 1 MiB..1 MiB + mem_upper*1024.
        if mem_upper_kb > 0 {
            let base = 0x10_0000u64;
            unsafe { mark_range_free(base, base + mem_upper_kb * 1024) };
        }
    }
}

// ── Multiboot2 parser ────────────────────────────────────────────────────────

const MB2_TAG_MMAP: u32 = 6;
const MB2_TAG_END:  u32 = 0;

unsafe fn parse_mb2(mb_info: u64) {
    let total_size = unsafe { *(mb_info as *const u32) } as u64;
    let info_end   = mb_info + total_size;
    let mut tag_addr = mb_info + 8; // skip total_size + reserved

    loop {
        if tag_addr + 8 > info_end {
            break;
        }
        let tag_type = unsafe { *(tag_addr as *const u32) };
        let tag_size = unsafe { *((tag_addr + 4) as *const u32) } as u64;

        if tag_type == MB2_TAG_END {
            break;
        }

        if tag_type == MB2_TAG_MMAP {
            let entry_size    = unsafe { *((tag_addr + 8) as *const u32) } as u64;
            // entry_version at tag_addr + 12 (ignored — always 0).
            let entries_start = tag_addr + 16;
            let entries_end   = tag_addr + tag_size;

            let mut e = entries_start;
            while e + entry_size <= entries_end {
                let base_addr = unsafe { *(e as *const u64) };
                let length    = unsafe { *((e + 8) as *const u64) };
                let typ       = unsafe { *((e + 16) as *const u32) };

                if typ == 1 {
                    unsafe { mark_range_free(base_addr, base_addr + length) };
                }

                e += entry_size;
            }
            break; // memory map found; done
        }

        // Advance to next tag, 8-byte aligned.
        tag_addr += (tag_size + 7) & !7;
    }
}

// ── Public API ───────────────────────────────────────────────────────────────

/// Initialise the PMM from the Multiboot info pointer.
///
/// Must be called exactly once, before any `alloc_frame` / `free_frame`.
pub fn init(mb_magic: u32, mb_info: u64) {
    // 1. Mark all frames used.
    unsafe {
        let p = ptr::addr_of_mut!(BITMAP) as *mut u64;
        for i in 0..BITMAP_WORDS {
            *p.add(i) = !0u64;
        }
    }

    // 2. Mark available RAM from the bootloader memory map.
    match mb_magic {
        MB1_MAGIC => unsafe { parse_mb1(mb_info) },
        MB2_MAGIC => unsafe { parse_mb2(mb_info) },
        other => panic!("pmm::init: unknown bootloader magic {:#010x}", other),
    }

    // 3. Re-mark the kernel image as used (it sits inside available RAM).
    unsafe extern "C" {
        static KERNEL_START: u8;
        static KERNEL_END:   u8;
    }
    let kstart = &raw const KERNEL_START as u64;
    let kend   = &raw const KERNEL_END   as u64;
    unsafe { mark_range_used(kstart, kend) };

    // 4. Always reserve physical frame 0 (BIOS data area / real-mode IVT).
    unsafe { set_used(0) };

    // 5. Reserve the lythd ELF region.
    //    QEMU's a.out-kludge MB1 path does not populate the MB1 modules list,
    //    so lythd is loaded by `run.sh` via:
    //      -device loader,file=lythd,addr=LYTHD_MODULE_ADDR,force-raw=on
    //    This writes raw bytes to a fixed physical address before the CPU
    //    starts.  Marking those frames as used here prevents the PMM from
    //    handing them out before kmain has copied the ELF to the heap.
    unsafe { mark_range_used(LYTHD_MODULE_ADDR, LYTHD_MODULE_ADDR + LYTHD_MODULE_MAX as u64) };

    // 6. Count free frames.
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
