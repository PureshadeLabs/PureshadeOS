//! Userspace heap allocator — free-list with address-ordered coalescing.
//!
//! Two-tier backing:
//!
//! 1. A 64 KiB static bootstrap arena in BSS (zeroed by the lythos ELF
//!    loader).  This is the whole heap for tasks that hold no Memory
//!    capability (SYS_BRK returns ENOPERM for them).
//! 2. A brk region grown on demand in `GROW_CHUNK` steps via SYS_BRK when
//!    the free list cannot satisfy a request.  When the topmost free block
//!    grows past `SHRINK_THRESHOLD`, the tail is returned to the kernel,
//!    keeping `SHRINK_SLACK` bytes as hysteresis so alloc/dealloc churn at
//!    the boundary does not thrash brk syscalls.
//!
//! The previous design was a single 4 MiB static arena — every task paid
//! 4 MiB of eagerly-backed BSS whether it allocated or not.
//!
//! All blocks and payloads are 16-byte aligned.  Free blocks are kept in a
//! single address-ordered list spanning both tiers (the static arena and the
//! brk region are far apart in VA, so cross-tier merges can never happen).
//!
//! This module registers itself as the `#[global_allocator]`, so any crate
//! that links `lythos-std` automatically gets heap allocation.

#![allow(static_mut_refs)]

use core::{
    alloc::{GlobalAlloc, Layout},
    ptr,
    sync::atomic::{AtomicBool, Ordering},
};

// ── Constants ────────────────────────────────────────────────────────────────

/// Static bootstrap arena size.  Covers idle daemons and any task without a
/// Memory capability.
pub const HEAP_SIZE: usize = 64 * 1024;

/// Minimum brk extension per growth request (amortises syscalls).
const GROW_CHUNK: usize = 256 * 1024;

/// Return brk memory to the kernel only when the topmost free block exceeds
/// this many bytes…
const SHRINK_THRESHOLD: usize = 512 * 1024;

/// …and keep this much of it mapped as slack (grow/shrink hysteresis).
const SHRINK_SLACK: usize = 256 * 1024;

/// Block header size — two pointer-sized fields (size + next).
const HDR: usize = core::mem::size_of::<FreeBlock>(); // 16 bytes

/// All blocks are a multiple of ALIGN bytes.
const ALIGN: usize = 16;

/// Minimum block: header + at least one header's worth of payload.
/// A split is only created when the remainder meets this minimum.
const MIN_BLOCK: usize = HDR * 2; // 32 bytes

// ── Block layout ─────────────────────────────────────────────────────────────
//
//  Free block:      [size: usize | next: *mut FreeBlock | ... payload ...]
//  Allocated block: [size: usize | _pad: usize          | ... payload ...]
//
// `size` is the total block length in bytes (header + payload).
// The allocator returns a pointer to the byte immediately after the 16-byte
// header.  `dealloc` subtracts HDR from the user pointer to recover the block.

#[repr(C)]
struct FreeBlock {
    size: usize,
    next: *mut FreeBlock,
}

// ── Backing storage ───────────────────────────────────────────────────────────

#[repr(align(16))]
#[allow(dead_code)]
struct Backing([u8; HEAP_SIZE]);

// SAFETY: single-threaded userspace; all access is guarded by INITED flag
// and the allocator itself (no preemption during alloc/dealloc).
static mut HEAP: Backing = Backing([0u8; HEAP_SIZE]);
static mut HEAD: *mut FreeBlock = ptr::null_mut();
static INITED: AtomicBool = AtomicBool::new(false);

/// Top of the brk region we own (0 = brk never grown).  Tracks the exact
/// value last returned by SYS_BRK so tail-shrink can identify the topmost
/// free block.
static mut BRK_TOP: usize = 0;

// ── Init ─────────────────────────────────────────────────────────────────────

unsafe fn heap_init() {
    // SAFETY: called exactly once, before any other heap access.
    let base = core::ptr::addr_of_mut!(HEAP).cast::<FreeBlock>();
    unsafe {
        (*base).size = HEAP_SIZE;
        (*base).next = ptr::null_mut();
        core::ptr::write(core::ptr::addr_of_mut!(HEAD), base);
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

#[inline(always)]
fn round_up(x: usize, align: usize) -> usize {
    (x + align - 1) & !(align - 1)
}

/// Insert `block` into the address-ordered free list and coalesce with both
/// physical neighbours.  Shared by `dealloc` and the brk growth path.
unsafe fn insert_free_block(block: *mut FreeBlock) {
    let head_ptr = core::ptr::addr_of_mut!(HEAD);
    let mut pprev: *mut FreeBlock = ptr::null_mut();
    let mut curr  = unsafe { *head_ptr };

    while !curr.is_null() && (curr as usize) < (block as usize) {
        pprev = curr;
        curr  = unsafe { (*curr).next };
    }

    unsafe {
        (*block).next = curr;
        if pprev.is_null() { *head_ptr    = block; }
        else               { (*pprev).next = block; }
    }

    // Coalesce forward: merge with `curr` if it immediately follows `block`.
    if !curr.is_null() {
        let block_end = block as usize + unsafe { (*block).size };
        if block_end == curr as usize {
            unsafe {
                (*block).size += (*curr).size;
                (*block).next  = (*curr).next;
            }
        }
    }

    // Coalesce backward: merge `pprev` into `block` if they are adjacent.
    if !pprev.is_null() {
        let pprev_end = pprev as usize + unsafe { (*pprev).size };
        if pprev_end == block as usize {
            unsafe {
                (*pprev).size += (*block).size;
                (*pprev).next  = (*block).next;
            }
        }
    }
}

/// Extend the brk region by at least `need` bytes (rounded up to
/// `GROW_CHUNK`) and add the new range to the free list.  Returns false if
/// the task holds no Memory capability or the kernel is out of frames.
unsafe fn grow_brk(need: usize) -> bool {
    let cur = match crate::sys_brk(0) {
        Ok(b)  => b as usize,
        Err(_) => return false, // no Memory capability
    };
    // The kernel returns a page-aligned base initially; keep our blocks
    // 16-byte aligned regardless.
    let base = round_up(cur, ALIGN);
    let want = round_up(need.max(GROW_CHUNK), ALIGN);
    let target = match base.checked_add(want) {
        Some(t) => t,
        None    => return false,
    };
    let got = match crate::sys_brk(target as u64) {
        Ok(b)  => b as usize,
        Err(_) => return false,
    };
    if got < base + need {
        return false; // partial growth (kernel OOM) — not enough for request
    }
    unsafe {
        core::ptr::write(core::ptr::addr_of_mut!(BRK_TOP), got);
        let blk = base as *mut FreeBlock;
        (*blk).size = got - base;
        (*blk).next = ptr::null_mut();
        insert_free_block(blk);
    }
    true
}

/// If the topmost free block ends exactly at BRK_TOP and exceeds
/// SHRINK_THRESHOLD, return its tail to the kernel, keeping SHRINK_SLACK.
unsafe fn maybe_shrink_brk() {
    let brk_top = unsafe { core::ptr::read(core::ptr::addr_of!(BRK_TOP)) };
    if brk_top == 0 { return; }

    // The topmost block is the last in the address-ordered list.
    let mut curr = unsafe { *core::ptr::addr_of!(HEAD) };
    let mut last: *mut FreeBlock = ptr::null_mut();
    while !curr.is_null() {
        last = curr;
        curr = unsafe { (*curr).next };
    }
    if last.is_null() { return; }

    let (start, size) = (last as usize, unsafe { (*last).size });
    if start + size != brk_top || size < SHRINK_THRESHOLD {
        return;
    }

    let new_top = start + SHRINK_SLACK;
    match crate::sys_brk(new_top as u64) {
        Ok(b) if b as usize == new_top => unsafe {
            (*last).size = SHRINK_SLACK;
            core::ptr::write(core::ptr::addr_of_mut!(BRK_TOP), new_top);
        },
        _ => {} // shrink refused — keep the block as-is
    }
}

// ── GlobalAlloc ───────────────────────────────────────────────────────────────

pub struct CaskAllocator;

// SAFETY: lythos userspace is single-threaded; no true concurrent access.
unsafe impl Sync for CaskAllocator {}
unsafe impl Send for CaskAllocator {}

unsafe fn alloc_from_list(need: usize) -> *mut u8 {
    let head_ptr = core::ptr::addr_of_mut!(HEAD);
    let mut pprev: *mut FreeBlock = ptr::null_mut();
    let mut curr  = unsafe { *head_ptr };

    while !curr.is_null() {
        let sz = unsafe { (*curr).size };

        if sz >= need {
            let rest = sz - need;
            unsafe {
                if rest >= MIN_BLOCK {
                    // Split: carve `need` bytes from the front of `curr`.
                    let split = (curr as *mut u8).add(need) as *mut FreeBlock;
                    (*split).size = rest;
                    (*split).next = (*curr).next;
                    (*curr).size  = need;
                    if pprev.is_null() { *head_ptr    = split; }
                    else               { (*pprev).next = split; }
                } else {
                    // Use the whole block (remainder too small to split).
                    if pprev.is_null() { *head_ptr    = (*curr).next; }
                    else               { (*pprev).next = (*curr).next; }
                }
            }
            // Return pointer past the header.
            return unsafe { (curr as *mut u8).add(HDR) };
        }

        pprev = curr;
        curr  = unsafe { (*curr).next };
    }

    ptr::null_mut()
}

unsafe impl GlobalAlloc for CaskAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // Lazy init on first allocation.
        if !INITED.swap(true, Ordering::Acquire) {
            unsafe { heap_init(); }
        }

        // Block must be large enough for the header plus the requested payload,
        // rounded up so the next block is also properly aligned.
        let payload = round_up(layout.size(), ALIGN).max(HDR);
        let need    = HDR + payload;

        let p = unsafe { alloc_from_list(need) };
        if !p.is_null() {
            return p;
        }

        // Free list exhausted — extend the brk region and retry once.
        if unsafe { grow_brk(need) } {
            return unsafe { alloc_from_list(need) };
        }

        ptr::null_mut() // OOM
    }

    unsafe fn dealloc(&self, ptr: *mut u8, _layout: Layout) {
        // Recover the block header from the user pointer.
        let block = unsafe { ptr.sub(HDR) as *mut FreeBlock };
        unsafe {
            insert_free_block(block);
            maybe_shrink_brk();
        }
    }
}

#[global_allocator]
pub static ALLOCATOR: CaskAllocator = CaskAllocator;
