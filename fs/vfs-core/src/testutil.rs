//! `MemBackend` — an in-memory [`FsBackend`] for host tests. Enough of a
//! filesystem to exercise mount routing and the realize flow: a flat map of
//! canonical absolute paths to nodes, with dir/file/rename/readdir. Not a
//! model of RFS V2 semantics — just a faithful [`FsBackend`].

use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::backend::{DirEntry, FsBackend, FsError, FsResult, InodeMeta};

struct Node {
    ino: u64,
    is_dir: bool,
    is_symlink: bool,
    data: Vec<u8>,
    target: Option<String>,
}

pub struct MemBackend {
    nodes: BTreeMap<String, Node>,
    by_ino: BTreeMap<u64, String>,
    next_ino: u64,
    dirty: bool,
    gen: u64,
}

/// Canonicalize an absolute path to `/`-joined components. `None` if relative.
fn canon(p: &str) -> Option<String> {
    if !p.starts_with('/') {
        return None;
    }
    let comps: Vec<&str> = p.split('/').filter(|c| !c.is_empty()).collect();
    if comps.is_empty() {
        Some("/".to_string())
    } else {
        let mut s = String::new();
        for c in &comps {
            s.push('/');
            s.push_str(c);
        }
        Some(s)
    }
}

/// Canonical parent path of a canonical path (`/a/b` → `/a`, `/a` → `/`).
fn parent_of(canon_path: &str) -> String {
    match canon_path.rfind('/') {
        Some(0) | None => "/".to_string(),
        Some(i) => canon_path[..i].to_string(),
    }
}

impl MemBackend {
    pub fn new() -> Self {
        let mut nodes = BTreeMap::new();
        nodes.insert(
            "/".to_string(),
            Node { ino: 1, is_dir: true, is_symlink: false, data: Vec::new(), target: None },
        );
        let mut by_ino = BTreeMap::new();
        by_ino.insert(1u64, "/".to_string());
        MemBackend { nodes, by_ino, next_ino: 2, dirty: false, gen: 1 }
    }

    /// Test convenience: create a directory chain, panicking on error.
    pub fn mkdirs(&mut self, path: &str) {
        let c = canon(path).unwrap();
        let mut cur = String::new();
        for comp in c.split('/').filter(|s| !s.is_empty()) {
            cur.push('/');
            cur.push_str(comp);
            if !self.nodes.contains_key(&cur) {
                self.mkdir(&cur).unwrap();
            }
        }
    }

    fn alloc_ino(&mut self) -> u64 {
        let i = self.next_ino;
        self.next_ino += 1;
        i
    }

    fn parent_must_be_dir(&self, canon_path: &str) -> FsResult<()> {
        let parent = parent_of(canon_path);
        match self.nodes.get(&parent) {
            Some(n) if n.is_dir => Ok(()),
            Some(_) => Err(FsError::NotDir),
            None => Err(FsError::NotFound),
        }
    }
}

impl FsBackend for MemBackend {
    fn lookup(&mut self, path: &str) -> FsResult<u64> {
        let c = canon(path).ok_or(FsError::Invalid)?;
        self.nodes.get(&c).map(|n| n.ino).ok_or(FsError::NotFound)
    }

    fn stat(&mut self, path: &str) -> FsResult<InodeMeta> {
        let c = canon(path).ok_or(FsError::Invalid)?;
        let n = self.nodes.get(&c).ok_or(FsError::NotFound)?;
        Ok(InodeMeta {
            ino: n.ino,
            size: n.data.len() as u64,
            is_dir: n.is_dir,
            is_symlink: n.is_symlink,
            mode: if n.is_dir { 0o755 } else { 0o644 },
            uid: 0,
            gid: 0,
            nlink: 1,
            mtime: 0,
            ctime: 0,
        })
    }

    fn readlink(&mut self, path: &str) -> FsResult<String> {
        let c = canon(path).ok_or(FsError::Invalid)?;
        let n = self.nodes.get(&c).ok_or(FsError::NotFound)?;
        n.target.clone().ok_or(FsError::Invalid)
    }

    fn read_at(&mut self, ino: u64, off: u64, out: &mut [u8]) -> FsResult<usize> {
        let path = self.by_ino.get(&ino).ok_or(FsError::NotFound)?.clone();
        let n = self.nodes.get(&path).ok_or(FsError::NotFound)?;
        let off = off as usize;
        if off >= n.data.len() {
            return Ok(0);
        }
        let end = (off + out.len()).min(n.data.len());
        let len = end - off;
        out[..len].copy_from_slice(&n.data[off..end]);
        Ok(len)
    }

    fn write_at(&mut self, ino: u64, off: u64, data: &[u8]) -> FsResult<()> {
        let path = self.by_ino.get(&ino).ok_or(FsError::NotFound)?.clone();
        let n = self.nodes.get_mut(&path).ok_or(FsError::NotFound)?;
        if n.is_dir {
            return Err(FsError::IsDir);
        }
        let off = off as usize;
        if n.data.len() < off {
            n.data.resize(off, 0);
        }
        let end = off + data.len();
        if n.data.len() < end {
            n.data.resize(end, 0);
        }
        n.data[off..end].copy_from_slice(data);
        self.dirty = true;
        Ok(())
    }

    fn create(&mut self, path: &str) -> FsResult<u64> {
        let c = canon(path).ok_or(FsError::Invalid)?;
        if self.nodes.contains_key(&c) {
            return Err(FsError::Exists);
        }
        self.parent_must_be_dir(&c)?;
        let ino = self.alloc_ino();
        self.nodes.insert(
            c.clone(),
            Node { ino, is_dir: false, is_symlink: false, data: Vec::new(), target: None },
        );
        self.by_ino.insert(ino, c);
        self.dirty = true;
        Ok(ino)
    }

    fn mkdir(&mut self, path: &str) -> FsResult<u64> {
        let c = canon(path).ok_or(FsError::Invalid)?;
        if self.nodes.contains_key(&c) {
            return Err(FsError::Exists);
        }
        self.parent_must_be_dir(&c)?;
        let ino = self.alloc_ino();
        self.nodes.insert(
            c.clone(),
            Node { ino, is_dir: true, is_symlink: false, data: Vec::new(), target: None },
        );
        self.by_ino.insert(ino, c);
        self.dirty = true;
        Ok(ino)
    }

    fn unlink(&mut self, path: &str) -> FsResult<()> {
        let c = canon(path).ok_or(FsError::Invalid)?;
        let n = self.nodes.get(&c).ok_or(FsError::NotFound)?;
        if n.is_dir {
            return Err(FsError::IsDir);
        }
        let ino = n.ino;
        self.nodes.remove(&c);
        self.by_ino.remove(&ino);
        self.dirty = true;
        Ok(())
    }

    fn rename(&mut self, old: &str, new: &str) -> FsResult<()> {
        let o = canon(old).ok_or(FsError::Invalid)?;
        let nw = canon(new).ok_or(FsError::Invalid)?;
        if !self.nodes.contains_key(&o) {
            return Err(FsError::NotFound);
        }
        if self.nodes.contains_key(&nw) {
            return Err(FsError::Exists);
        }
        self.parent_must_be_dir(&nw)?;
        // Move the node and, for a directory, its whole subtree (prefix remap).
        let old_prefix = if o == "/" { "/".to_string() } else { alloc::format!("{o}/") };
        let moving: Vec<String> = self
            .nodes
            .keys()
            .filter(|k| **k == o || k.starts_with(&old_prefix))
            .cloned()
            .collect();
        for key in moving {
            let node = self.nodes.remove(&key).unwrap();
            let new_key = if key == o {
                nw.clone()
            } else {
                alloc::format!("{}{}", nw, &key[o.len()..])
            };
            self.by_ino.insert(node.ino, new_key.clone());
            self.nodes.insert(new_key, node);
        }
        self.dirty = true;
        Ok(())
    }

    fn readdir(&mut self, path: &str) -> FsResult<Vec<DirEntry>> {
        let c = canon(path).ok_or(FsError::Invalid)?;
        match self.nodes.get(&c) {
            Some(n) if n.is_dir => {}
            Some(_) => return Err(FsError::NotDir),
            None => return Err(FsError::NotFound),
        }
        let prefix = if c == "/" { "/".to_string() } else { alloc::format!("{c}/") };
        let mut out = Vec::new();
        for (k, n) in &self.nodes {
            if let Some(rest) = k.strip_prefix(&prefix) {
                if !rest.is_empty() && !rest.contains('/') {
                    out.push(DirEntry {
                        ino: n.ino,
                        file_type: if n.is_dir { 2 } else { 1 },
                        name: rest.to_string(),
                    });
                }
            }
        }
        Ok(out)
    }

    fn pin(&mut self, _ino: u64) -> FsResult<()> {
        Ok(())
    }
    fn unpin(&mut self, _ino: u64) -> FsResult<()> {
        Ok(())
    }
    fn commit(&mut self) -> FsResult<()> {
        if self.dirty {
            self.gen += 1;
            self.dirty = false;
        }
        Ok(())
    }
    fn has_staged_changes(&self) -> bool {
        self.dirty
    }
    fn generation(&self) -> u64 {
        self.gen
    }
}
