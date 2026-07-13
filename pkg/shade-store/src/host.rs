//! `HostFs` — the std-backed [`StoreFs`] for the host suite and host tooling
//! (the seed CLI). Same realization logic as on target; only this backend
//! touches `std::fs`.

use std::fs;
use std::io::{self, Write as _};
use std::path::Path;

use alloc::string::String;
use alloc::vec::Vec;

use crate::backend::{FsError, FsResult, NodeKind, NodeMeta, StoreFs};

/// The std host backend. Stateless.
#[derive(Debug, Default, Clone, Copy)]
pub struct HostFs;

fn fs_err(e: io::Error) -> FsError {
    match e.kind() {
        io::ErrorKind::NotFound => FsError::NotFound,
        io::ErrorKind::AlreadyExists => FsError::Exists,
        io::ErrorKind::NotADirectory => FsError::NotDir,
        io::ErrorKind::IsADirectory => FsError::IsDir,
        io::ErrorKind::DirectoryNotEmpty => FsError::NotEmpty,
        io::ErrorKind::StorageFull => FsError::NoSpace,
        io::ErrorKind::InvalidInput => FsError::Invalid,
        io::ErrorKind::ReadOnlyFilesystem => FsError::ReadOnly,
        _ => FsError::Device,
    }
}

fn kind_of(ft: fs::FileType) -> NodeKind {
    if ft.is_symlink() {
        NodeKind::Symlink
    } else if ft.is_dir() {
        NodeKind::Dir
    } else if ft.is_file() {
        NodeKind::File
    } else {
        NodeKind::Other
    }
}

impl StoreFs for HostFs {
    fn metadata(&mut self, path: &str) -> FsResult<NodeMeta> {
        let m = fs::symlink_metadata(path).map_err(fs_err)?;
        let kind = kind_of(m.file_type());
        #[cfg(unix)]
        let exec = {
            use std::os::unix::fs::PermissionsExt;
            kind == NodeKind::File && m.permissions().mode() & 0o111 != 0
        };
        #[cfg(not(unix))]
        let exec = false;
        Ok(NodeMeta { kind, exec })
    }

    fn read_file(&mut self, path: &str) -> FsResult<Vec<u8>> {
        fs::read(path).map_err(fs_err)
    }

    fn write_file(&mut self, path: &str, data: &[u8], exec: bool) -> FsResult<()> {
        let mut f = fs::File::create(path).map_err(fs_err)?;
        f.write_all(data).map_err(fs_err)?;
        #[cfg(unix)]
        if exec {
            use std::os::unix::fs::PermissionsExt;
            f.set_permissions(fs::Permissions::from_mode(0o755)).map_err(fs_err)?;
        }
        #[cfg(not(unix))]
        let _ = exec;
        f.sync_all().map_err(fs_err)
    }

    fn mkdir(&mut self, path: &str) -> FsResult<()> {
        fs::create_dir(path).map_err(fs_err)
    }

    fn rename(&mut self, old: &str, new: &str) -> FsResult<()> {
        fs::rename(old, new).map_err(fs_err)
    }

    fn unlink(&mut self, path: &str) -> FsResult<()> {
        fs::remove_file(path).map_err(fs_err)
    }

    fn rmdir(&mut self, path: &str) -> FsResult<()> {
        fs::remove_dir(path).map_err(fs_err)
    }

    fn read_dir(&mut self, path: &str) -> FsResult<Vec<(String, NodeKind)>> {
        let mut out = Vec::new();
        for entry in fs::read_dir(path).map_err(fs_err)? {
            let entry = entry.map_err(fs_err)?;
            let name = entry.file_name().into_string().map_err(|_| FsError::Invalid)?;
            let ft = entry.file_type().map_err(fs_err)?;
            out.push((name, kind_of(ft)));
        }
        Ok(out)
    }

    fn read_link(&mut self, path: &str) -> FsResult<String> {
        let t = fs::read_link(path).map_err(fs_err)?;
        t.into_os_string().into_string().map_err(|_| FsError::Invalid)
    }

    #[cfg(unix)]
    fn symlink(&mut self, target: &str, link: &str) -> FsResult<()> {
        std::os::unix::fs::symlink(target, link).map_err(fs_err)
    }
    #[cfg(not(unix))]
    fn symlink(&mut self, _target: &str, _link: &str) -> FsResult<()> {
        Err(FsError::Unsupported)
    }

    /// Best-effort: some platforms reject fsync on a directory fd — not fatal.
    fn sync_dir(&mut self, path: &str) -> FsResult<()> {
        if let Ok(f) = fs::File::open(Path::new(path)) {
            let _ = f.sync_all();
        }
        Ok(())
    }

    fn unique_token(&mut self) -> u64 {
        std::process::id() as u64
    }
}
