//! Command-line arguments — the argv the kernel wrote onto the initial stack.
//!
//! `SYS_EXEC` places a SysV-style frame at the new task's initial `rsp`
//! (docs/spec/syscalls.md SYS_EXEC):
//!
//! ```text
//! rsp+0              argc
//! rsp+8 ..           argv[0..argc-1]   (pointers into the string area)
//! + 8                NULL              (argv terminator)
//! + 8                NULL              (envp terminator — Lythos has no env)
//! + 32               auxv: AT_PAGESZ=4096, AT_NULL
//! then               "argv[0]\0argv[1]\0…"
//! ```
//!
//! The strings live at the very top of the task's own stack. The stack only
//! grows *down* from the initial `rsp`, so they stay valid for the task's
//! lifetime — `&'static str` is sound.
//!
//! Capturing the frame requires the initial `rsp`, which only the first
//! instruction of `_start` sees. Use the [`entry!`](crate::entry) macro to
//! generate that `_start`; a binary with a hand-written `_start` never calls
//! [`init`] and sees an empty argv (argc = 0), same as before argv existed.

static mut ARGC: usize = 0;
static mut ARGV: *const u64 = core::ptr::null();

/// Record the initial stack pointer. Called exactly once, before `main`, by
/// the shim `entry!` generates. Not for user code.
///
/// # Safety
/// `rsp` must be the untouched initial stack pointer of this task, pointing
/// at the kernel-written argc slot.
pub unsafe fn init(rsp: *const u64) {
    // SAFETY: single-threaded task, called once before main; the frame
    // layout is the SYS_EXEC ABI contract.
    unsafe {
        ARGC = *rsp as usize;
        ARGV = rsp.add(1);
    }
}

/// Number of arguments (0 when the spawner passed no argv, or when the
/// binary's `_start` never captured the frame).
pub fn argc() -> usize {
    unsafe { ARGC }
}

/// The `i`-th argument, if present and valid UTF-8.
pub fn arg(i: usize) -> Option<&'static str> {
    unsafe {
        if i >= ARGC || ARGV.is_null() {
            return None;
        }
        let p = *ARGV.add(i) as *const u8;
        if p.is_null() {
            return None;
        }
        let mut len = 0usize;
        while *p.add(len) != 0 {
            len += 1;
        }
        core::str::from_utf8(core::slice::from_raw_parts(p, len)).ok()
    }
}

/// Iterator over the arguments, `argv[0]` first.
pub fn args() -> Args {
    Args { idx: 0 }
}

pub struct Args {
    idx: usize,
}

impl Iterator for Args {
    type Item = &'static str;
    fn next(&mut self) -> Option<&'static str> {
        let a = arg(self.idx)?;
        self.idx += 1;
        Some(a)
    }
}

impl ExactSizeIterator for Args {
    fn len(&self) -> usize {
        argc().saturating_sub(self.idx)
    }
}

/// Generate a `_start` that captures the initial `rsp` (making
/// [`args`](crate::args) work), calls your `fn main()`, then exits the task.
///
/// ```rust,ignore
/// #![no_std]
/// #![no_main]
/// lythos_rt::entry!(main);
///
/// fn main() {
///     for a in lythos_rt::args::args() { lythos_rt::println!("{a}"); }
/// }
/// ```
///
/// The panic handler is still the binary's to provide.
#[macro_export]
macro_rules! entry {
    ($main:path) => {
        #[unsafe(no_mangle)]
        #[unsafe(naked)]
        pub extern "C" fn _start() -> ! {
            // rsp points at the kernel-written argc slot and is 16-byte
            // aligned; `call` leaves the shim with the standard SysV
            // rsp % 16 == 8 at entry. The frame stays intact above rsp.
            ::core::arch::naked_asm!(
                "mov rdi, rsp",
                "call {shim}",
                "ud2",
                shim = sym __lythos_entry_shim,
            )
        }

        #[unsafe(no_mangle)]
        extern "C" fn __lythos_entry_shim(rsp: *const u64) -> ! {
            unsafe { $crate::args::init(rsp) };
            let main_fn: fn() = $main;
            main_fn();
            $crate::sys_task_exit()
        }
    };
}
