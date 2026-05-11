//! Path manipulation — no filesystem syscalls, pure string operations.
//!
//! Lythos uses POSIX-style forward-slash paths.  This is a minimal but
//! correct implementation; expand as needed.

use _alloc::string::String;
use _alloc::vec::Vec;
use crate::ffi::{OsStr, OsString};

// ── Path ──────────────────────────────────────────────────────────────────────

#[repr(transparent)]
pub struct Path(str);

impl Path {
    pub fn new(s: &str) -> &Self {
        unsafe { &*(s as *const str as *const Path) }
    }

    pub fn as_str(&self) -> &str { &self.0 }
    pub fn as_os_str(&self) -> &OsStr { OsStr::new(&self.0) }
    pub fn to_path_buf(&self) -> PathBuf { PathBuf(self.0.to_string_lossy_owned()) }
    pub fn is_absolute(&self) -> bool { self.0.starts_with('/') }
    pub fn is_relative(&self) -> bool { !self.is_absolute() }
    pub fn has_root(&self) -> bool { self.0.starts_with('/') }

    pub fn file_name(&self) -> Option<&str> {
        let s = self.0.trim_end_matches('/');
        s.rfind('/').map(|i| &s[i + 1..]).or(if s.is_empty() { None } else { Some(s) })
    }

    pub fn extension(&self) -> Option<&str> {
        self.file_name().and_then(|name| {
            let dot = name.rfind('.')?;
            if dot == 0 { None } else { Some(&name[dot + 1..]) }
        })
    }

    pub fn file_stem(&self) -> Option<&str> {
        self.file_name().map(|name| {
            match name.rfind('.') {
                Some(dot) if dot > 0 => &name[..dot],
                _ => name,
            }
        })
    }

    pub fn parent(&self) -> Option<&Path> {
        let s = self.0.trim_end_matches('/');
        let i = s.rfind('/')?;
        let parent = &s[..i];
        Some(Path::new(if parent.is_empty() { "/" } else { parent }))
    }

    pub fn join(&self, child: &str) -> PathBuf {
        let mut buf = PathBuf::from(self.as_str());
        buf.push(child);
        buf
    }

    pub fn components(&self) -> Components<'_> {
        Components { path: self.0.trim_start_matches('/'), leading_sep: self.is_absolute() }
    }

    pub fn display(&self) -> &str { &self.0 }
}

impl core::fmt::Debug for Path {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{:?}", &self.0)
    }
}

impl core::fmt::Display for Path {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.0)
    }
}

impl PartialEq for Path {
    fn eq(&self, other: &Self) -> bool { self.0 == other.0 }
}

trait ToStringLossyOwned { fn to_string_lossy_owned(&self) -> String; }
impl ToStringLossyOwned for str {
    fn to_string_lossy_owned(&self) -> String { String::from(self) }
}

// ── PathBuf ───────────────────────────────────────────────────────────────────

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PathBuf(String);

impl PathBuf {
    pub fn new() -> Self { PathBuf(String::new()) }
    pub fn from(s: &str) -> Self { PathBuf(String::from(s)) }
    pub fn with_capacity(cap: usize) -> Self { PathBuf(String::with_capacity(cap)) }

    pub fn as_path(&self) -> &Path { Path::new(&self.0) }
    pub fn as_str(&self) -> &str { &self.0 }

    pub fn push(&mut self, p: &str) {
        if p.starts_with('/') {
            self.0 = String::from(p);
        } else {
            if !self.0.ends_with('/') && !self.0.is_empty() {
                self.0.push('/');
            }
            self.0.push_str(p);
        }
    }

    pub fn pop(&mut self) -> bool {
        let s = self.0.trim_end_matches('/');
        if let Some(i) = s.rfind('/') {
            self.0.truncate(i.max(1));
            true
        } else if !s.is_empty() {
            self.0.clear();
            true
        } else {
            false
        }
    }

    pub fn set_file_name(&mut self, name: &str) { self.pop(); self.push(name); }

    pub fn into_os_string(self) -> OsString { OsString::from(self.0) }
}

impl Default for PathBuf {
    fn default() -> Self { Self::new() }
}

impl core::ops::Deref for PathBuf {
    type Target = Path;
    fn deref(&self) -> &Path { self.as_path() }
}

impl core::fmt::Debug for PathBuf {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{:?}", self.0)
    }
}

impl core::fmt::Display for PathBuf {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<String> for PathBuf {
    fn from(s: String) -> Self { PathBuf(s) }
}

impl From<&str> for PathBuf {
    fn from(s: &str) -> Self { PathBuf::from(s) }
}

// ── Components ────────────────────────────────────────────────────────────────

pub struct Components<'a> {
    path: &'a str,
    leading_sep: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Component<'a> {
    RootDir,
    CurDir,
    ParentDir,
    Normal(&'a str),
}

impl<'a> Iterator for Components<'a> {
    type Item = Component<'a>;
    fn next(&mut self) -> Option<Component<'a>> {
        if self.leading_sep {
            self.leading_sep = false;
            return Some(Component::RootDir);
        }
        self.path = self.path.trim_start_matches('/');
        if self.path.is_empty() { return None; }
        let (part, rest) = match self.path.find('/') {
            Some(i) => (&self.path[..i], &self.path[i + 1..]),
            None    => (self.path, ""),
        };
        self.path = rest;
        Some(match part {
            "."  => Component::CurDir,
            ".." => Component::ParentDir,
            n    => Component::Normal(n),
        })
    }
}
