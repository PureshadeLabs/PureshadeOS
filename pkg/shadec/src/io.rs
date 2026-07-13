//! Evaluation IO boundary. All filesystem access the purity rules permit
//! (`docs/shade/03-semantics.md` §5.2 — tracked reads) goes through this
//! trait; the evaluator records every call as an eval input. There is no
//! environment/clock/network access here by construction — the trait simply
//! has no such methods.
//!
//! Host implementation behind `std`; the OROS implementation (lythos-libstd
//! VFS) lands with the `shade` binary wiring — TODO(open): blocked on an
//! OROS `EvalIo` over lythos-libstd's VFS. (The former argv blocker is
//! resolved — argv flows through SYS_EXEC and `lythos_rt::args`; see
//! pkg/shade.)

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
