//! The Tokio handler for seam.
//!
//! [`Tokio`] performs seam operations with Tokio.
//! It implements the async traits in [`seam_core::future`], including the
//! `Send` forms needed on a multithreaded runtime.
//!
//! It covers what Tokio is for: time, the filesystem, the network, stdio,
//! processes, signals, tasks and channels. Operations with nothing to do
//! with Tokio, the environment (`Var`, `Args`, ...) and randomness, are not
//! here: take them from `seam-std`'s `Std` in the same root, as
//! `seam::root![Std<fx::Var, fx::FillRandom>, Tokio<fx::Now, fx::Sleep,
//! fx::TcpBind>]`. Both are zero-sized, and
//! so is the root.
//!
//! ```no_run
//! # #[cfg(all(feature = "rt", feature = "time", feature = "net"))] {
//! use seam_core::io;
//! use std::net::SocketAddr;
//! use std::time::Duration;
//! use seam_core::prelude::*;
//! use seam_core::ops::io::{Write, write_all_send};
//! use seam_core::ops::net::{ResolveInto, TcpConnect};
//! use seam_core::ops::time::Sleep;
//!
//! /// The core: sends a heartbeat, retrying with backoff. `Send` bounds,
//! /// so it can run on a multithreaded runtime.
//! async fn heartbeat<C, S>(c: &mut C, host: &str) -> io::Result<()>
//! where
//!     C: for<'a> SendInspect<ResolveInto<'a>> + AsyncProvide<TcpConnect<S>> + SendPerform<Sleep> + Send,
//!     S: for<'a> SendPerform<Write<'a>> + Send,
//! {
//!     let mut addr = [SocketAddr::from(([0, 0, 0, 0], 0))];
//!     if c.inspect_send(ResolveInto::new(host, 9000, &mut addr)).await? == 0 {
//!         return Err(io::Error::new(io::ErrorKind::NotFound, "no addresses"));
//!     }
//!     let [addr] = addr;
//!     for attempt in 0..5 {
//!         // The stream exists only inside this scope, and is shut down
//!         // cleanly when the scope succeeds.
//!         let sent = c
//!             .acquire_async(TcpConnect::new(addr), async |_, s| write_all_send(s, b"alive\n").await)
//!             .await;
//!         match sent {
//!             Ok(()) => return Ok(()),
//!             Err(_) => c.perform_send(Sleep(Duration::from_millis(100 << attempt))).await,
//!         }
//!     }
//!     Err(io::ErrorKind::TimedOut.into())
//! }
//!
//! fn main() -> seam_core::io::Result<()> {
//!     seam_tokio::block_on(async {
//!         let mut tokio = seam_tokio::Tokio;
//!         heartbeat::<_, seam_tokio::net::TcpStream>(&mut tokio, "localhost").await
//!     })
//! }
//! # }
//! ```
//!
//! Every operation must run inside a Tokio runtime. With the `rt` feature,
//! `seam_tokio::block_on` starts one and runs a future on it; create
//! `Tokio` (or a root with it as a part) inside, as inside any
//! runtime you already have.
//!
//! # In a root
//!
//! Tokio has no environment or randomness, so a program on Tokio takes
//! those from `Std` in the same root, and Tokio serves what waits:
//!
//! ```no_run
//! # #[cfg(all(feature = "rt", feature = "time"))] {
//! use std::time::Duration;
//!
//! use seam_core::effects as fx;
//! use seam_core::ops::env::Var;
//! use seam_core::ops::time::Sleep;
//! use seam_core::prelude::*;
//! use seam_std::Std;
//! use seam_tokio::Tokio;
//!
//! /// The core: waits as long as `DELAY_MS` says, then greets.
//! async fn greet<C>(c: &mut C) -> String
//! where
//!     C: for<'a> SendInspect<Var<'a>> + SendPerform<Sleep> + Send + Sync,
//! {
//!     let ms = c.inspect_send(Var::new("DELAY_MS")).await;
//!     let ms = ms.and_then(|v| v.to_str()?.parse().ok()).unwrap_or(10);
//!     c.perform_send(Sleep(Duration::from_millis(ms))).await;
//!     "hello".to_owned()
//! }
//!
//! seam_tokio::block_on(async {
//!     let mut root = seam_core::root![Std<fx::Var>, Tokio<fx::Sleep>];
//!     println!("{}", greet(&mut root).await);
//! });
//! # }
//! ```
//!
//! # With the other crates
//!
//! A program on Tokio routes what Tokio does not handle to other handlers
//! in the same root, the wrapper crates' among them:
//!
//! | For | Use |
//! |---|---|
//! | the environment, arguments, randomness | `seam-std`'s `Std` |
//! | channels, locks and tasks on any executor, futures-io adapters, `within` | `seam-futures` |
//! | time zones, UUIDs, platform directories | `seam-jiff`, `seam-uuid`, `seam-dirs`, over `Std` |
//! | logging | `seam-tracing`'s `Tracing` |
//! | file watching | `seam-notify`'s `Notify`, which waits without blocking under any executor |
//! | HTTP | `seam-reqwest`'s `Reqwest` |
//! | SQL | `seam-sqlx`'s `Sqlx` |
//! | Unix calls | `seam-rustix`'s `Rustix` or `seam-nix`'s `Nix` |
//! | the GPU | `seam-ash`'s `Ash`, which waits without blocking under any executor |
//! # Tasks
//!
//! With `rt`, `Tokio` provides `Spawn` (a `task::JoinHandle`) and
//! `SpawnSet` (a `task::TaskSet`, backed by Tokio's `JoinSet`), and a
//! `TcpListener` provides `SpawnWith<Accept<_>, B, _>` for any `TaskBody`
//! `B` (see `seam_core::ops::task`). On a handle the core performs `Join`,
//! `Abort`, `Detach` and `IsFinished`; on a set, `SpawnInto`, `JoinNext` and
//! `Abort`. A successful scope waits for its tasks; a failed or cancelled
//! one aborts them, unless a handle was detached.
//! Spawned futures must be `Send + 'static`, as with `tokio::spawn`.
//!
//! # Handles
//!
//! As in `seam-std`, files and sockets are newtypes that expose no Tokio I/O
//! traits, and a core only ever borrows one inside an `acquire_async` scope.
//! When the scope succeeds, a file is flushed and synced, a stream is shut
//! down, and `fs::AtomicFile` is renamed into place. When it fails, panics
//! or is cancelled, they are dropped instead. `fs::AtomicFile` is safe to
//! cancel at any point, including while it is being created or renamed.
//! Inside the scope a file can also `Seek`, `SetLen`, `SyncAll`, `SyncData`
//! and `Stat`. Tokio streams can only shut down their write half; `Shutdown`
//! of the read half fails with `Unsupported`.
//!
//! `Split` lends a stream as Tokio's owned halves (`net::TcpReadHalf`,
//! `net::TcpWriteHalf`), reunited when the scope succeeds; a failed split
//! closes the stream. `SplitChild` lends a child's pipes as
//! `process::ChildStdin`, `ChildStdout` and `ChildStderr`. Join futures on
//! the pieces to use them at once.
//!
//! # Features
//!
//! Names follow Tokio's: `rt`, `rt-multi-thread`, `fs`, `net`, `time`,
//! `process`, `io-std`, `signal`, `sync`, `io-util`, `test-util`, `full`.
//! Each turns on the Tokio feature of the same name and the matching seam
//! operations.
//! With `test-util`, `Now` follows Tokio's pausable clock.
#![forbid(unsafe_code)]

/// The Tokio handler's capability.
///
/// It holds no state. Every operation it supports is a real effect, and must
/// run inside a Tokio runtime. It is `Clone`, so a core can hand a copy to
/// each task it spawns.
#[derive(Debug, Default, Clone, Copy)]
pub struct Tokio;

impl Tokio {
    /// Creates it. Call this at the edge of the program. The same as
    /// writing `Tokio`, a unit struct.
    pub const fn new() -> Self {
        Self
    }
}

/// What an operation's body may produce: its outcome, or for an operation
/// returning seam's `io::Result`, std's, which converts. The two impls
/// cannot overlap: a `Result` with std's error is never one with seam's.
#[allow(dead_code)] // used by whichever modules are on
pub(crate) trait IntoOutcome<T> {
    fn into_outcome(self) -> T;
}

impl<T> IntoOutcome<T> for T {
    fn into_outcome(self) -> T {
        self
    }
}

impl<T> IntoOutcome<seam_core::io::Result<T>> for std::io::Result<T> {
    fn into_outcome(self) -> seam_core::io::Result<T> {
        self.map_err(Into::into)
    }
}

/// Converts a std result into seam's: the error keeps the original.
#[allow(dead_code)] // used by whichever modules are on
pub(crate) fn sio<T>(r: std::io::Result<T>) -> seam_core::io::Result<T> {
    r.map_err(Into::into)
}

/// Implements the async capability traits for an operation, from one async
/// body. `perform` ops get `AsyncPerform` and `SendPerform`. `inspect` ops
/// (queries) also get `AsyncInspect` and `SendInspect`, with the perform
/// side delegating to them.
#[allow(unused_macros)]
macro_rules! async_op {
    (perform [$($gen:tt)*] $ty:ty, $op:ty, |$s:ident, $o:pat_param| $body:expr $(, where $($w:tt)*)?) => {
        impl<$($gen)*> seam_core::future::AsyncPerform<$op> for $ty $(where $($w)*)? {
            fn perform_async(
                &mut self,
                $o: $op,
            ) -> impl core::future::Future<Output = <$op as seam_core::Operation>::Outcome> {
                let $s = self;
                async move { crate::IntoOutcome::into_outcome({ $body }) }
            }
        }

        impl<$($gen)*> seam_core::future::SendPerform<$op> for $ty $(where $($w)*)? {
            fn perform_send(
                &mut self,
                $o: $op,
            ) -> impl core::future::Future<Output = <$op as seam_core::Operation>::Outcome> + Send
            {
                let $s = self;
                async move { crate::IntoOutcome::into_outcome({ $body }) }
            }
        }
    };
    (inspect [$($gen:tt)*] $ty:ty, $op:ty, |$s:ident, $o:pat_param| $body:expr $(, where $($w:tt)*)?) => {
        impl<$($gen)*> seam_core::future::AsyncInspect<$op> for $ty $(where $($w)*)? {
            fn inspect_async(
                &self,
                $o: $op,
            ) -> impl core::future::Future<Output = <$op as seam_core::Operation>::Outcome> {
                let $s = self;
                async move { crate::IntoOutcome::into_outcome({ $body }) }
            }
        }

        impl<$($gen)*> seam_core::future::SendInspect<$op> for $ty $(where $($w)*)? {
            fn inspect_send(
                &self,
                $o: $op,
            ) -> impl core::future::Future<Output = <$op as seam_core::Operation>::Outcome> + Send
            {
                let $s = self;
                async move { crate::IntoOutcome::into_outcome({ $body }) }
            }
        }

        async_op!(perform [$($gen)*] $ty, $op, |s, o| {
            seam_core::future::SendInspect::inspect_send(&*s, o).await
        } $(, where $($w)*)?);
    };
}

/// Implements `Read` for a wrapper around a Tokio type that implements
/// `AsyncRead`: `read_ops!([generics] Type)`.
#[cfg(any(feature = "fs", feature = "net", feature = "io-std"))]
macro_rules! read_ops {
    ([$($g:tt)*] $ty:ty) => {
        async_op!(perform ['a, $($g)*] $ty, seam_core::ops::io::Read<'a>, |s, op| {
            tokio::io::AsyncReadExt::read(&mut s.0, op.0).await
        });
    };
}

/// Implements `Write` and `Flush` for a wrapper around a Tokio type that
/// implements `AsyncWrite`: `write_ops!([generics] Type)`.
#[cfg(any(feature = "fs", feature = "net", feature = "io-std"))]
macro_rules! write_ops {
    ([$($g:tt)*] $ty:ty) => {
        async_op!(perform ['a, $($g)*] $ty, seam_core::ops::io::Write<'a>, |s, op| {
            tokio::io::AsyncWriteExt::write(&mut s.0, op.0).await
        });
        async_op!(perform [$($g)*] $ty, seam_core::ops::io::Flush, |s, _op| {
            tokio::io::AsyncWriteExt::flush(&mut s.0).await
        });
    };
}

#[cfg(feature = "fs")]
pub mod fs;
#[cfg(feature = "net")]
pub mod net;
#[cfg(feature = "process")]
pub mod process;
#[cfg(feature = "rt")]
mod runtime;
#[cfg(feature = "signal")]
mod signal;
#[cfg(feature = "io-std")]
pub mod stdio;
#[cfg(feature = "sync")]
pub mod sync;
#[cfg(feature = "rt")]
pub mod task;
#[cfg(feature = "time")]
mod time;
#[cfg(feature = "time")]
mod timeouts;

#[cfg(feature = "rt")]
pub use runtime::block_on;
#[cfg(feature = "rt-multi-thread")]
pub use runtime::block_on_multi_thread;

// Every capability type opts in to seam's lifts.
impl seam_core::Capability for Tokio {}
