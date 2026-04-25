//! Kernel heap allocator — free-list with address-ordered coalescing.
//!
//! Backed by a 4 MiB static buffer in BSS (zeroed by the cask ELF loader).
//! All blocks and payloads are 16-byte aligned.  Supports full alloc/dealloc
//! with both forward and backward coalescing to prevent fragmentation.
//!
//! This module registers itself as the `#[global_allocator]`, so any crate
//! that links `cask-std` automatically gets heap allocation.

#![allow(static_mut_refs)]

use core::{
    alloc::{GlobalAlloc, Layout},
    ptr,
    sync::atomic::{AtomicBool, Ordering},
};

// ── Constants ────────────────────────────────────────────────────────────────

/// Total heap size.
pub const HEAP_SIZE: usize = 4 * 1024 * 1024; // 4 MiB

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

// ── GlobalAlloc ───────────────────────────────────────────────────────────────

pub struct CaskAllocator;

// SAFETY: cask userspace is single-threaded; no true concurrent access.
unsafe impl Sync for CaskAllocator {}
unsafe impl Send for CaskAllocator {}

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

        ptr::null_mut() // OOM
    }

    unsafe fn dealloc(&self, ptr: *mut u8, _layout: Layout) {
        // Recover the block header from the user pointer.
        let block = unsafe { ptr.sub(HDR) as *mut FreeBlock };

        let head_ptr = core::ptr::addr_of_mut!(HEAD);
        let mut pprev: *mut FreeBlock = ptr::null_mut();
        let mut curr  = unsafe { *head_ptr };

        // Walk the free list to find the address-ordered insertion point.
        while !curr.is_null() && (curr as usize) < (block as usize) {
            pprev = curr;
            curr  = unsafe { (*curr).next };
        }

        // Insert `block` between pprev and curr.
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
}

#[global_allocator]
pub static ALLOCATOR: CaskAllocator = CaskAllocator;
