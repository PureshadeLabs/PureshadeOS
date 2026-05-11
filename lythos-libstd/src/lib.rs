//! **lythos-libstd** — Rust standard-library port for the Lythos microkernel.
//!
//! This crate exposes a `std`-compatible API surface so programs written against
//! the standard library can be compiled for the Lythos ABI with minimal changes.
//!
//! ## What is implemented
//!
//! | Module | Status | Notes |
//! |--------|--------|-------|
//! | `alloc` | ✓ | `SYS_MMAP`-backed bump+free-list allocator |
//! | `collections` | ✓ | Thin `alloc` re-exports; HashMap via hashbrown |
//! | `env` | partial | no env vars; exposes boot-info via `lythos_boot_info()` |
//! | `error` | ✓ | `std::error::Error` trait + blanket impls |
//! | `ffi` | partial | `CStr`, `CString`; `OsStr`/`OsString` as UTF-8 |
//! | `fmt` | ✓ | re-exports `core::fmt` |
//! | `fs` | stub | all ops return `ErrorKind::Unsupported` |
//! | `io` | ✓ | `Read`, `Write`, `BufReader`, `BufWriter`, `Cursor`, stdio |
//! | `net` | stub | all ops return `ErrorKind::Unsupported` |
//! | `os::lythos` | ✓ | platform raw types and boot-info access |
//! | `path` | ✓ | pure `Path`/`PathBuf` (no FS syscalls) |
//! | `process` | ✓ | `exit`, `abort`, `Command` stub |
//! | `sync` | ✓ | `Mutex`, `RwLock`, `OnceLock`, `Arc`, `Weak`, `Condvar` |
//! | `thread` | partial | `spawn` via `SYS_EXEC`, `yield_now`, `sleep` stub |
//! | `time` | ✓ | `Duration`, `Instant` via `SYS_TIME` |
//!
//! ## Usage
//!
//! In your crate root add:
//!
//! ```rust,ignore
//! #![no_std]
//! #![no_main]
//! extern crate lythos_libstd as std;
//!
//! use std::io::{self, Write};
//! use std::sync::Mutex;
//!
//! #[unsafe(no_mangle)]
//! pub extern "C" fn _start() -> ! {
//!     let stdout = io::stdout();
//!     writeln!(&mut stdout.lock(), "hello from lythos std!").ok();
//!     std::process::exit(0)
//! }
//! ```

#![no_std]
#![allow(clippy::module_inception)]

extern crate alloc as _alloc;
extern crate lythos_std;

// ── Allocator ─────────────────────────────────────────────────────────────────
//
// Re-use the global allocator already registered by lythos-std.
// Programs only need to link lythos-libstd; the global allocator is set up
// transparently via lythos-std's `#[global_allocator]`.

// ── Re-export core/alloc fundamentals ────────────────────────────────────────

pub use core::{
    clone, cmp, convert, default, hint, iter, marker, mem, num, ops, option,
    result, slice, str, u8, i8, u16, i16, u32, i32, u64, i64, u128, i128,
    usize, isize, f32, f64, ptr, borrow, array,
};

// ── Public modules ────────────────────────────────────────────────────────────

pub mod collections;
pub mod env;
pub mod error;
pub mod ffi;
pub mod fmt;
pub mod fs;
pub mod io;
pub mod net;
pub mod os;
pub mod path;
pub mod process;
pub mod sync;
pub mod thread;
pub mod time;

// ── alloc surface ─────────────────────────────────────────────────────────────

pub use _alloc::{boxed, rc, string, vec};
pub use _alloc::string::String;
pub use _alloc::vec::Vec;
pub use _alloc::boxed::Box;
pub use _alloc::sync::Arc;

// ── Prelude ───────────────────────────────────────────────────────────────────

pub mod prelude {
    pub mod v1 {
        pub use core::prelude::v1::*;
        pub use _alloc::{
            boxed::Box,
            format,
            string::{String, ToString},
            vec,
            vec::Vec,
        };
        pub use crate::io::{Read, Write};
    }
}

// Internal PAL (platform abstraction layer) — not public API.
mod sys;
