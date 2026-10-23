//! seam's I/O error: one type, with or without std.
//!
//! [`Error`] is a kind and a static message, so making one allocates
//! nothing and needs no std. With the `std` feature it also converts to and
//! from `std::io::Error` (keeping the original inside, so nothing about an
//! OS error is lost), so `?` works between the two in both directions.
//!
//! The feature only adds conversions and methods; the type is the same
//! with and without it. A no_std crate and a std crate in one build
//! therefore always agree on it.
//!
//! ```
//! use seam_core::io::{Error, ErrorKind, Result};
//!
//! fn parse_port(s: &str) -> Result<u16> {
//!     s.parse().map_err(|_| Error::new(ErrorKind::InvalidInput, "not a port number"))
//! }
//!
//! assert_eq!(parse_port("80").unwrap(), 80);
//! assert_eq!(parse_port("http").unwrap_err().kind(), ErrorKind::InvalidInput);
//! ```

use core::fmt;

/// A result whose error is seam's [`Error`].
pub type Result<T> = core::result::Result<T, Error>;

/// What went wrong, in general terms. The same kinds as `std::io::ErrorKind`,
/// under the same names, for the ones seam's operations report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum ErrorKind {
    /// Nothing is there.
    NotFound,
    /// The operation is not allowed.
    PermissionDenied,
    /// The remote end refused the connection.
    ConnectionRefused,
    /// The remote end reset the connection.
    ConnectionReset,
    /// The connection was aborted.
    ConnectionAborted,
    /// Not connected.
    NotConnected,
    /// The address is already in use.
    AddrInUse,
    /// The address is not available here.
    AddrNotAvailable,
    /// The other end of a pipe or stream is gone.
    BrokenPipe,
    /// Something is already there.
    AlreadyExists,
    /// The operation would have to wait.
    WouldBlock,
    /// A directory was expected.
    NotADirectory,
    /// A file was expected, but it is a directory.
    IsADirectory,
    /// The directory is not empty.
    DirectoryNotEmpty,
    /// An argument was not valid.
    InvalidInput,
    /// The data was not valid.
    InvalidData,
    /// The operation gave up waiting.
    TimedOut,
    /// A write wrote nothing.
    WriteZero,
    /// The operation was interrupted, and can be retried.
    Interrupted,
    /// The operation is not supported here.
    Unsupported,
    /// The data ended too soon.
    UnexpectedEof,
    /// Memory ran out.
    OutOfMemory,
    /// Anything else.
    Other,
}

impl ErrorKind {
    /// A short description, as `std::io::ErrorKind` gives.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotFound => "entity not found",
            Self::PermissionDenied => "permission denied",
            Self::ConnectionRefused => "connection refused",
            Self::ConnectionReset => "connection reset",
            Self::ConnectionAborted => "connection aborted",
            Self::NotConnected => "not connected",
            Self::AddrInUse => "address in use",
            Self::AddrNotAvailable => "address not available",
            Self::BrokenPipe => "broken pipe",
            Self::AlreadyExists => "entity already exists",
            Self::WouldBlock => "operation would block",
            Self::NotADirectory => "not a directory",
            Self::IsADirectory => "is a directory",
            Self::DirectoryNotEmpty => "directory not empty",
            Self::InvalidInput => "invalid input parameter",
            Self::InvalidData => "invalid data",
            Self::TimedOut => "timed out",
            Self::WriteZero => "write zero",
            Self::Interrupted => "operation interrupted",
            Self::Unsupported => "unsupported",
            Self::UnexpectedEof => "unexpected end of file",
            Self::OutOfMemory => "out of memory",
            Self::Other => "other error",
        }
    }
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// An I/O error: a kind and a static message, and with std, the
/// `std::io::Error` it came from.
pub struct Error {
    kind: ErrorKind,
    message: &'static str,
    #[cfg(feature = "std")]
    source: Option<std::io::Error>,
}

impl Error {
    /// An error of `kind`, described by `message`.
    pub const fn new(kind: ErrorKind, message: &'static str) -> Self {
        Self {
            kind,
            message,
            #[cfg(feature = "std")]
            source: None,
        }
    }

    /// An error of kind `Other`, described by `message`.
    pub const fn other(message: &'static str) -> Self {
        Self::new(ErrorKind::Other, message)
    }

    /// What kind of error this is.
    pub const fn kind(&self) -> ErrorKind {
        self.kind
    }

    /// The `std::io::Error` this came from, if it came from one.
    #[cfg(feature = "std")]
    pub fn as_std(&self) -> Option<&std::io::Error> {
        self.source.as_ref()
    }

    /// Converts into a `std::io::Error`: the one this came from, if any.
    #[cfg(feature = "std")]
    pub fn into_std(self) -> std::io::Error {
        match self.source {
            Some(e) => e,
            None if self.message.is_empty() => std_kind(self.kind).into(),
            None => std::io::Error::new(std_kind(self.kind), self.message),
        }
    }
}

impl From<ErrorKind> for Error {
    fn from(kind: ErrorKind) -> Self {
        Self::new(kind, "")
    }
}

impl fmt::Debug for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut d = f.debug_struct("Error");
        d.field("kind", &self.kind);
        if !self.message.is_empty() {
            d.field("message", &self.message);
        }
        #[cfg(feature = "std")]
        if let Some(source) = &self.source {
            d.field("source", source);
        }
        d.finish()
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        #[cfg(feature = "std")]
        if let Some(source) = &self.source {
            return fmt::Display::fmt(source, f);
        }
        if self.message.is_empty() {
            f.write_str(self.kind.as_str())
        } else {
            f.write_str(self.message)
        }
    }
}

impl core::error::Error for Error {
    #[cfg(feature = "std")]
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        self.source
            .as_ref()
            .map(|e| e as &(dyn core::error::Error + 'static))
    }
}

#[cfg(feature = "std")]
fn std_kind(kind: ErrorKind) -> std::io::ErrorKind {
    use std::io::ErrorKind as S;
    match kind {
        ErrorKind::NotFound => S::NotFound,
        ErrorKind::PermissionDenied => S::PermissionDenied,
        ErrorKind::ConnectionRefused => S::ConnectionRefused,
        ErrorKind::ConnectionReset => S::ConnectionReset,
        ErrorKind::ConnectionAborted => S::ConnectionAborted,
        ErrorKind::NotConnected => S::NotConnected,
        ErrorKind::AddrInUse => S::AddrInUse,
        ErrorKind::AddrNotAvailable => S::AddrNotAvailable,
        ErrorKind::BrokenPipe => S::BrokenPipe,
        ErrorKind::AlreadyExists => S::AlreadyExists,
        ErrorKind::WouldBlock => S::WouldBlock,
        ErrorKind::NotADirectory => S::NotADirectory,
        ErrorKind::IsADirectory => S::IsADirectory,
        ErrorKind::DirectoryNotEmpty => S::DirectoryNotEmpty,
        ErrorKind::InvalidInput => S::InvalidInput,
        ErrorKind::InvalidData => S::InvalidData,
        ErrorKind::TimedOut => S::TimedOut,
        ErrorKind::WriteZero => S::WriteZero,
        ErrorKind::Interrupted => S::Interrupted,
        ErrorKind::Unsupported => S::Unsupported,
        ErrorKind::UnexpectedEof => S::UnexpectedEof,
        ErrorKind::OutOfMemory => S::OutOfMemory,
        ErrorKind::Other => S::Other,
    }
}

#[cfg(feature = "std")]
fn seam_kind(kind: std::io::ErrorKind) -> ErrorKind {
    use std::io::ErrorKind as S;
    match kind {
        S::NotFound => ErrorKind::NotFound,
        S::PermissionDenied => ErrorKind::PermissionDenied,
        S::ConnectionRefused => ErrorKind::ConnectionRefused,
        S::ConnectionReset => ErrorKind::ConnectionReset,
        S::ConnectionAborted => ErrorKind::ConnectionAborted,
        S::NotConnected => ErrorKind::NotConnected,
        S::AddrInUse => ErrorKind::AddrInUse,
        S::AddrNotAvailable => ErrorKind::AddrNotAvailable,
        S::BrokenPipe => ErrorKind::BrokenPipe,
        S::AlreadyExists => ErrorKind::AlreadyExists,
        S::WouldBlock => ErrorKind::WouldBlock,
        S::NotADirectory => ErrorKind::NotADirectory,
        S::IsADirectory => ErrorKind::IsADirectory,
        S::DirectoryNotEmpty => ErrorKind::DirectoryNotEmpty,
        S::InvalidInput => ErrorKind::InvalidInput,
        S::InvalidData => ErrorKind::InvalidData,
        S::TimedOut => ErrorKind::TimedOut,
        S::WriteZero => ErrorKind::WriteZero,
        S::Interrupted => ErrorKind::Interrupted,
        S::Unsupported => ErrorKind::Unsupported,
        S::UnexpectedEof => ErrorKind::UnexpectedEof,
        S::OutOfMemory => ErrorKind::OutOfMemory,
        _ => ErrorKind::Other,
    }
}

/// Keeps the original: nothing about an OS error is lost.
#[cfg(feature = "std")]
impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self {
            kind: seam_kind(e.kind()),
            message: "",
            source: Some(e),
        }
    }
}

#[cfg(feature = "std")]
impl From<Error> for std::io::Error {
    fn from(e: Error) -> Self {
        e.into_std()
    }
}

#[cfg(feature = "std")]
impl From<ErrorKind> for std::io::ErrorKind {
    fn from(kind: ErrorKind) -> Self {
        std_kind(kind)
    }
}

#[cfg(feature = "std")]
impl From<std::io::ErrorKind> for ErrorKind {
    fn from(kind: std::io::ErrorKind) -> Self {
        seam_kind(kind)
    }
}

#[cfg(all(test, feature = "std"))]
mod tests {
    use std::string::ToString;

    use super::*;

    #[test]
    fn std_errors_round_trip() {
        let os = std::io::Error::from_raw_os_error(2); // ENOENT on unix
        let seam = Error::from(os);
        assert_eq!(seam.kind(), ErrorKind::NotFound);
        let back = seam.into_std();
        assert_eq!(back.raw_os_error(), Some(2), "the original is kept");

        let made = Error::new(ErrorKind::TimedOut, "gave up");
        let std = std::io::Error::from(made);
        assert_eq!(std.kind(), std::io::ErrorKind::TimedOut);
        assert_eq!(std.to_string(), "gave up");
    }

    #[test]
    fn every_kind_maps_to_std_and_back() {
        use ErrorKind::*;
        for kind in [
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
            NotADirectory,
            IsADirectory,
            DirectoryNotEmpty,
            InvalidInput,
            InvalidData,
            TimedOut,
            WriteZero,
            Interrupted,
            Unsupported,
            UnexpectedEof,
            OutOfMemory,
            Other,
        ] {
            assert_eq!(seam_kind(std_kind(kind)), kind);
            assert_eq!(
                std_kind(kind).to_string(),
                kind.as_str(),
                "the same descriptions as std"
            );
        }
    }
}
