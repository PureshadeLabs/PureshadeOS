//! **lythos-unwind** — no-op Itanium C++ ABI unwinder for Lythos.
//!
//! Lythos uses `panic = "abort"` so genuine stack unwinding never occurs.
//! However, the Rust compiler and some crates link against `_Unwind_*` and
//! `__rust_*` symbols from `libunwind` / `libgcc_s`.  This crate provides
//! stub implementations so the linker succeeds.
//!
//! ## Implementation notes
//!
//! - Every function either loops forever (for `_Unwind_Resume`, which should
//!   never be called with abort-panic) or returns an "invalid" sentinel.
//! - `__rust_begin_short_backtrace` / `__rust_end_short_backtrace` are
//!   pass-through wrappers used by the panic runtime; they are safe to leave
//!   as thin shims.

#![no_std]
#![allow(non_snake_case, unused_variables)]

// ── Itanium C++ ABI unwind types ─────────────────────────────────────────────

#[repr(C)]
pub enum _Unwind_Reason_Code {
    NoReason        = 0,
    ForeignException = 1,
    FatalPhase2Error = 2,
    FatalPhase1Error = 3,
    NormalStop      = 4,
    EndOfStack      = 5,
    HandlerFound    = 6,
    InstallContext  = 7,
    ContinueUnwind  = 8,
}

pub type _Unwind_Exception_Class = u64;
pub type _Unwind_Word            = usize;
pub type _Unwind_Ptr             = usize;
pub type _Unwind_Action          = i32;

#[repr(C)]
pub struct _Unwind_Exception {
    pub exception_class:   _Unwind_Exception_Class,
    pub exception_cleanup: Option<extern "C" fn(_Unwind_Reason_Code, *mut _Unwind_Exception)>,
    pub private1:          _Unwind_Word,
    pub private2:          _Unwind_Word,
}

#[repr(C)]
pub struct _Unwind_Context {
    _opaque: [u8; 0],
}

// ── _Unwind_* stubs ───────────────────────────────────────────────────────────

#[no_mangle]
pub extern "C" fn _Unwind_Resume(exception: *mut _Unwind_Exception) -> ! {
    // With panic=abort this should never be called.
    loop { core::hint::spin_loop(); }
}

#[no_mangle]
pub extern "C" fn _Unwind_RaiseException(
    exception: *mut _Unwind_Exception,
) -> _Unwind_Reason_Code {
    _Unwind_Reason_Code::FatalPhase1Error
}

#[no_mangle]
pub extern "C" fn _Unwind_DeleteException(exception: *mut _Unwind_Exception) {}

#[no_mangle]
pub extern "C" fn _Unwind_GetLanguageSpecificData(ctx: *mut _Unwind_Context) -> _Unwind_Ptr { 0 }

#[no_mangle]
pub extern "C" fn _Unwind_GetRegionStart(ctx: *mut _Unwind_Context) -> _Unwind_Ptr { 0 }

#[no_mangle]
pub extern "C" fn _Unwind_GetTextRelBase(ctx: *mut _Unwind_Context) -> _Unwind_Ptr { 0 }

#[no_mangle]
pub extern "C" fn _Unwind_GetDataRelBase(ctx: *mut _Unwind_Context) -> _Unwind_Ptr { 0 }

#[no_mangle]
pub extern "C" fn _Unwind_GetGR(ctx: *mut _Unwind_Context, index: i32) -> _Unwind_Word { 0 }

#[no_mangle]
pub extern "C" fn _Unwind_SetGR(ctx: *mut _Unwind_Context, index: i32, val: _Unwind_Word) {}

#[no_mangle]
pub extern "C" fn _Unwind_GetIP(ctx: *mut _Unwind_Context) -> _Unwind_Word { 0 }

#[no_mangle]
pub extern "C" fn _Unwind_SetIP(ctx: *mut _Unwind_Context, val: _Unwind_Word) {}

#[no_mangle]
pub extern "C" fn _Unwind_GetIPInfo(ctx: *mut _Unwind_Context, ip_before_insn: *mut i32) -> _Unwind_Word {
    if !ip_before_insn.is_null() { unsafe { *ip_before_insn = 0; } }
    0
}

#[no_mangle]
pub extern "C" fn _Unwind_Backtrace(
    _trace: extern "C" fn(*mut _Unwind_Context, *mut core::ffi::c_void) -> _Unwind_Reason_Code,
    _trace_argument: *mut core::ffi::c_void,
) -> _Unwind_Reason_Code {
    _Unwind_Reason_Code::EndOfStack
}

// ── __rust_* hooks ────────────────────────────────────────────────────────────

#[no_mangle]
pub extern "C" fn __rust_begin_short_backtrace<F: FnOnce() -> R, R>(f: F) -> R {
    let result = f();
    core::hint::black_box(());
    result
}

#[no_mangle]
pub extern "C" fn __rust_end_short_backtrace<F: FnOnce() -> R, R>(f: F) -> R {
    let result = f();
    core::hint::black_box(());
    result
}

// ── Panic handler ─────────────────────────────────────────────────────────────

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    // SYS_TASK_EXIT = 1
    unsafe {
        core::arch::asm!(
            "syscall",
            in("rax") 1u64,
            options(noreturn, nostack),
        );
    }
}
