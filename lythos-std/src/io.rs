/// Console output via `SYS_LOG`.
///
/// `sys_log` is the primitive; `print!` / `println!` / `eprintln!` format
/// into a 512-byte stack buffer and flush it in one syscall.
///
/// There is no real stdout/stderr distinction on lythos; `eprintln!` and
/// `println!` both write to the kernel serial console.

use crate::syscall::{SYS_LOG, syscall2};

/// Write a UTF-8 string slice to the kernel serial console via `SYS_LOG`.
///
/// Strings longer than 4096 bytes are silently truncated to the kernel limit.
#[inline]
pub fn sys_log(s: &str) {
    if s.is_empty() { return; }
    let len = s.len().min(4096);
    unsafe { syscall2(SYS_LOG, s.as_ptr() as u64, len as u64); }
}

// ── Internal fmt writer ───────────────────────────────────────────────────────

/// Stack-allocated write buffer.  Flushed to `sys_log` on drop.
pub struct LogWriter {
    buf: [u8; 512],
    pos: usize,
}

impl LogWriter {
    pub fn new() -> Self { Self { buf: [0; 512], pos: 0 } }

    pub fn flush(&self) {
        if self.pos > 0 {
            // SAFETY: we only ever write valid UTF-8 through write_str.
            let s = unsafe { core::str::from_utf8_unchecked(&self.buf[..self.pos]) };
            sys_log(s);
        }
    }
}

impl core::fmt::Write for LogWriter {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let bytes = s.as_bytes();
        let space = self.buf.len() - self.pos;
        let n = bytes.len().min(space);
        self.buf[self.pos..self.pos + n].copy_from_slice(&bytes[..n]);
        self.pos += n;
        Ok(())
    }
}

/// Internal helper called by the `print!` macro.
#[doc(hidden)]
pub fn _print(args: core::fmt::Arguments<'_>) {
    use core::fmt::Write;
    let mut w = LogWriter::new();
    let _ = w.write_fmt(args);
    w.flush();
}

// ── Public macros ─────────────────────────────────────────────────────────────

#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => {
        $crate::io::_print(core::format_args!($($arg)*))
    };
}

#[macro_export]
macro_rules! println {
    ()              => { $crate::print!("\n") };
    ($($arg:tt)*)   => { $crate::print!("{}\n", core::format_args!($($arg)*)) };
}

/// Identical to `println!` — lythos has no separate stderr.
#[macro_export]
macro_rules! eprintln {
    ()              => { $crate::print!("\n") };
    ($($arg:tt)*)   => { $crate::print!("{}\n", core::format_args!($($arg)*)) };
}
