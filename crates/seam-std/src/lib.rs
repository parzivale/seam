//! The std backend for seam.
//!
//! [`Std`] performs seam operations with real std calls. Construct it at
//! the edge of the program (usually `main`) and pass it into the core, whose
//! bounds decide what the core may do with it; or make it a part of a
//! `seam_core::Root`, next to other backends and the program's own effects.
//!
//! ```no_run
//! # #[cfg(all(feature = "fs", feature = "io-std", feature = "env"))] {
//! use seam_core::io;
//! use seam_core::prelude::*;
//! use seam_core::ops::env::Args;
//! use seam_core::ops::fs::{ReadAt, View, read_to_string};
//! use seam_core::ops::io::Write;
//! use seam_core::ops::stdio::{OpenStdout, print};
//! use seam_std::Std;
//!
//! /// The core: counts the lines of the file named on the command line.
//! /// Pure apart from the capabilities in its bounds: it can read its
//! /// arguments, read files and print.
//! fn count_lines<C, F, O>(c: &mut C) -> io::Result<()>
//! where
//!     C: Inspect<Args> + for<'a> Lend<View<'a, F>> + Provide<OpenStdout<O>>,
//!     F: for<'a> Inspect<ReadAt<'a>>,
//!     O: for<'a> Perform<Write<'a>>,
//! {
//!     let args = c.inspect(Args);
//!     let path = args.get(1).ok_or(io::ErrorKind::InvalidInput)?;
//!     let n = read_to_string(c, path)?.lines().count();
//!     print(c, format!("{n}\n").as_bytes())
//! }
//!
//! /// The edge: the only place real effects are chosen.
//! fn main() -> io::Result<()> {
//!     count_lines(&mut Std)
//! }
//! # }
//! ```
//!
//! Test the same core against `seam_test::Sim` instead of `Std`.
//!
//! # Handles
//!
//! Files, sockets and the standard streams are newtypes (`fs::File<M>`,
//! `net::TcpStream`, `stdio::Stdout`, ...) that expose no std I/O traits
//! and no way to get the std value back, so
//! every effect on them also goes through [`Perform`](seam_core::Perform)
//! or [`Inspect`](seam_core::Inspect). A core only ever borrows one inside
//! an [`acquire`](seam_core::Provide::acquire) scope. When the scope
//! succeeds, a file is flushed and synced, a stream is shut down cleanly,
//! and `fs::AtomicFile` is renamed into place, and errors are returned;
//! otherwise they are just closed, and an atomic file's temporary file is
//! deleted. Inside the scope a file can also `Seek`, `SetLen`, `SyncAll`,
//! `SyncData` and `Stat`, and `ReadDir` lends an `fs::Dir` to step through
//! with `NextEntry`.
//!
//! `Split` lends a stream as `net::TcpReadHalf` and `net::TcpWriteHalf`
//! (duplicates of the socket, so a failed split loses nothing), and
//! `SplitChild` lends a child's pipes as `process::ChildStdin`,
//! `ChildStdout` and `ChildStderr`, to use from scoped threads at once.
//!
//! # Changing the environment
//!
//! `Std` reads the environment but does not change it, since
//! `std::env::set_var` is `unsafe`: another thread reading the environment
//! meanwhile is undefined behaviour. `env::WritableEnv` performs `SetVar`
//! and `RemoveVar` as well as every read; it is made with an `unsafe fn
//! new()` that carries `set_var`'s contract, and cannot leave its thread.
//! Put it in a root for `effects::SetVar` and `effects::RemoveVar`.
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
//! Nothing stops an async core on a runtime from using `Std`, and that is
//! fine for operations that never wait: `Now`, `WallClock`, `Var`, `Args`,
//! `CurrentDir`, `TempDir`, `SetVar`, `RemoveVar`, `FillRandom` and `Exit`.
//! Everything else (files, sockets, processes, stdio, sleeping, channels,
//! threads, DNS) blocks the runtime's thread for as long as it takes, which
//! stalls every task on that thread. Under a runtime, take those from
//! `seam-tokio`.
//!
//! # Features
//!
//! One per capability group: `fs`, `net`, `time`, `env`, `process`,
//! `random`, `io-std`, `sync`, `rt` (tasks on OS threads), or `full`. `Std`
//! implements only the groups that are enabled.
//!
//! `random` uses std's randomly keyed hasher, since std has no public random
//! number generator. It is fine for jitter, sampling and seeds, and is not
//! cryptographically secure.

/// The std backend's capability.
///
/// It holds no state. Every operation it supports is a real effect. It is
/// `Clone`, so a core can hand a copy to each thread or task it starts.
#[derive(Debug, Default, Clone, Copy)]
pub struct Std;

impl Std {
    /// Creates it. Call this at the edge of the program. The same as
    /// writing `Std`, a unit struct.
    pub const fn new() -> Self {
        Self
    }
}

#[cfg(feature = "env")]
pub mod env;
#[cfg(feature = "fs")]
pub mod fs;
#[cfg(feature = "net")]
pub mod net;
#[cfg(feature = "process")]
pub mod process;
#[cfg(feature = "random")]
mod random;
#[cfg(feature = "io-std")]
pub mod stdio;
#[cfg(feature = "sync")]
pub mod sync;
#[cfg(feature = "rt")]
pub mod task;
#[cfg(feature = "time")]
mod time;

/// Converts a std result into seam's: the error keeps the original.
#[allow(dead_code)] // used by whichever modules are on
pub(crate) fn sio<T>(r: std::io::Result<T>) -> seam_core::io::Result<T> {
    r.map_err(Into::into)
}

/// Implements `Read` for a wrapper around a std type that implements
/// `std::io::Read`: `read_ops!([generics] Type)`.
#[cfg(any(feature = "fs", feature = "net", feature = "io-std"))]
macro_rules! read_ops {
    ([$($g:tt)*] $ty:ty) => {
        impl<$($g)*> seam_core::Perform<seam_core::ops::io::Read<'_>> for $ty {
            fn perform(
                &mut self,
                op: seam_core::ops::io::Read<'_>,
            ) -> seam_core::io::Result<usize> {
                crate::sio(std::io::Read::read(&mut self.0, op.0))
            }
        }
    };
}
#[cfg(any(feature = "fs", feature = "net", feature = "io-std"))]
use read_ops;

/// Implements `Write` and `Flush` for a wrapper around a std type that
/// implements `std::io::Write`: `write_ops!([generics] Type)`.
#[cfg(any(feature = "fs", feature = "net", feature = "io-std"))]
macro_rules! write_ops {
    ([$($g:tt)*] $ty:ty) => {
        impl<$($g)*> seam_core::Perform<seam_core::ops::io::Write<'_>> for $ty {
            fn perform(
                &mut self,
                op: seam_core::ops::io::Write<'_>,
            ) -> seam_core::io::Result<usize> {
                crate::sio(std::io::Write::write(&mut self.0, op.0))
            }
        }

        impl<$($g)*> seam_core::Perform<seam_core::ops::io::Flush> for $ty {
            fn perform(&mut self, _: seam_core::ops::io::Flush) -> seam_core::io::Result<()> {
                crate::sio(std::io::Write::flush(&mut self.0))
            }
        }
    };
}
#[cfg(any(feature = "fs", feature = "net", feature = "io-std"))]
use write_ops;

// Every capability type opts in to seam's lifts.
impl seam_core::Capability for Std {}
