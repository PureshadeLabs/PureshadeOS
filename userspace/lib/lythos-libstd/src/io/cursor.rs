use super::{Read, Write, Seek, SeekFrom, BufRead, Result, Error, ErrorKind};
use _alloc::vec::Vec;

/// An in-memory I/O cursor, equivalent to `std::io::Cursor`.
pub struct Cursor<T> {
    inner: T,
    pos:   u64,
}

impl<T> Cursor<T> {
    pub fn new(inner: T) -> Self { Cursor { inner, pos: 0 } }
    pub fn into_inner(self) -> T { self.inner }
    pub fn get_ref(&self) -> &T { &self.inner }
    pub fn get_mut(&mut self) -> &mut T { &mut self.inner }
    pub fn position(&self) -> u64 { self.pos }
    pub fn set_position(&mut self, pos: u64) { self.pos = pos; }
}

impl<T: AsRef<[u8]>> Read for Cursor<T> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        let data = self.inner.as_ref();
        let start = self.pos.min(data.len() as u64) as usize;
        let slice = &data[start..];
        let n = buf.len().min(slice.len());
        buf[..n].copy_from_slice(&slice[..n]);
        self.pos += n as u64;
        Ok(n)
    }
}

impl<T: AsRef<[u8]>> BufRead for Cursor<T> {
    fn fill_buf(&mut self) -> Result<&[u8]> {
        let data = self.inner.as_ref();
        let start = self.pos.min(data.len() as u64) as usize;
        Ok(&data[start..])
    }
    fn consume(&mut self, amt: usize) { self.pos += amt as u64; }
}

impl Write for Cursor<Vec<u8>> {
    fn write(&mut self, buf: &[u8]) -> Result<usize> {
        let pos = self.pos as usize;
        let end = pos + buf.len();
        if end > self.inner.len() {
            self.inner.resize(end, 0);
        }
        self.inner[pos..end].copy_from_slice(buf);
        self.pos = end as u64;
        Ok(buf.len())
    }
    fn flush(&mut self) -> Result<()> { Ok(()) }
}

impl<T: AsRef<[u8]>> Seek for Cursor<T> {
    fn seek(&mut self, pos: SeekFrom) -> Result<u64> {
        let len = self.inner.as_ref().len() as i64;
        let new = match pos {
            SeekFrom::Start(n)   => n as i64,
            SeekFrom::End(n)     => len + n,
            SeekFrom::Current(n) => self.pos as i64 + n,
        };
        if new < 0 {
            return Err(Error::new(ErrorKind::InvalidInput, "seek before start"));
        }
        self.pos = new as u64;
        Ok(self.pos)
    }
}
