use crate::syscall::{ERR_MIN, ENOSYS, ENOCAP, ENOPERM, EINVAL};

/// A cask syscall error — a raw `u64` sentinel in the range
/// `[EINVAL, ENOSYS]` (i.e. `[0xFFFF_FFFF_FFFF_FFFC, 0xFFFF_FFFF_FFFF_FFFF]`).
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct SysError(pub u64);

impl SysError {
    pub const ENOSYS:  Self = Self(ENOSYS);
    pub const ENOCAP:  Self = Self(ENOCAP);
    pub const ENOPERM: Self = Self(ENOPERM);
    pub const EINVAL:  Self = Self(EINVAL);

    /// Return `true` if `val` is a syscall error sentinel.
    #[inline]
    pub fn is_err(val: u64) -> bool {
        val >= ERR_MIN
    }

    /// Convert a raw syscall return value into `Ok(val)` or `Err(SysError)`.
    #[inline]
    pub fn from_raw(val: u64) -> Result<u64, Self> {
        if Self::is_err(val) { Err(Self(val)) } else { Ok(val) }
    }

    pub fn raw(self) -> u64 { self.0 }
}

impl core::fmt::Debug for SysError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self.0 {
            ENOSYS  => f.write_str("ENOSYS"),
            ENOCAP  => f.write_str("ENOCAP"),
            ENOPERM => f.write_str("ENOPERM"),
            EINVAL  => f.write_str("EINVAL"),
            v       => write!(f, "SysError({:#x})", v),
        }
    }
}
