//! Filesystem stub for Lythos.
//!
//! The Lythos microkernel does not yet have a VFS layer.  All operations
//! return `ErrorKind::Unsupported`.  This module exists so that code that
//! imports `std::fs` compiles; replace with real implementations once a
//! filesystem capability server exists.

use crate::io::{Error, ErrorKind, Read, Write, Seek, SeekFrom, Result};
use _alloc::string::String;
use _alloc::vec::Vec;
use crate::path::Path;

// ── File ──────────────────────────────────────────────────────────────────────

pub struct File(());

impl File {
    pub fn open(_path: &Path) -> Result<File> {
        Err(Error::new(ErrorKind::Unsupported, "filesystem not implemented"))
    }

    pub fn create(_path: &Path) -> Result<File> {
        Err(Error::new(ErrorKind::Unsupported, "filesystem not implemented"))
    }

    pub fn metadata(&self) -> Result<Metadata> {
        Err(Error::new(ErrorKind::Unsupported, "filesystem not implemented"))
    }

    pub fn sync_all(&self) -> Result<()> { Ok(()) }
    pub fn sync_data(&self) -> Result<()> { Ok(()) }
    pub fn set_len(&self, _size: u64) -> Result<()> {
        Err(Error::new(ErrorKind::Unsupported, "filesystem not implemented"))
    }
    pub fn try_clone(&self) -> Result<File> {
        Err(Error::new(ErrorKind::Unsupported, "filesystem not implemented"))
    }
}

impl Read for File {
    fn read(&mut self, _buf: &mut [u8]) -> Result<usize> {
        Err(Error::new(ErrorKind::Unsupported, "filesystem not implemented"))
    }
}

impl Write for File {
    fn write(&mut self, _buf: &[u8]) -> Result<usize> {
        Err(Error::new(ErrorKind::Unsupported, "filesystem not implemented"))
    }
    fn flush(&mut self) -> Result<()> { Ok(()) }
}

impl Seek for File {
    fn seek(&mut self, _pos: SeekFrom) -> Result<u64> {
        Err(Error::new(ErrorKind::Unsupported, "filesystem not implemented"))
    }
}

// ── Metadata ──────────────────────────────────────────────────────────────────

pub struct Metadata(());

impl Metadata {
    pub fn len(&self) -> u64 { 0 }
    pub fn is_dir(&self) -> bool { false }
    pub fn is_file(&self) -> bool { false }
    pub fn is_symlink(&self) -> bool { false }
}

// ── DirEntry / ReadDir ────────────────────────────────────────────────────────

pub struct DirEntry(());
pub struct ReadDir(());

impl DirEntry {
    pub fn path(&self) -> crate::path::PathBuf { crate::path::PathBuf::new() }
    pub fn file_name(&self) -> crate::ffi::OsString { crate::ffi::OsString::new() }
    pub fn metadata(&self) -> Result<Metadata> {
        Err(Error::new(ErrorKind::Unsupported, "filesystem not implemented"))
    }
}

impl Iterator for ReadDir {
    type Item = Result<DirEntry>;
    fn next(&mut self) -> Option<Self::Item> { None }
}

// ── Free functions ────────────────────────────────────────────────────────────

fn unsupported<T>() -> Result<T> {
    Err(Error::new(ErrorKind::Unsupported, "filesystem not implemented"))
}

pub fn read(_path: &Path) -> Result<Vec<u8>> { unsupported() }
pub fn read_to_string(_path: &Path) -> Result<String> { unsupported() }
pub fn write(_path: &Path, _contents: &[u8]) -> Result<()> { unsupported() }
pub fn create_dir(_path: &Path) -> Result<()> { unsupported() }
pub fn create_dir_all(_path: &Path) -> Result<()> { unsupported() }
pub fn remove_file(_path: &Path) -> Result<()> { unsupported() }
pub fn remove_dir(_path: &Path) -> Result<()> { unsupported() }
pub fn remove_dir_all(_path: &Path) -> Result<()> { unsupported() }
pub fn rename(_from: &Path, _to: &Path) -> Result<()> { unsupported() }
pub fn copy(_from: &Path, _to: &Path) -> Result<u64> { unsupported() }
pub fn metadata(_path: &Path) -> Result<Metadata> { unsupported() }
pub fn read_dir(_path: &Path) -> Result<ReadDir> { unsupported() }
pub fn canonicalize(_path: &Path) -> Result<crate::path::PathBuf> { unsupported() }
pub fn symlink_metadata(_path: &Path) -> Result<Metadata> { unsupported() }
pub fn hard_link(_src: &Path, _dst: &Path) -> Result<()> { unsupported() }
