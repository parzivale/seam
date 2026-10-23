//! The Tokio backend for seam.
//!
//! [`Tokio`] performs seam operations with Tokio.
//! It implements the async traits in [`seam_core::future`], including the
//! `Send` forms needed on a multithreaded runtime.
//!
//! It covers what Tokio is for: time, the filesystem, the network, stdio,
//! processes, signals, tasks and channels. Operations with nothing to do
//! with Tokio, the environment (`Var`, `Args`, ...) and randomness, are not
//! here: take them from `seam-std`'s `Std` in the same root, as
//! `seam::root! { std: Std::new() => [kinds::Env, kinds::Random], tokio:
//! Tokio::new() => [kinds::Time, kinds::Net] }`. Both are zero-sized, and
//! so is the root.
//!
//! ```no_run
//! # #[cfg(all(feature = "rt", feature = "time", feature = "net"))] {
//! use seam_core::io;
//! use std::time::Duration;
//! use seam_core::prelude::*;
//! use seam_core::ops::io::WriteAll;
//! use seam_core::ops::net::{Resolve, TcpConnect};
//! use seam_core::ops::time::Sleep;
//!
//! /// The core: sends a heartbeat, retrying with backoff. `Send` bounds,
//! /// so it can run on a multithreaded runtime.
//! async fn heartbeat<C, S>(c: &mut C, host: &str) -> io::Result<()>
//! where
//!     C: SendInspect<Resolve> + AsyncProvide<TcpConnect<S>> + SendPerform<Sleep> + Send,
//!     S: for<'a> SendPerform<WriteAll<'a>> + Send,
//! {
//!     let addr = c.inspect_send(Resolve::new(host, 9000)).await?[0];
//!     for attempt in 0..5 {
//!         // The stream exists only inside this scope, and is shut down
//!         // cleanly when the scope succeeds.
//!         let sent = c
//!             .acquire_async(TcpConnect::new(addr), async |_, s| s.perform_send(WriteAll(b"alive\n")).await)
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
//!         let mut tokio = seam_tokio::Tokio::new();
//!         heartbeat::<_, seam_tokio::net::TcpStream>(&mut tokio, "localhost").await
//!     })
//! }
//! # }
//! ```
//!
//! Every operation must run inside a Tokio runtime. With the `rt` feature,
//! `seam_tokio::block_on` starts one and runs a future on it; create
//! `Tokio::new()` (or a root with it as a part) inside, as inside any
//! runtime you already have.
//!
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
//!
//! # Features
//!
//! Names follow Tokio's: `rt`, `rt-multi-thread`, `fs`, `net`, `time`,
//! `process`, `io-std`, `signal`, `sync`, `io-util`, `test-util`, `full`. Each turns
//! on the Tokio feature of the same name and the matching seam operations.
//! With `test-util`, `Now` follows Tokio's pausable clock.

/// The Tokio backend's capability.
///
/// It holds no state. Every operation it supports is a real effect, and must
/// run inside a Tokio runtime. It is `Clone`, so a core can hand a copy to
/// each task it spawns.
#[derive(Debug, Default, Clone)]
#[non_exhaustive]
pub struct Tokio;

impl Tokio {
    /// Creates it. Call this at the edge of the program.
    pub fn new() -> Self {
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
    (perform [$($gen:tt)*] $ty:ty, $op:ty, |$s:ident, $o:ident| $body:expr) => {
        impl<$($gen)*> seam_core::future::AsyncPerform<$op> for $ty {
            fn perform_async(
                &mut self,
                $o: $op,
            ) -> impl core::future::Future<Output = <$op as seam_core::Operation>::Outcome> {
                let $s = self;
                async move { crate::IntoOutcome::into_outcome({ $body }) }
            }
        }

        impl<$($gen)*> seam_core::future::SendPerform<$op> for $ty {
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
    (inspect [$($gen:tt)*] $ty:ty, $op:ty, |$s:ident, $o:ident| $body:expr) => {
        impl<$($gen)*> seam_core::future::AsyncInspect<$op> for $ty {
            fn inspect_async(
                &self,
                $o: $op,
            ) -> impl core::future::Future<Output = <$op as seam_core::Operation>::Outcome> {
                let $s = self;
                async move { crate::IntoOutcome::into_outcome({ $body }) }
            }
        }

        impl<$($gen)*> seam_core::future::SendInspect<$op> for $ty {
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
        });
    };
}

/// Implements the handle-level `io` operations for a wrapper around a Tokio
/// type that implements `AsyncRead` and `AsyncWrite`.
#[cfg(any(feature = "fs", feature = "net"))]
macro_rules! io_ops {
    ($ty:ty) => {
        async_op!(perform ['a] $ty, seam_core::ops::io::Read<'a>, |s, op| {
            tokio::io::AsyncReadExt::read(&mut s.0, op.0).await
        });
        async_op!(perform ['a] $ty, seam_core::ops::io::ReadToEnd<'a>, |s, op| {
            tokio::io::AsyncReadExt::read_to_end(&mut s.0, op.0).await
        });
        async_op!(perform ['a] $ty, seam_core::ops::io::Write<'a>, |s, op| {
            tokio::io::AsyncWriteExt::write(&mut s.0, op.0).await
        });
        async_op!(perform ['a] $ty, seam_core::ops::io::WriteAll<'a>, |s, op| {
            tokio::io::AsyncWriteExt::write_all(&mut s.0, op.0).await
        });
        async_op!(perform [] $ty, seam_core::ops::io::Flush, |s, _op| {
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
mod stdio;
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
