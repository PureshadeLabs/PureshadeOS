//! FFI types — `CStr`, `CString`, `OsStr`, `OsString`.
//!
//! `OsStr`/`OsString` are backed by UTF-8 on Lythos (there is no encoding
//! negotiation with a host OS).

use _alloc::string::String;
use _alloc::vec::Vec;
use _alloc::borrow::ToOwned;

// ── CStr / CString ────────────────────────────────────────────────────────────

/// A borrowed C-style string (NUL-terminated byte slice).
#[repr(transparent)]
pub struct CStr([u8]);

impl CStr {
    /// Wrap a byte slice that already ends with a NUL terminator.
    ///
    /// # Safety
    /// `bytes` must end with exactly one `\0` and contain no interior `\0`s.
    pub unsafe fn from_bytes_with_nul_unchecked(bytes: &[u8]) -> &Self {
        unsafe { &*(bytes as *const [u8] as *const CStr) }
    }

    pub fn from_bytes_with_nul(bytes: &[u8]) -> Result<&Self, CStrError> {
        if bytes.last() != Some(&0) {
            return Err(CStrError::NotNulTerminated);
        }
        if bytes[..bytes.len() - 1].contains(&0) {
            return Err(CStrError::InteriorNul);
        }
        Ok(unsafe { Self::from_bytes_with_nul_unchecked(bytes) })
    }

    pub fn to_bytes(&self) -> &[u8] {
        let b = &self.0;
        &b[..b.len() - 1]
    }

    pub fn to_bytes_with_nul(&self) -> &[u8] { &self.0 }

    pub fn to_str(&self) -> Result<&str, core::str::Utf8Error> {
        core::str::from_utf8(self.to_bytes())
    }
}

impl core::fmt::Debug for CStr {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "\"{}\"", self.to_str().unwrap_or("<invalid utf8>"))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CStrError {
    NotNulTerminated,
    InteriorNul,
}

/// An owned NUL-terminated C string.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CString(Vec<u8>);

impl CString {
    pub fn new(bytes: impl Into<Vec<u8>>) -> Result<Self, CStrError> {
        let mut v = bytes.into();
        if v.contains(&0) {
            return Err(CStrError::InteriorNul);
        }
        v.push(0);
        Ok(CString(v))
    }

    pub fn as_c_str(&self) -> &CStr {
        unsafe { CStr::from_bytes_with_nul_unchecked(&self.0) }
    }

    pub fn into_bytes(mut self) -> Vec<u8> {
        self.0.pop(); // drop nul
        self.0
    }

    pub fn as_bytes(&self) -> &[u8] { &self.0[..self.0.len() - 1] }
    pub fn as_bytes_with_nul(&self) -> &[u8] { &self.0 }
    pub fn to_str(&self) -> Result<&str, core::str::Utf8Error> {
        core::str::from_utf8(self.as_bytes())
    }
}

impl core::fmt::Debug for CString {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "\"{}\"", self.to_str().unwrap_or("<invalid utf8>"))
    }
}

// ── OsStr / OsString ─────────────────────────────────────────────────────────

/// A borrowed OS-native string.  On Lythos this is always valid UTF-8.
#[repr(transparent)]
pub struct OsStr(str);

impl OsStr {
    pub fn new(s: &str) -> &Self {
        unsafe { &*(s as *const str as *const OsStr) }
    }
    pub fn to_str(&self) -> Option<&str> { Some(&self.0) }
    pub fn len(&self) -> usize { self.0.len() }
    pub fn is_empty(&self) -> bool { self.0.is_empty() }
    pub fn to_os_string(&self) -> OsString { OsString(self.0.to_owned()) }
}

impl core::fmt::Debug for OsStr {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{:?}", &self.0)
    }
}

impl core::fmt::Display for OsStr {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.0)
    }
}

/// An owned OS-native string.  On Lythos this is always `String`.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OsString(String);

impl OsString {
    pub fn new() -> Self { OsString(String::new()) }
    pub fn from(s: &str) -> Self { OsString(s.to_owned()) }
    pub fn as_os_str(&self) -> &OsStr { OsStr::new(&self.0) }
    pub fn to_str(&self) -> Option<&str> { Some(&self.0) }
    pub fn into_string(self) -> Result<String, OsString> { Ok(self.0) }
    pub fn push(&mut self, s: &str) { self.0.push_str(s); }
    pub fn len(&self) -> usize { self.0.len() }
    pub fn is_empty(&self) -> bool { self.0.is_empty() }
    pub fn clear(&mut self) { self.0.clear(); }
}

impl Default for OsString {
    fn default() -> Self { Self::new() }
}

impl core::fmt::Debug for OsString {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{:?}", self.0)
    }
}

impl core::fmt::Display for OsString {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<String> for OsString {
    fn from(s: String) -> Self { OsString(s) }
}

impl From<&str> for OsString {
    fn from(s: &str) -> Self { OsString(s.to_owned()) }
}
