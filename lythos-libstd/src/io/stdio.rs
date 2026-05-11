//! Standard I/O streams backed by Lythos syscalls.
//!
//! - stdout / stderr → SYS_LOG (kernel serial log)
//! - stdin           → SYS_SERIAL_READ

use super::{Read, Write, Result, ErrorKind, Error};
use crate::sys::io_impl;

// ── Stdout ────────────────────────────────────────────────────────────────────

pub struct Stdout(());
pub struct StdoutLock<'a>(&'a Stdout);

pub fn stdout() -> Stdout { Stdout(()) }

impl Stdout {
    pub fn lock(&self) -> StdoutLock<'_> { StdoutLock(self) }
    pub fn write_all(&self, buf: &[u8]) -> Result<()> {
        io_impl::log_write(buf).map(|_| ()).map_err(|_| Error::new(ErrorKind::Other, "log write failed"))
    }
}

impl Write for Stdout {
    fn write(&mut self, buf: &[u8]) -> Result<usize> {
        io_impl::log_write(buf).map_err(|_| Error::new(ErrorKind::Other, "log write failed"))
    }
    fn flush(&mut self) -> Result<()> { Ok(()) }
}

impl<'a> Write for StdoutLock<'a> {
    fn write(&mut self, buf: &[u8]) -> Result<usize> {
        io_impl::log_write(buf).map_err(|_| Error::new(ErrorKind::Other, "log write failed"))
    }
    fn flush(&mut self) -> Result<()> { Ok(()) }
}

// ── Stderr ────────────────────────────────────────────────────────────────────

pub struct Stderr(());
pub struct StderrLock<'a>(&'a Stderr);

pub fn stderr() -> Stderr { Stderr(()) }

impl Stderr {
    pub fn lock(&self) -> StderrLock<'_> { StderrLock(self) }
}

impl Write for Stderr {
    fn write(&mut self, buf: &[u8]) -> Result<usize> {
        io_impl::log_write(buf).map_err(|_| Error::new(ErrorKind::Other, "log write failed"))
    }
    fn flush(&mut self) -> Result<()> { Ok(()) }
}

impl<'a> Write for StderrLock<'a> {
    fn write(&mut self, buf: &[u8]) -> Result<usize> {
        io_impl::log_write(buf).map_err(|_| Error::new(ErrorKind::Other, "log write failed"))
    }
    fn flush(&mut self) -> Result<()> { Ok(()) }
}

// ── Stdin ─────────────────────────────────────────────────────────────────────

pub struct Stdin(());
pub struct StdinLock<'a>(&'a Stdin);

pub fn stdin() -> Stdin { Stdin(()) }

impl Stdin {
    pub fn lock(&self) -> StdinLock<'_> { StdinLock(self) }

    pub fn read_line(&self, buf: &mut _alloc::string::String) -> Result<usize> {
        let mut tmp = [0u8; 1];
        let mut n = 0;
        loop {
            let r = io_impl::serial_read(&mut tmp)
                .map_err(|_| Error::new(ErrorKind::Other, "serial read failed"))?;
            if r == 0 { break; }
            let ch = tmp[0];
            buf.push(ch as char);
            n += 1;
            if ch == b'\n' { break; }
        }
        Ok(n)
    }
}

impl Read for Stdin {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        io_impl::serial_read(buf).map_err(|_| Error::new(ErrorKind::Other, "serial read failed"))
    }
}

impl<'a> Read for StdinLock<'a> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        io_impl::serial_read(buf).map_err(|_| Error::new(ErrorKind::Other, "serial read failed"))
    }
}

// ── print! / println! / eprint! / eprintln! ───────────────────────────────────

#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => {{
        use $crate::io::Write as _;
        let _ = core::write!($crate::io::stdout(), $($arg)*);
    }};
}

#[macro_export]
macro_rules! println {
    () => { $crate::print!("\n") };
    ($($arg:tt)*) => {{
        use $crate::io::Write as _;
        let _ = core::writeln!($crate::io::stdout(), $($arg)*);
    }};
}

#[macro_export]
macro_rules! eprint {
    ($($arg:tt)*) => {{
        use $crate::io::Write as _;
        let _ = core::write!($crate::io::stderr(), $($arg)*);
    }};
}

#[macro_export]
macro_rules! eprintln {
    () => { $crate::eprint!("\n") };
    ($($arg:tt)*) => {{
        use $crate::io::Write as _;
        let _ = core::writeln!($crate::io::stderr(), $($arg)*);
    }};
}
