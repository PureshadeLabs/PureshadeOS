use super::{Read, Write, BufRead, Result, Error, ErrorKind};
use _alloc::vec::Vec;

const DEFAULT_BUF_SIZE: usize = 8192;

// ── BufReader ─────────────────────────────────────────────────────────────────

pub struct BufReader<R> {
    inner: R,
    buf:   Vec<u8>,
    pos:   usize,
    cap:   usize,
}

impl<R: Read> BufReader<R> {
    pub fn new(inner: R) -> Self { Self::with_capacity(DEFAULT_BUF_SIZE, inner) }

    pub fn with_capacity(cap: usize, inner: R) -> Self {
        BufReader { inner, buf: _alloc::vec![0; cap], pos: 0, cap: 0 }
    }

    pub fn get_ref(&self)      -> &R          { &self.inner }
    pub fn get_mut(&mut self)  -> &mut R      { &mut self.inner }
    pub fn into_inner(self)    -> R           { self.inner }
    pub fn buffer(&self)       -> &[u8]       { &self.buf[self.pos..self.cap] }
    pub fn capacity(&self)     -> usize       { self.buf.len() }
}

impl<R: Read> Read for BufReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        if self.pos >= self.cap {
            self.cap = self.inner.read(&mut self.buf)?;
            self.pos = 0;
        }
        let available = &self.buf[self.pos..self.cap];
        let n = buf.len().min(available.len());
        buf[..n].copy_from_slice(&available[..n]);
        self.pos += n;
        Ok(n)
    }
}

impl<R: Read> BufRead for BufReader<R> {
    fn fill_buf(&mut self) -> Result<&[u8]> {
        if self.pos >= self.cap {
            self.cap = self.inner.read(&mut self.buf)?;
            self.pos = 0;
        }
        Ok(&self.buf[self.pos..self.cap])
    }
    fn consume(&mut self, amt: usize) { self.pos = (self.pos + amt).min(self.cap); }
}

// ── BufWriter ─────────────────────────────────────────────────────────────────

pub struct BufWriter<W: Write> {
    inner: W,
    buf:   Vec<u8>,
}

impl<W: Write> BufWriter<W> {
    pub fn new(inner: W) -> Self { Self::with_capacity(DEFAULT_BUF_SIZE, inner) }

    pub fn with_capacity(cap: usize, inner: W) -> Self {
        BufWriter { inner, buf: Vec::with_capacity(cap) }
    }

    pub fn get_ref(&self)     -> &W     { &self.inner }
    pub fn get_mut(&mut self) -> &mut W { &mut self.inner }
    pub fn buffer(&self)      -> &[u8]  { &self.buf }

    pub fn into_inner(mut self) -> core::result::Result<W, crate::io::Error> {
        match self.flush_buf() {
            Ok(()) => {
                // Use ManuallyDrop to move `inner` out of a type that implements Drop.
                let md = core::mem::ManuallyDrop::new(self);
                Ok(unsafe { core::ptr::read(&md.inner) })
            }
            Err(e) => Err(e),
        }
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
    fn drop(&mut self) {
        let _ = self.flush_buf();
    }
}

// ── LineWriter ────────────────────────────────────────────────────────────────

pub struct LineWriter<W: Write>(BufWriter<W>);

impl<W: Write> LineWriter<W> {
    pub fn new(inner: W) -> Self { LineWriter(BufWriter::with_capacity(1024, inner)) }
    pub fn into_inner(self) -> core::result::Result<W, crate::io::Error> {
        self.0.into_inner()
    }
}

impl<W: Write> Write for LineWriter<W> {
    fn write(&mut self, buf: &[u8]) -> Result<usize> {
        let n = self.0.write(buf)?;
        if buf[..n].contains(&b'\n') {
            self.0.flush_buf()?;
        }
        Ok(n)
    }
    fn flush(&mut self) -> Result<()> { self.0.flush() }
}
