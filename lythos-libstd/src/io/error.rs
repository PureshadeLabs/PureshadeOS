use _alloc::string::String;
use _alloc::boxed::Box;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorKind {
    NotFound,
    PermissionDenied,
    ConnectionRefused,
    ConnectionReset,
    ConnectionAborted,
    NotConnected,
    AddrInUse,
    AddrNotAvailable,
    BrokenPipe,
    AlreadyExists,
    WouldBlock,
    InvalidInput,
    InvalidData,
    TimedOut,
    WriteZero,
    Interrupted,
    Unsupported,
    UnexpectedEof,
    OutOfMemory,
    Other,
}

impl ErrorKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotFound          => "entity not found",
            Self::PermissionDenied  => "permission denied",
            Self::ConnectionRefused => "connection refused",
            Self::ConnectionReset   => "connection reset",
            Self::ConnectionAborted => "connection aborted",
            Self::NotConnected      => "not connected",
            Self::AddrInUse         => "address in use",
            Self::AddrNotAvailable  => "address not available",
            Self::BrokenPipe        => "broken pipe",
            Self::AlreadyExists     => "entity already exists",
            Self::WouldBlock        => "operation would block",
            Self::InvalidInput      => "invalid input",
            Self::InvalidData       => "invalid data",
            Self::TimedOut          => "timed out",
            Self::WriteZero         => "write zero",
            Self::Interrupted       => "operation interrupted",
            Self::Unsupported       => "unsupported",
            Self::UnexpectedEof     => "unexpected end of file",
            Self::OutOfMemory       => "out of memory",
            Self::Other             => "other error",
        }
    }
}

impl core::fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Lythos I/O error, mirroring `std::io::Error`.
pub struct Error {
    kind: ErrorKind,
    msg:  Option<String>,
}

impl Error {
    pub const INVALID_DATA: Error = Error { kind: ErrorKind::InvalidData, msg: None };

    pub fn new(kind: ErrorKind, msg: &str) -> Self {
        Error { kind, msg: Some(String::from(msg)) }
    }

    pub fn from_kernel(e: lythos_std::error::SysError) -> Self {
        use lythos_std::syscall::{ENOCAP, ENOPERM, EINVAL};
        let kind = match e.raw() {
            ENOCAP  => ErrorKind::PermissionDenied,
            ENOPERM => ErrorKind::PermissionDenied,
            EINVAL  => ErrorKind::InvalidInput,
            _       => ErrorKind::Other,
        };
        Error { kind, msg: None }
    }

    pub fn kind(&self) -> ErrorKind { self.kind }
}

impl core::fmt::Debug for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match &self.msg {
            Some(m) => write!(f, "io::Error({:?}: {})", self.kind, m),
            None    => write!(f, "io::Error({:?})", self.kind),
        }
    }
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match &self.msg {
            Some(m) => write!(f, "{}: {}", self.kind.as_str(), m),
            None    => f.write_str(self.kind.as_str()),
        }
    }
}

pub type Result<T> = core::result::Result<T, Error>;
