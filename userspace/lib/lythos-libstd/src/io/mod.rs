//! I/O traits and standard streams for Lythos.
//!
//! Provides `Read`, `Write`, `BufRead`, `Seek` (stub), `Error`, `Result`,
//! `stdin()`, `stdout()`, `stderr()`, `Cursor`, `BufReader`, `BufWriter`.

use _alloc::string::String;
use _alloc::vec::Vec;

mod error;
mod stdio;
mod cursor;
mod buffered;

pub use error::{Error, ErrorKind, Result};
pub use stdio::{stdin, stdout, stderr, Stdin, Stdout, Stderr, StdinLock, StdoutLock, StderrLock};
pub use cursor::Cursor;
pub use buffered::{BufReader, BufWriter, LineWriter};

// ── Read ──────────────────────────────────────────────────────────────────────

pub trait Read {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize>;

    fn read_to_end(&mut self, buf: &mut Vec<u8>) -> Result<usize> {
        let mut tmp = [0u8; 512];
        let mut total = 0;
        loop {
            match self.read(&mut tmp) {
                Ok(0)  => break,
                Ok(n)  => { buf.extend_from_slice(&tmp[..n]); total += n; }
                Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
        }
        Ok(total)
    }

    fn read_to_string(&mut self, buf: &mut String) -> Result<usize> {
        let mut bytes = Vec::new();
        let n = self.read_to_end(&mut bytes)?;
        let s = core::str::from_utf8(&bytes)
            .map_err(|_| Error::new(ErrorKind::InvalidData, "not valid UTF-8"))?;
        buf.push_str(s);
        Ok(n)
    }

    fn read_exact(&mut self, mut buf: &mut [u8]) -> Result<()> {
        while !buf.is_empty() {
            match self.read(buf) {
                Ok(0) => return Err(Error::new(ErrorKind::UnexpectedEof, "unexpected EOF")),
                Ok(n) => buf = &mut buf[n..],
                Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    fn by_ref(&mut self) -> &mut Self where Self: Sized { self }

    fn bytes(self) -> Bytes<Self> where Self: Sized { Bytes { inner: self } }

    fn take(self, limit: u64) -> Take<Self> where Self: Sized {
        Take { inner: self, limit }
    }
}

pub struct Bytes<R> { inner: R }
impl<R: Read> Iterator for Bytes<R> {
    type Item = Result<u8>;
    fn next(&mut self) -> Option<Self::Item> {
        let mut byte = [0u8];
        match self.inner.read(&mut byte) {
            Ok(0) => None,
            Ok(_) => Some(Ok(byte[0])),
            Err(e) => Some(Err(e)),
        }
    }
}

pub struct Take<R> { inner: R, limit: u64 }
impl<R: Read> Read for Take<R> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        if self.limit == 0 { return Ok(0); }
        let max = buf.len().min(self.limit as usize);
        let n = self.inner.read(&mut buf[..max])?;
        self.limit -= n as u64;
        Ok(n)
    }
}

// ── Write ─────────────────────────────────────────────────────────────────────

pub trait Write {
    fn write(&mut self, buf: &[u8]) -> Result<usize>;
    fn flush(&mut self) -> Result<()>;

    fn write_all(&mut self, mut buf: &[u8]) -> Result<()> {
        while !buf.is_empty() {
            match self.write(buf) {
                Ok(0)  => return Err(Error::new(ErrorKind::WriteZero, "write returned 0")),
                Ok(n)  => buf = &buf[n..],
                Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    fn write_fmt(&mut self, fmt: core::fmt::Arguments<'_>) -> Result<()> {
        struct Adapter<'a, T: ?Sized>(&'a mut T, Option<Error>);
        impl<T: Write + ?Sized> core::fmt::Write for Adapter<'_, T> {
            fn write_str(&mut self, s: &str) -> core::fmt::Result {
                match self.0.write_all(s.as_bytes()) {
                    Ok(()) => Ok(()),
                    Err(e) => { self.1 = Some(e); Err(core::fmt::Error) }
                }
            }
        }
        let mut out = Adapter(self, None);
        core::fmt::write(&mut out, fmt)?;
        Ok(())
    }

    fn by_ref(&mut self) -> &mut Self where Self: Sized { self }
}

// ── BufRead ───────────────────────────────────────────────────────────────────

pub trait BufRead: Read {
    fn fill_buf(&mut self) -> Result<&[u8]>;
    fn consume(&mut self, amt: usize);

    fn read_line(&mut self, buf: &mut String) -> Result<usize> {
        let mut bytes = Vec::new();
        loop {
            let (done, used) = {
                let available = self.fill_buf()?;
                if let Some(i) = available.iter().position(|&b| b == b'\n') {
                    bytes.extend_from_slice(&available[..=i]);
                    (true, i + 1)
                } else {
                    bytes.extend_from_slice(available);
                    (available.is_empty(), available.len())
                }
            };
            self.consume(used);
            if done { break; }
        }
        let n = bytes.len();
        match core::str::from_utf8(&bytes) {
            Ok(s) => { buf.push_str(s); Ok(n) }
            Err(_) => Err(Error::new(ErrorKind::InvalidData, "not valid UTF-8")),
        }
    }

    fn lines(self) -> Lines<Self> where Self: Sized { Lines { buf: self } }
}

pub struct Lines<B> { buf: B }
impl<B: BufRead> Iterator for Lines<B> {
    type Item = Result<String>;
    fn next(&mut self) -> Option<Self::Item> {
        let mut line = String::new();
        match self.buf.read_line(&mut line) {
            Ok(0) => None,
            Ok(_) => {
                if line.ends_with('\n') { line.pop(); if line.ends_with('\r') { line.pop(); } }
                Some(Ok(line))
            }
            Err(e) => Some(Err(e)),
        }
    }
}

// ── Seek (stub) ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeekFrom {
    Start(u64),
    End(i64),
    Current(i64),
}

pub trait Seek {
    fn seek(&mut self, pos: SeekFrom) -> Result<u64>;
    fn stream_position(&mut self) -> Result<u64> { self.seek(SeekFrom::Current(0)) }
}

// ── Copy ─────────────────────────────────────────────────────────────────────

pub fn copy<R: Read, W: Write>(reader: &mut R, writer: &mut W) -> Result<u64> {
    let mut buf = [0u8; 8192];
    let mut total = 0u64;
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => { writer.write_all(&buf[..n])?; total += n as u64; }
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(total)
}

// ── impl Read/Write for common primitives ─────────────────────────────────────

impl Read for &[u8] {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        let n = buf.len().min(self.len());
        buf[..n].copy_from_slice(&self[..n]);
        *self = &self[n..];
        Ok(n)
    }
}

impl Write for Vec<u8> {
    fn write(&mut self, buf: &[u8]) -> Result<usize> {
        self.extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> Result<()> { Ok(()) }
}

impl Write for &mut Vec<u8> {
    fn write(&mut self, buf: &[u8]) -> Result<usize> {
        self.extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> Result<()> { Ok(()) }
}
