//! `MemFs` — a pure in-memory [`StoreFs`] for the seam tests. Deliberately
//! strict where the OROS backend is strict (`write_file` is exclusive-create,
//! like `SYS_CREATE`; parents must exist), and it logs every mutation so
//! tests can prove rename-as-sole-seal.

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::backend::{split_parent, FsError, FsResult, NodeKind, NodeMeta, StoreFs};

#[derive(Debug, Clone)]
enum Node {
    File { data: Vec<u8>, exec: bool },
    Dir,
    Symlink(String),
}

#[derive(Debug, Default)]
pub struct MemFs {
    nodes: BTreeMap<String, Node>,
    /// Mutation log: `(op, target_path)`. Rename logs both `rename_src` and
    /// `rename_dst`.
    pub ops: Vec<(String, String)>,
    token: u64,
}

impl MemFs {
    pub fn new() -> Self {
        let mut fs = MemFs::default();
        fs.nodes.insert(String::from("/"), Node::Dir);
        fs
    }

    fn log(&mut self, op: &str, target: &str) {
        self.ops.push((String::from(op), String::from(target)));
    }

    fn require_parent_dir(&self, path: &str) -> FsResult<()> {
        let (parent, _) = split_parent(path);
        match self.nodes.get(parent) {
            Some(Node::Dir) => Ok(()),
            Some(_) => Err(FsError::NotDir),
            None => Err(FsError::NotFound),
        }
    }
}

impl StoreFs for MemFs {
    fn metadata(&mut self, path: &str) -> FsResult<NodeMeta> {
        match self.nodes.get(path) {
            Some(Node::File { data, exec }) => Ok(NodeMeta {
                kind: NodeKind::File,
                exec: *exec,
                len: data.len() as u64,
            }),
            Some(Node::Dir) => Ok(NodeMeta { kind: NodeKind::Dir, exec: false, len: 0 }),
            Some(Node::Symlink(_)) => Ok(NodeMeta { kind: NodeKind::Symlink, exec: false, len: 0 }),
            None => Err(FsError::NotFound),
        }
    }

    fn read_file(&mut self, path: &str) -> FsResult<Vec<u8>> {
        match self.nodes.get(path) {
            Some(Node::File { data, .. }) => Ok(data.clone()),
            Some(Node::Dir) => Err(FsError::IsDir),
            Some(Node::Symlink(_)) => Err(FsError::Invalid),
            None => Err(FsError::NotFound),
        }
    }

    /// Exclusive-create, like `SYS_CREATE`: an existing path is `Exists`,
    /// never truncated.
    fn write_file(&mut self, path: &str, data: &[u8], exec: bool) -> FsResult<()> {
        if self.nodes.contains_key(path) {
            return Err(FsError::Exists);
        }
        self.require_parent_dir(path)?;
        self.log("write", path);
        self.nodes
            .insert(String::from(path), Node::File { data: data.to_vec(), exec });
        Ok(())
    }

    /// Same as [`write_file`](StoreFs::write_file) — MemFs is already
    /// exclusive-create, like `SYS_CREATE`.
    fn create_exclusive(&mut self, path: &str, data: &[u8]) -> FsResult<()> {
        self.write_file(path, data, false)
    }

    fn mkdir(&mut self, path: &str) -> FsResult<()> {
        if self.nodes.contains_key(path) {
            return Err(FsError::Exists);
        }
        self.require_parent_dir(path)?;
        self.log("mkdir", path);
        self.nodes.insert(String::from(path), Node::Dir);
        Ok(())
    }

    fn rename(&mut self, old: &str, new: &str) -> FsResult<()> {
        if !self.nodes.contains_key(old) {
            return Err(FsError::NotFound);
        }
        if self.nodes.contains_key(new) {
            return Err(FsError::Exists);
        }
        self.require_parent_dir(new)?;
        self.log("rename_src", old);
        self.log("rename_dst", new);
        // Move the node and, for a dir, its whole subtree.
        let old_prefix = format!("{old}/");
        let moved: Vec<String> = self
            .nodes
            .keys()
            .filter(|k| *k == old || k.starts_with(&old_prefix))
            .cloned()
            .collect();
        for k in moved {
            let node = self.nodes.remove(&k).unwrap();
            let nk = format!("{new}{}", &k[old.len()..]);
            self.nodes.insert(nk, node);
        }
        Ok(())
    }

    fn unlink(&mut self, path: &str) -> FsResult<()> {
        match self.nodes.get(path) {
            Some(Node::Dir) => Err(FsError::IsDir),
            Some(_) => {
                self.log("unlink", path);
                self.nodes.remove(path);
                Ok(())
            }
            None => Err(FsError::NotFound),
        }
    }

    fn rmdir(&mut self, path: &str) -> FsResult<()> {
        match self.nodes.get(path) {
            Some(Node::Dir) => {
                let prefix = format!("{path}/");
                if self.nodes.keys().any(|k| k.starts_with(&prefix)) {
                    return Err(FsError::NotEmpty);
                }
                self.log("rmdir", path);
                self.nodes.remove(path);
                Ok(())
            }
            Some(_) => Err(FsError::NotDir),
            None => Err(FsError::NotFound),
        }
    }

    fn read_dir(&mut self, path: &str) -> FsResult<Vec<(String, NodeKind)>> {
        match self.nodes.get(path) {
            Some(Node::Dir) => {}
            Some(_) => return Err(FsError::NotDir),
            None => return Err(FsError::NotFound),
        }
        let prefix = if path == "/" { String::from("/") } else { format!("{path}/") };
        let mut out = Vec::new();
        for (k, node) in self.nodes.range(prefix.clone()..) {
            let Some(rest) = k.strip_prefix(&prefix) else { break };
            if rest.is_empty() || rest.contains('/') {
                continue;
            }
            let kind = match node {
                Node::File { .. } => NodeKind::File,
                Node::Dir => NodeKind::Dir,
                Node::Symlink(_) => NodeKind::Symlink,
            };
            out.push((String::from(rest), kind));
        }
        Ok(out)
    }

    fn read_link(&mut self, path: &str) -> FsResult<String> {
        match self.nodes.get(path) {
            Some(Node::Symlink(t)) => Ok(t.clone()),
            Some(_) => Err(FsError::Invalid),
            None => Err(FsError::NotFound),
        }
    }

    fn symlink(&mut self, target: &str, link: &str) -> FsResult<()> {
        if self.nodes.contains_key(link) {
            return Err(FsError::Exists);
        }
        self.require_parent_dir(link)?;
        self.log("symlink", link);
        self.nodes.insert(String::from(link), Node::Symlink(String::from(target)));
        Ok(())
    }

    fn unique_token(&mut self) -> u64 {
        self.token += 1;
        self.token
    }
}
