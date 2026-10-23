//! The std backend for seam.
//!
//! [`Std`] performs seam operations with real std calls. Construct it at
//! the edge of the program (usually `main`) and pass it into the core, whose
//! bounds decide what the core may do with it; or make it a part of a
//! `seam_core::Root`, next to other backends and the program's own effects.
//!
//! ```no_run
//! # #[cfg(all(feature = "fs", feature = "io-std", feature = "env"))] {
//! use std::io;
//! use seam_core::prelude::*;
//! use seam_core::ops::env::Args;
//! use seam_core::ops::fs::ReadToString;
//! use seam_core::ops::stdio::Print;
//! use seam_std::Std;
//!
//! /// The core: counts the lines of the file named on the command line.
//! /// Pure apart from the capabilities in its bounds.
//! fn count_lines<C>(c: &mut C) -> io::Result<()>
//! where
//!     C: Inspect<Args> + Inspect<ReadToString> + for<'a> Perform<Print<'a>>,
//! {
//!     let args = c.inspect(Args);
//!     let path = args.get(1).ok_or(io::ErrorKind::InvalidInput)?;
//!     let n = c.inspect(ReadToString::new(path))?.lines().count();
//!     c.perform(Print(format!("{n}\n").as_bytes()))
//! }
//!
//! /// The edge: the only place real effects are chosen.
//! fn main() -> io::Result<()> {
//!     count_lines(&mut Std::new())
//! }
//! # }
//! ```
//!
//! Test the same core against `seam_test::Sim` instead of `Std`.
//!
//! # Handles
//!
//! Files and sockets are newtypes (`fs::File`, `net::TcpStream`, ...)
//! that expose no std I/O traits and no way to get the std value back, so
//! every effect on them also goes through [`Perform`](seam_core::Perform)
//! or [`Inspect`](seam_core::Inspect). A core only ever borrows one inside
//! an [`acquire`](seam_core::Acquire::acquire) scope. When the scope
//! succeeds, a file is flushed and synced, a stream is shut down cleanly,
//! and `fs::AtomicFile` is renamed into place, and errors are returned;
//! otherwise they are just closed, and an atomic file's temporary file is
//! deleted.
//!
//! # Threads
//!
//! With `rt`, `Std` provides `Spawn` and `SpawnSet` with closures run on OS
//! threads, and a `net::TcpListener` provides `SpawnWith<Accept<_>, B, _>`
//! for a thread per connection (see the `task` module). Threads cannot be
//! aborted: a scope that fails waits for its threads instead.
//!
//! # Async
//!
//! Everything here is synchronous and blocking. Through seam's blanket
//! impls, `Std` also satisfies `AsyncPerform` and `SendPerform`, so an async
//! core can run on it with [`block_on`](seam_core::future::block_on); each
//! operation blocks the thread while it runs. For real async I/O, use
//! `seam-tokio`.
//!
//! # Features
//!
//! One per capability group: `fs`, `net`, `time`, `env`, `process`,
//! `random`, `io-std`, `sync`, `rt` (tasks on OS threads), or `full`. `Std` implements only the groups that are
//! enabled.
//!
//! `random` uses std's randomly keyed hasher, since std has no public random
//! number generator. It is fine for jitter, sampling and seeds, and is not
//! cryptographically secure.

/// The std backend's capability.
///
/// It holds no state. Every operation it supports is a real effect. It is
/// `Clone`, so a core can hand a copy to each thread or task it starts.
#[derive(Debug, Default, Clone)]
#[non_exhaustive]
pub struct Std;

impl Std {
    /// Creates it. Call this at the edge of the program.
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
pub mod process;
#[cfg(feature = "random")]
mod random;
#[cfg(feature = "io-std")]
mod stdio;
#[cfg(feature = "sync")]
pub mod sync;
#[cfg(feature = "rt")]
pub mod task;
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

// Every capability type opts in to seam's lifts.
impl seam_core::Capability for Std {}
