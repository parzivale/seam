//! The std backend for seam.
//!
//! [`Std`] is the root capability: it performs seam operations with real
//! std calls. Construct it once at the edge of the program (usually `main`)
//! and pass it, or narrower views of it, into the core as generic bounds.
//!
//! Handles that std would hand out (files, sockets) are wrapped in newtypes
//! that expose no std I/O traits and no way to get the std value back, so
//! every effect on them goes through [`Perform`](seam_core::Perform) or
//! [`Inspect`](seam_core::Inspect).
//!
//! Each capability group is a feature: `fs`, `net`, `time`, `env`,
//! `process`, `random`, `io-std`, or `full` for all of them.

/// The root capability over std.
///
/// It holds no state. Every operation it supports is a real effect.
#[derive(Debug, Default)]
#[non_exhaustive]
pub struct Std;

impl Std {
    /// Creates the root capability. Call this at the edge of the program.
    pub fn new() -> Self {
        Self
    }
}

#[cfg(feature = "env")]
mod env;
#[cfg(feature = "fs")]
pub mod fs;
#[cfg(feature = "net")]
pub mod net;
#[cfg(feature = "process")]
mod process;
#[cfg(feature = "random")]
mod random;
#[cfg(feature = "io-std")]
mod stdio;
#[cfg(feature = "time")]
mod time;

/// Implements the handle-level `io` operations for a wrapper around a std
/// type that implements `Read` and `Write`.
#[cfg(any(feature = "fs", feature = "net"))]
macro_rules! io_ops {
    ($ty:ty) => {
        impl seam_core::Perform<seam_core::ops::io::Read<'_>> for $ty {
            fn perform(&mut self, op: seam_core::ops::io::Read<'_>) -> std::io::Result<usize> {
                std::io::Read::read(&mut self.0, op.0)
            }
        }

        impl seam_core::Perform<seam_core::ops::io::ReadToEnd<'_>> for $ty {
            fn perform(&mut self, op: seam_core::ops::io::ReadToEnd<'_>) -> std::io::Result<usize> {
                std::io::Read::read_to_end(&mut self.0, op.0)
            }
        }

        impl seam_core::Perform<seam_core::ops::io::Write<'_>> for $ty {
            fn perform(&mut self, op: seam_core::ops::io::Write<'_>) -> std::io::Result<usize> {
                std::io::Write::write(&mut self.0, op.0)
            }
        }

        impl seam_core::Perform<seam_core::ops::io::WriteAll<'_>> for $ty {
            fn perform(&mut self, op: seam_core::ops::io::WriteAll<'_>) -> std::io::Result<()> {
                std::io::Write::write_all(&mut self.0, op.0)
            }
        }

        impl seam_core::Perform<seam_core::ops::io::Flush> for $ty {
            fn perform(&mut self, _: seam_core::ops::io::Flush) -> std::io::Result<()> {
                std::io::Write::flush(&mut self.0)
            }
        }
    };
}
#[cfg(any(feature = "fs", feature = "net"))]
use io_ops;
