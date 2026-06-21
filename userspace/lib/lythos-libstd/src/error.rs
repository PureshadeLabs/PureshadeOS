//! The `Error` trait and blanket implementations.

use _alloc::boxed::Box;
use _alloc::string::String;

/// The `Error` trait, equivalent to `std::error::Error`.
///
/// All types that represent an error condition should implement this.
pub trait Error: core::fmt::Debug + core::fmt::Display {
    /// The lower-level source of this error, if any.
    fn source(&self) -> Option<&(dyn Error + 'static)> { None }

    /// Deprecated — use `Display` for user-facing messages.
    fn description(&self) -> &str { "error" }
}

// ── Blanket impls ─────────────────────────────────────────────────────────────

impl<'a> Error for &'a str {}
impl Error for String {}
impl Error for core::convert::Infallible {}
impl Error for core::fmt::Error {}
impl Error for core::num::ParseIntError {}
impl Error for core::num::ParseFloatError {}
impl Error for core::str::Utf8Error {}
impl Error for core::char::TryFromCharError {}

impl Error for _alloc::string::FromUtf8Error {}

// Box<dyn Error> impls.
impl<T: Error + 'static> From<T> for Box<dyn Error + 'static> {
    fn from(e: T) -> Box<dyn Error + 'static> { Box::new(e) }
}

impl<T: Error + Send + Sync + 'static> From<T> for Box<dyn Error + Send + Sync + 'static> {
    fn from(e: T) -> Box<dyn Error + Send + Sync + 'static> { Box::new(e) }
}
