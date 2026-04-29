//! I/O abstractions — `Read`, `Write`, standard streams, and buffered wrappers.
//!
//! Mirrors `std::io` for lythos userspace.  Output goes through `SYS_LOG` to
//! the kernel serial console; there is no stdin or file I/O yet.

use alloc::{string::String, vec::Vec};

// ── Error ─────────────────────────────────────────────────────────────────────

/// Categories of I/O error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    NotFound,
    PermissionDenied,
    ConnectionRefused,
    ConnectionReset,
    ConnectionAborted,
    BrokenPipe,
    AlreadyExists,
    WouldBlock,
    InvalidInput,
    InvalidData,
    TimedOut,
    WriteZero,
    Interrupted,
    Unsupported,
    UnexpectedEof,
    OutOfMemory,
    Other,
}

/// An I/O error, with a coarse category and an optional static description.
pub struct Error {
    kind: ErrorKind,
    msg:  &'static str,
}

impl Error {
    pub const fn new(kind: ErrorKind) -> Self {
        Error { kind, msg: "" }
    }

    pub const fn with_msg(kind: ErrorKind, msg: &'static str) -> Self {
        Error { kind, msg }
    }

    pub fn kind(&self) -> ErrorKind { self.kind }

    /// Convert a lythos kernel `SysError` into an `io::Error`.
    pub fn from_kernel(e: crate::SysError) -> Self {
        match e {
            crate::SysError::NoPerm  => Error::new(ErrorKind::PermissionDenied),
            crate::SysError::Inval   => Error::new(ErrorKind::InvalidInput),
            crate::SysError::NoCap   => Error::new(ErrorKind::PermissionDenied),
            crate::SysError::NoSys   => Error::with_msg(ErrorKind::Unsupported, "ENOSYS"),
            _                         => Error::new(ErrorKind::Other),
        }
    }

    // Common pre-built errors (avoids allocation).
    pub const UNEXPECTED_EOF: Error = Error::with_msg(ErrorKind::UnexpectedEof, "unexpected EOF");
    pub const WRITE_ZERO:     Error = Error::with_msg(ErrorKind::WriteZero,     "write returned 0");
    pub const INVALID_DATA:   Error = Error::with_msg(ErrorKind::InvalidData,   "invalid data");
    pub const UNSUPPORTED:    Error = Error::with_msg(ErrorKind::Unsupported,   "unsupported");
    pub const OTHER:          Error = Error::new(ErrorKind::Other);
}

impl core::fmt::Debug for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        if self.msg.is_empty() {
            write!(f, "io::Error({:?})", self.kind)
        } else {
            write!(f, "io::Error({:?}: {})", self.kind, self.msg)
        }
    }
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        if self.msg.is_empty() {
            write!(f, "{:?}", self.kind)
        } else {
            f.write_str(self.msg)
        }
    }
}

/// Alias for `Result<T, io::Error>`.
pub type Result<T> = core::result::Result<T, Error>;

// ── Read ──────────────────────────────────────────────────────────────────────

/// The `Read` trait for byte sources.
pub trait Read {
    /// Pull bytes into `buf`, returning how many were read. `Ok(0)` = EOF.
    fn read(&mut self, buf: &mut [u8]) -> Result<usize>;

    /// Read the exact number of bytes required to fill `buf`.
    fn read_exact(&mut self, mut buf: &mut [u8]) -> Result<()> {
        while !buf.is_empty() {
            match self.read(buf) {
                Ok(0)  => return Err(Error::UNEXPECTED_EOF),
                Ok(n)  => buf = &mut buf[n..],
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    /// Read all bytes until EOF, appending into `buf`.
    fn read_to_end(&mut self, buf: &mut Vec<u8>) -> Result<usize> {
        let start = buf.len();
        let mut tmp = [0u8; 512];
        loop {
            match self.read(&mut tmp) {
                Ok(0) => break,
                Ok(n) => buf.extend_from_slice(&tmp[..n]),
                Err(e) => return Err(e),
            }
        }
        Ok(buf.len() - start)
    }

    /// Read all bytes until EOF and decode as UTF-8, appending into `s`.
    fn read_to_string(&mut self, s: &mut String) -> Result<usize> {
        let mut buf = Vec::new();
        let n = self.read_to_end(&mut buf)?;
        match String::from_utf8(buf) {
            Ok(t)  => { s.push_str(&t); Ok(n) }
            Err(_) => Err(Error::INVALID_DATA),
        }
    }

    /// Wrap `self` in a `Bytes` iterator.
    fn bytes(self) -> Bytes<Self> where Self: Sized {
        Bytes { inner: self }
    }

    /// Wrap `self` with a `Take` limit.
    fn take(self, limit: u64) -> Take<Self> where Self: Sized {
        Take { inner: self, limit }
    }

    /// Chain `self` with `next`, reading from `next` once `self` is exhausted.
    fn chain<R: Read>(self, next: R) -> Chain<Self, R> where Self: Sized {
        Chain { first: self, second: next, done_first: false }
    }
}

// ── Read impls ────────────────────────────────────────────────────────────────

impl Read for &[u8] {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        let n = self.len().min(buf.len());
        buf[..n].copy_from_slice(&self[..n]);
        *self = &self[n..];
        Ok(n)
    }
}

// ── Read adaptors ─────────────────────────────────────────────────────────────

pub struct Bytes<R> { inner: R }

impl<R: Read> Iterator for Bytes<R> {
    type Item = Result<u8>;
    fn next(&mut self) -> Option<Self::Item> {
        let mut b = [0u8; 1];
        match self.inner.read(&mut b) {
            Ok(0) => None,
            Ok(_) => Some(Ok(b[0])),
            Err(e) => Some(Err(e)),
        }
    }
}

pub struct Take<R> {
    inner: R,
    limit: u64,
}

impl<R: Read> Read for Take<R> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        if self.limit == 0 { return Ok(0); }
        let max = (buf.len() as u64).min(self.limit) as usize;
        let n = self.inner.read(&mut buf[..max])?;
        self.limit -= n as u64;
        Ok(n)
    }
}

pub struct Chain<A, B> {
    first:      A,
    second:     B,
    done_first: bool,
}

impl<A: Read, B: Read> Read for Chain<A, B> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        if !self.done_first {
            match self.first.read(buf)? {
                0 => self.done_first = true,
                n => return Ok(n),
            }
        }
        self.second.read(buf)
    }
}

// ── Write ─────────────────────────────────────────────────────────────────────

/// The `Write` trait for byte sinks.
pub trait Write {
    /// Write some bytes from `buf`. Returns how many were written.
    fn write(&mut self, buf: &[u8]) -> Result<usize>;

    /// Flush any internally buffered data to the underlying sink.
    fn flush(&mut self) -> Result<()>;

    /// Write all bytes in `buf`, retrying on short writes.
    fn write_all(&mut self, mut buf: &[u8]) -> Result<()> {
        while !buf.is_empty() {
            match self.write(buf) {
                Ok(0)  => return Err(Error::WRITE_ZERO),
                Ok(n)  => buf = &buf[n..],
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    /// Format and write using `core::fmt::Arguments`.
    fn write_fmt(&mut self, args: core::fmt::Arguments<'_>) -> Result<()> {
        struct Adaptor<'a, T: Write + ?Sized> {
            inner: &'a mut T,
            err:   Option<Error>,
        }
        impl<T: Write + ?Sized> core::fmt::Write for Adaptor<'_, T> {
            fn write_str(&mut self, s: &str) -> core::fmt::Result {
                match self.inner.write_all(s.as_bytes()) {
                    Ok(())  => Ok(()),
                    Err(e)  => { self.err = Some(e); Err(core::fmt::Error) }
                }
            }
        }
        let mut a = Adaptor { inner: self, err: None };
        match core::fmt::write(&mut a, args) {
            Ok(())  => Ok(()),
            Err(_)  => Err(a.err.unwrap_or(Error::OTHER)),
        }
    }

    /// Return a mutable reference to `self` (mirrors std::io::Write::by_ref).
    fn by_ref(&mut self) -> &mut Self where Self: Sized { self }
}

// ── Write impls ───────────────────────────────────────────────────────────────

impl Write for Vec<u8> {
    fn write(&mut self, buf: &[u8]) -> Result<usize> {
        self.extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> Result<()> { Ok(()) }
}

impl<W: Write + ?Sized> Write for &mut W {
    fn write(&mut self, buf: &[u8]) -> Result<usize>  { (**self).write(buf) }
    fn flush(&mut self) -> Result<()>                  { (**self).flush() }
    fn write_all(&mut self, buf: &[u8]) -> Result<()>  { (**self).write_all(buf) }
    fn write_fmt(&mut self, args: core::fmt::Arguments<'_>) -> Result<()> {
        (**self).write_fmt(args)
    }
}

// ── Stdout / Stderr ───────────────────────────────────────────────────────────

/// Handle to the kernel serial console (standard output).
pub struct Stdout;

/// Handle to the kernel serial console (standard error — same sink as stdout).
pub struct Stderr;

/// Returns a handle to the standard output stream.
pub fn stdout() -> Stdout { Stdout }

/// Returns a handle to the standard error stream.
pub fn stderr() -> Stderr { Stderr }

impl Write for Stdout {
    fn write(&mut self, buf: &[u8]) -> Result<usize> {
        if let Ok(s) = core::str::from_utf8(buf) {
            crate::sys_log(s);
        }
        Ok(buf.len())
    }
    fn flush(&mut self) -> Result<()> { Ok(()) }
}

impl core::fmt::Write for Stdout {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        crate::sys_log(s);
        Ok(())
    }
}

impl Write for Stderr {
    fn write(&mut self, buf: &[u8]) -> Result<usize> {
        if let Ok(s) = core::str::from_utf8(buf) {
            crate::sys_log(s);
        }
        Ok(buf.len())
    }
    fn flush(&mut self) -> Result<()> { Ok(()) }
}

impl core::fmt::Write for Stderr {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        crate::sys_log(s);
        Ok(())
    }
}

// ── BufWriter ─────────────────────────────────────────────────────────────────

/// Wraps a writer and batches small writes into a memory buffer.
pub struct BufWriter<W: Write> {
    inner: W,
    buf:   Vec<u8>,
}

impl<W: Write> BufWriter<W> {
    pub fn new(inner: W) -> Self              { Self::with_capacity(8192, inner) }
    pub fn with_capacity(n: usize, inner: W) -> Self {
        BufWriter { inner, buf: Vec::with_capacity(n) }
    }
    pub fn get_ref(&self)      -> &W      { &self.inner }
    pub fn get_mut(&mut self)  -> &mut W  { &mut self.inner }
    pub fn buffer(&self)       -> &[u8]   { &self.buf }

    pub fn into_inner(self) -> Result<W> {
        // Use ManuallyDrop to prevent the Drop impl from running after we move out.
        let mut me = core::mem::ManuallyDrop::new(self);
        me.flush_buf()?;
        // SAFETY: `me` is ManuallyDrop, so `inner` won't be double-dropped.
        Ok(unsafe { core::ptr::read(&me.inner) })
    }

    fn flush_buf(&mut self) -> Result<()> {
        self.inner.write_all(&self.buf)?;
        self.buf.clear();
        Ok(())
    }
}

impl<W: Write> Write for BufWriter<W> {
    fn write(&mut self, buf: &[u8]) -> Result<usize> {
        if self.buf.len() + buf.len() > self.buf.capacity() {
            self.flush_buf()?;
        }
        if buf.len() >= self.buf.capacity() {
            self.inner.write(buf)
        } else {
            self.buf.extend_from_slice(buf);
            Ok(buf.len())
        }
    }
    fn flush(&mut self) -> Result<()> {
        self.flush_buf()?;
        self.inner.flush()
    }
}

impl<W: Write> Drop for BufWriter<W> {
    fn drop(&mut self) { let _ = self.flush_buf(); }
}

// ── BufReader ─────────────────────────────────────────────────────────────────

/// Wraps a reader and buffers reads to reduce syscall overhead.
pub struct BufReader<R: Read> {
    inner:  R,
    buf:    Vec<u8>,
    pos:    usize,
    filled: usize,
}

impl<R: Read> BufReader<R> {
    pub fn new(inner: R) -> Self              { Self::with_capacity(8192, inner) }
    pub fn with_capacity(n: usize, inner: R) -> Self {
        let mut buf = Vec::with_capacity(n);
        buf.resize(n, 0);
        BufReader { inner, buf, pos: 0, filled: 0 }
    }
    pub fn get_ref(&self)     -> &R      { &self.inner }
    pub fn get_mut(&mut self) -> &mut R  { &mut self.inner }
    pub fn into_inner(self)   -> R       { self.inner }
    pub fn buffer(&self)      -> &[u8]   { &self.buf[self.pos..self.filled] }

    fn do_fill(&mut self) -> Result<&[u8]> {
        if self.pos >= self.filled {
            self.filled = self.inner.read(&mut self.buf)?;
            self.pos = 0;
        }
        Ok(&self.buf[self.pos..self.filled])
    }
}

impl<R: Read> Read for BufReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        let avail = self.do_fill()?;
        let n = avail.len().min(buf.len());
        buf[..n].copy_from_slice(&avail[..n]);
        self.pos += n;
        Ok(n)
    }
}

// ── Cursor ────────────────────────────────────────────────────────────────────

/// In-memory byte buffer usable as both `Read` and `Write`.
pub struct Cursor<T> {
    inner: T,
    pos:   u64,
}

impl<T> Cursor<T> {
    pub fn new(inner: T)                -> Self { Cursor { inner, pos: 0 } }
    pub fn into_inner(self)             -> T    { self.inner }
    pub fn get_ref(&self)               -> &T   { &self.inner }
    pub fn get_mut(&mut self)           -> &mut T { &mut self.inner }
    pub fn position(&self)              -> u64  { self.pos }
    pub fn set_position(&mut self, p: u64)      { self.pos = p; }
}

impl<T: AsRef<[u8]>> Cursor<T> {
    fn remaining(&self) -> &[u8] {
        let b = self.inner.as_ref();
        let p = (self.pos as usize).min(b.len());
        &b[p..]
    }
}

impl<T: AsRef<[u8]>> Read for Cursor<T> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        let rem = self.remaining();
        let n = rem.len().min(buf.len());
        buf[..n].copy_from_slice(&rem[..n]);
        self.pos += n as u64;
        Ok(n)
    }
}

impl Write for Cursor<Vec<u8>> {
    fn write(&mut self, buf: &[u8]) -> Result<usize> {
        let pos = self.pos as usize;
        let inner = &mut self.inner;
        if pos > inner.len() { inner.resize(pos, 0); }
        let end = pos + buf.len();
        if end > inner.len() { inner.resize(end, 0); }
        inner[pos..end].copy_from_slice(buf);
        self.pos = end as u64;
        Ok(buf.len())
    }
    fn flush(&mut self) -> Result<()> { Ok(()) }
}

// ── Utility functions ─────────────────────────────────────────────────────────

/// Copy all bytes from `reader` into `writer`. Returns the byte count.
pub fn copy<R: Read, W: Write>(reader: &mut R, writer: &mut W) -> Result<u64> {
    let mut buf   = [0u8; 4096];
    let mut total = 0u64;
    loop {
        match reader.read(&mut buf)? {
            0 => return Ok(total),
            n => { writer.write_all(&buf[..n])?; total += n as u64; }
        }
    }
}

/// A writer that discards all output.
pub struct Sink;
pub fn sink() -> Sink { Sink }

impl Write for Sink {
    fn write(&mut self, buf: &[u8]) -> Result<usize> { Ok(buf.len()) }
    fn flush(&mut self) -> Result<()> { Ok(()) }
}

/// A reader that immediately returns EOF.
pub struct Empty;
pub fn empty() -> Empty { Empty }

impl Read for Empty {
    fn read(&mut self, _: &mut [u8]) -> Result<usize> { Ok(0) }
}

/// A reader that repeats a single byte indefinitely.
pub struct Repeat { byte: u8 }
pub fn repeat(byte: u8) -> Repeat { Repeat { byte } }

impl Read for Repeat {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        for b in buf.iter_mut() { *b = self.byte; }
        Ok(buf.len())
    }
}
