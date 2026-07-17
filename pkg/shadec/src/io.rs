//! Evaluation IO boundary. All filesystem access the purity rules permit
//! (`docs/shade/03-semantics.md` §5.2 — tracked reads) goes through this
//! trait; the evaluator records every call as an eval input. There is no
//! environment/clock/network access here by construction — the trait simply
//! has no such methods.
//!
//! Host implementation behind `std`; on-target implementation ([`OrosIo`])
//! behind `oros`, over the raw Lythos VFS syscalls (same rationale as
//! shade-store's `OrosFs`: the raw stubs keep errno fidelity).

use alloc::string::String;
use alloc::vec::Vec;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    Regular,
    Directory,
    Symlink,
    Other,
}

impl FileKind {
    /// The strings `builtins.readDir` reports (07 §2.5).
    pub fn tag(self) -> &'static str {
        match self {
            FileKind::Regular => "regular",
            FileKind::Directory => "directory",
            FileKind::Symlink => "symlink",
            FileKind::Other => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct FileMeta {
    pub kind: FileKind,
    pub exec: bool,
}

pub trait EvalIo {
    fn read_file(&self, path: &str) -> Result<Vec<u8>, String>;
    /// lstat semantics (no symlink follow) — the tree-hash walk needs the
    /// symlink itself (shade-pkg 04 §3.3).
    fn metadata(&self, path: &str) -> Result<FileMeta, String>;
    /// stat semantics (follows symlinks) — `builtins.pathExists`.
    fn exists(&self, path: &str) -> bool;
    fn read_dir(&self, path: &str) -> Result<Vec<(String, FileKind)>, String>;
    fn read_link(&self, path: &str) -> Result<String, String>;
    /// `builtins.trace` diagnostic stream — eval-pure (07 §2.1).
    fn trace(&self, msg: &str);
}

#[cfg(feature = "std")]
pub struct HostIo;

#[cfg(feature = "std")]
impl EvalIo for HostIo {
    fn read_file(&self, path: &str) -> Result<Vec<u8>, String> {
        std::fs::read(path).map_err(|e| std::format!("{path}: {e}"))
    }

    fn metadata(&self, path: &str) -> Result<FileMeta, String> {
        let m = std::fs::symlink_metadata(path).map_err(|e| std::format!("{path}: {e}"))?;
        let ft = m.file_type();
        let kind = if ft.is_file() {
            FileKind::Regular
        } else if ft.is_dir() {
            FileKind::Directory
        } else if ft.is_symlink() {
            FileKind::Symlink
        } else {
            FileKind::Other
        };
        #[cfg(unix)]
        let exec = {
            use std::os::unix::fs::PermissionsExt;
            kind == FileKind::Regular && m.permissions().mode() & 0o111 != 0
        };
        #[cfg(not(unix))]
        let exec = false;
        Ok(FileMeta { kind, exec })
    }

    fn exists(&self, path: &str) -> bool {
        std::fs::metadata(path).is_ok()
    }

    fn read_dir(&self, path: &str) -> Result<Vec<(String, FileKind)>, String> {
        let mut out = Vec::new();
        for entry in std::fs::read_dir(path).map_err(|e| std::format!("{path}: {e}"))? {
            let entry = entry.map_err(|e| std::format!("{path}: {e}"))?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| std::format!("{path}: non-UTF-8 file name"))?;
            let ft = entry.file_type().map_err(|e| std::format!("{path}: {e}"))?;
            let kind = if ft.is_file() {
                FileKind::Regular
            } else if ft.is_dir() {
                FileKind::Directory
            } else if ft.is_symlink() {
                FileKind::Symlink
            } else {
                FileKind::Other
            };
            out.push((name, kind));
        }
        out.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(out)
    }

    fn read_link(&self, path: &str) -> Result<String, String> {
        let t = std::fs::read_link(path).map_err(|e| std::format!("{path}: {e}"))?;
        t.into_os_string().into_string().map_err(|_| std::format!("{path}: non-UTF-8 symlink target"))
    }

    fn trace(&self, msg: &str) {
        std::eprintln!("trace: {msg}");
    }
}

/// The on-target `EvalIo` over raw Lythos VFS syscalls (feature `oros`,
/// x86_64 only — `lythos-syscall` is empty elsewhere).
///
/// lstat semantics for [`metadata`](EvalIo::metadata) come from probing
/// `SYS_READLINK` first (success ⇒ the path IS a symlink) — the ABI has no
/// lstat; `SYS_STAT` follows symlinks. Exec bits: the kernel does not enforce
/// them and `SYS_CREATE` cannot set them, so staged trees report `exec=false`
/// (matches what OrosFs-realized store trees carry).
#[cfg(feature = "oros")]
pub struct OrosIo;

#[cfg(feature = "oros")]
impl EvalIo for OrosIo {
    fn read_file(&self, path: &str) -> Result<Vec<u8>, String> {
        use alloc::format;
        use lythos_abi::{errno as e, syscall as nr};
        use lythos_syscall::{syscall1, syscall2, syscall3};
        let fd = unsafe { syscall2(nr::SYS_OPEN, path.as_ptr() as u64, path.len() as u64) };
        if e::is_err(fd) {
            return Err(format!("{path}: open failed (errno {})", fd as i64));
        }
        let mut out = Vec::new();
        let mut chunk = alloc::vec![0u8; 64 * 1024];
        loop {
            let r = unsafe {
                syscall3(nr::SYS_READ, fd, chunk.as_mut_ptr() as u64, chunk.len() as u64)
            };
            if e::is_err(r) {
                unsafe { syscall1(nr::SYS_CLOSE, fd) };
                return Err(format!("{path}: read failed (errno {})", r as i64));
            }
            let n = r as usize;
            if n == 0 {
                break;
            }
            out.extend_from_slice(&chunk[..n]);
        }
        unsafe { syscall1(nr::SYS_CLOSE, fd) };
        Ok(out)
    }

    fn metadata(&self, path: &str) -> Result<FileMeta, String> {
        use alloc::format;
        use lythos_abi::{errno as e, syscall as nr};
        use lythos_syscall::{syscall3, syscall4};
        // lstat: a successful READLINK means the final component is a symlink.
        let mut tbuf = [0u8; 1];
        let r = unsafe {
            syscall4(
                nr::SYS_READLINK,
                path.as_ptr() as u64,
                path.len() as u64,
                tbuf.as_mut_ptr() as u64,
                0,
            )
        };
        if !e::is_err(r) {
            return Ok(FileMeta { kind: FileKind::Symlink, exec: false });
        }
        let mut buf = [0u8; 48];
        let r = unsafe {
            syscall3(nr::SYS_STAT, path.as_ptr() as u64, path.len() as u64, buf.as_mut_ptr() as u64)
        };
        if e::is_err(r) {
            return Err(format!("{path}: stat failed (errno {})", r as i64));
        }
        // FileStat wire layout (docs/spec/syscalls.md): flags u32 at offset
        // 24 (bit 1 = dir), mode u16 at offset 40.
        let flags = u32::from_le_bytes(buf[24..28].try_into().unwrap());
        let mode = u16::from_le_bytes(buf[40..42].try_into().unwrap());
        let kind = if flags & (1 << 1) != 0 { FileKind::Directory } else { FileKind::Regular };
        Ok(FileMeta { kind, exec: kind == FileKind::Regular && mode & 0o111 != 0 })
    }

    fn exists(&self, path: &str) -> bool {
        use lythos_abi::{errno as e, syscall as nr};
        use lythos_syscall::syscall3;
        let mut buf = [0u8; 48];
        let r = unsafe {
            syscall3(nr::SYS_STAT, path.as_ptr() as u64, path.len() as u64, buf.as_mut_ptr() as u64)
        };
        !e::is_err(r)
    }

    fn read_dir(&self, path: &str) -> Result<Vec<(String, FileKind)>, String> {
        use alloc::format;
        use lythos_abi::{errno as e, syscall as nr};
        use lythos_syscall::syscall4;
        // Readdir wire slot (264 bytes): file_type u8 at 4 (1=reg, 2=dir,
        // 3=symlink), name_len u8 at 5, name at 8.
        const SLOT: usize = 264;
        let mut buf = alloc::vec![0u8; 1024 * SLOT];
        let r = unsafe {
            syscall4(
                nr::SYS_READDIR,
                path.as_ptr() as u64,
                path.len() as u64,
                buf.as_mut_ptr() as u64,
                buf.len() as u64,
            )
        };
        if e::is_err(r) {
            return Err(format!("{path}: readdir failed (errno {})", r as i64));
        }
        let mut out = Vec::with_capacity(r as usize);
        for i in 0..r as usize {
            let s = &buf[i * SLOT..(i + 1) * SLOT];
            let name_len = s[5] as usize;
            let name = core::str::from_utf8(&s[8..8 + name_len])
                .map_err(|_| format!("{path}: non-UTF-8 entry name"))?;
            // Kernel SYS_READDIR yields "." and ".."; the EvalIo contract (like
            // HostIo over std::fs) excludes them.
            if name == "." || name == ".." {
                continue;
            }
            let kind = match s[4] {
                1 => FileKind::Regular,
                2 => FileKind::Directory,
                3 => FileKind::Symlink,
                _ => FileKind::Other,
            };
            out.push((String::from(name), kind));
        }
        out.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(out)
    }

    fn read_link(&self, path: &str) -> Result<String, String> {
        use alloc::format;
        use lythos_abi::{errno as e, syscall as nr};
        use lythos_syscall::syscall4;
        let mut buf = alloc::vec![0u8; 4096];
        let r = unsafe {
            syscall4(
                nr::SYS_READLINK,
                path.as_ptr() as u64,
                path.len() as u64,
                buf.as_mut_ptr() as u64,
                buf.len() as u64,
            )
        };
        if e::is_err(r) {
            return Err(format!("{path}: readlink failed (errno {})", r as i64));
        }
        let n = (r as usize).min(buf.len());
        buf.truncate(n);
        String::from_utf8(buf).map_err(|_| format!("{path}: non-UTF-8 symlink target"))
    }

    fn trace(&self, msg: &str) {
        use lythos_abi::syscall as nr;
        use lythos_syscall::syscall2;
        use alloc::format;
        let line = format!("trace: {msg}\n");
        unsafe { syscall2(nr::SYS_LOG, line.as_ptr() as u64, line.len() as u64) };
    }
}
