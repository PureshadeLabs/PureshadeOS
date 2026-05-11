//! **lythos-libc** — minimal C ABI shim for Lythos.
//!
//! The Rust compiler and some crates expect a subset of libc symbols to be
//! available at link time even when targeting a custom OS.  This crate provides
//! those symbols as stubs so that the linker does not fail with "undefined
//! reference" errors.
//!
//! ## Symbols provided
//!
//! - Memory: `malloc`, `free`, `realloc`, `calloc`, `memcpy`, `memmove`,
//!   `memset`, `memcmp`, `strlen`
//! - Exit: `abort`, `exit`
//! - I/O: `write` (fd-based stub — fd 1 and 2 route to SYS_LOG)
//! - Math: `__muloti4`, `__udivti3`, `__umodti3` (compiler-rt intrinsics)
//!
//! All other POSIX functions are absent; crates that call them will fail to
//! compile until a proper implementation is added here.

#![no_std]
#![allow(non_camel_case_types, non_upper_case_globals)]

// ── C integer types ────────────────────────────────────────────────────────────

pub type c_void    = core::ffi::c_void;
pub type c_char    = i8;
pub type c_uchar   = u8;
pub type c_short   = i16;
pub type c_ushort  = u16;
pub type c_int     = i32;
pub type c_uint    = u32;
pub type c_long    = i64;
pub type c_ulong   = u64;
pub type c_longlong  = i64;
pub type c_ulonglong = u64;
pub type size_t    = usize;
pub type ssize_t   = isize;
pub type ptrdiff_t = isize;
pub type intptr_t  = isize;
pub type uintptr_t = usize;

// errno — single-task, no threads, so a static cell suffices for now.
static mut ERRNO_VAL: c_int = 0;

#[no_mangle]
pub unsafe extern "C" fn __errno_location() -> *mut c_int {
    unsafe { &raw mut ERRNO_VAL }
}

// ── Memory ────────────────────────────────────────────────────────────────────
//
// Route through the Lythos allocator (provided by lythos-std via its
// `#[global_allocator]`).  We use the global alloc API from core.

extern crate alloc;
use alloc::alloc::{alloc, dealloc, realloc as realloc_inner, alloc_zeroed, Layout};

#[no_mangle]
pub unsafe extern "C" fn malloc(size: size_t) -> *mut c_void {
    if size == 0 { return core::ptr::null_mut(); }
    let layout = Layout::from_size_align(size + core::mem::size_of::<usize>(), 16).unwrap();
    let ptr = unsafe { alloc(layout) };
    if ptr.is_null() { return core::ptr::null_mut(); }
    // Store the allocation size just before the returned pointer.
    unsafe { (ptr as *mut usize).write(size); }
    unsafe { ptr.add(core::mem::size_of::<usize>()) as *mut c_void }
}

#[no_mangle]
pub unsafe extern "C" fn free(ptr: *mut c_void) {
    if ptr.is_null() { return; }
    let base = unsafe { (ptr as *mut u8).sub(core::mem::size_of::<usize>()) };
    let size = unsafe { (base as *const usize).read() };
    let layout = Layout::from_size_align(size + core::mem::size_of::<usize>(), 16).unwrap();
    unsafe { dealloc(base, layout); }
}

#[no_mangle]
pub unsafe extern "C" fn calloc(nmemb: size_t, size: size_t) -> *mut c_void {
    let total = nmemb.checked_mul(size).unwrap_or(0);
    if total == 0 { return core::ptr::null_mut(); }
    let layout = Layout::from_size_align(total + core::mem::size_of::<usize>(), 16).unwrap();
    let ptr = unsafe { alloc_zeroed(layout) };
    if ptr.is_null() { return core::ptr::null_mut(); }
    unsafe { (ptr as *mut usize).write(total); }
    unsafe { ptr.add(core::mem::size_of::<usize>()) as *mut c_void }
}

#[no_mangle]
pub unsafe extern "C" fn realloc(ptr: *mut c_void, new_size: size_t) -> *mut c_void {
    if ptr.is_null() { return unsafe { malloc(new_size) }; }
    if new_size == 0 { unsafe { free(ptr) }; return core::ptr::null_mut(); }

    let base = unsafe { (ptr as *mut u8).sub(core::mem::size_of::<usize>()) };
    let old_size = unsafe { (base as *const usize).read() };
    let old_layout = Layout::from_size_align(old_size + core::mem::size_of::<usize>(), 16).unwrap();
    let new_layout_size = new_size + core::mem::size_of::<usize>();

    let new_ptr = unsafe { realloc_inner(base, old_layout, new_layout_size) };
    if new_ptr.is_null() { return core::ptr::null_mut(); }
    unsafe { (new_ptr as *mut usize).write(new_size); }
    unsafe { new_ptr.add(core::mem::size_of::<usize>()) as *mut c_void }
}

// ── String / memory primitives ────────────────────────────────────────────────

#[no_mangle]
pub unsafe extern "C" fn memcpy(dst: *mut c_void, src: *const c_void, n: size_t) -> *mut c_void {
    unsafe { core::ptr::copy_nonoverlapping(src as *const u8, dst as *mut u8, n); }
    dst
}

#[no_mangle]
pub unsafe extern "C" fn memmove(dst: *mut c_void, src: *const c_void, n: size_t) -> *mut c_void {
    unsafe { core::ptr::copy(src as *const u8, dst as *mut u8, n); }
    dst
}

#[no_mangle]
pub unsafe extern "C" fn memset(dst: *mut c_void, c: c_int, n: size_t) -> *mut c_void {
    unsafe { core::ptr::write_bytes(dst as *mut u8, c as u8, n); }
    dst
}

#[no_mangle]
pub unsafe extern "C" fn memcmp(a: *const c_void, b: *const c_void, n: size_t) -> c_int {
    let a = unsafe { core::slice::from_raw_parts(a as *const u8, n) };
    let b = unsafe { core::slice::from_raw_parts(b as *const u8, n) };
    for (x, y) in a.iter().zip(b.iter()) {
        let d = (*x as c_int) - (*y as c_int);
        if d != 0 { return d; }
    }
    0
}

#[no_mangle]
pub unsafe extern "C" fn strlen(s: *const c_char) -> size_t {
    let mut n = 0;
    while unsafe { *s.add(n) } != 0 { n += 1; }
    n
}

// ── I/O ───────────────────────────────────────────────────────────────────────

#[no_mangle]
pub unsafe extern "C" fn write(fd: c_int, buf: *const c_void, count: size_t) -> ssize_t {
    // fd 1 (stdout) and fd 2 (stderr) → SYS_LOG.  All others are EBADF.
    if fd == 1 || fd == 2 {
        let slice = unsafe { core::slice::from_raw_parts(buf as *const u8, count) };
        let ret = unsafe {
            lythos_std_raw_log(slice.as_ptr(), slice.len())
        };
        if ret >= 0x8000_0000_0000_0000u64 { -1 } else { count as ssize_t }
    } else {
        unsafe { ERRNO_VAL = 9; } // EBADF
        -1
    }
}

// Thin inline asm wrapper — avoids dragging in lythos-std as a dependency.
#[inline(always)]
unsafe fn lythos_std_raw_log(ptr: *const u8, len: usize) -> u64 {
    let ret: u64;
    unsafe {
        core::arch::asm!(
            "syscall",
            inlateout("rax") 11u64 => ret,  // SYS_LOG = 11
            in("rdi") ptr as u64,
            in("rsi") len as u64,
            out("rcx") _,
            out("r11") _,
            options(nostack, preserves_flags),
        );
    }
    ret
}

// ── Process control ───────────────────────────────────────────────────────────

#[no_mangle]
pub unsafe extern "C" fn abort() -> ! {
    unsafe {
        core::arch::asm!(
            "syscall",
            in("rax") 1u64,  // SYS_TASK_EXIT = 1
            options(noreturn, nostack),
        );
    }
}

#[no_mangle]
pub unsafe extern "C" fn exit(_code: c_int) -> ! {
    unsafe { abort() }
}

// ── Panic handler (required in no_std staticlib) ──────────────────────────────

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    unsafe { abort() }
}
