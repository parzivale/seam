//! The smol handler for seam.
//!
//! [`Smol`] performs seam operations with smol: time, the filesystem, the
//! network, stdio, processes, Ctrl-C (Unix), tasks and channels. It
//! implements the async traits in [`seam_core::future`], including the
//! `Send` forms for multithreaded executors. The environment and
//! randomness have nothing to do with smol: take them from `seam-std`'s
//! `Std` in the same root.
//!
//! ```no_run
//! # #[cfg(all(feature = "time", feature = "net"))] {
//! use std::time::Duration;
//! use seam_core::io;
//! use seam_core::prelude::*;
//! use seam_core::ops::io::{Write, write_all_send};
//! use seam_core::ops::net::TcpConnect;
//! use seam_core::ops::time::Sleep;
//!
//! async fn ping<C, S>(c: &mut C) -> io::Result<()>
//! where
//!     C: AsyncProvide<TcpConnect<S>> + SendPerform<Sleep> + Send,
//!     S: for<'a> SendPerform<Write<'a>> + Send,
//! {
//!     c.perform_send(Sleep(Duration::from_millis(10))).await;
//!     c.acquire_async(TcpConnect::new(([127, 0, 0, 1], 9000)), async |_, s| {
//!         write_all_send(s, b"ping\n").await
//!     })
//!     .await
//! }
//!
//! seam_smol::block_on(ping::<_, seam_smol::net::TcpStream>(&mut seam_smol::Smol)).unwrap();
//! # }
//! ```
//!
//! # In a root
//!
//! smol has no environment or randomness, so a program on smol takes those
//! from `Std` in the same root, and smol serves what waits:
//!
//! ```no_run
//! # #[cfg(feature = "time")] {
//! use std::time::Duration;
//!
//! use seam_core::effects as fx;
//! use seam_core::ops::random::{FillRandom, random_u64_send};
//! use seam_core::ops::time::Sleep;
//! use seam_core::prelude::*;
//! use seam_smol::Smol;
//! use seam_std::Std;
//!
//! /// The core: waits a random 100 to 199 ms, as a retry backs off.
//! async fn back_off<C>(c: &mut C) -> u64
//! where
//!     C: for<'a> SendPerform<FillRandom<'a>> + SendPerform<Sleep> + Send,
//! {
//!     let ms = 100 + random_u64_send(c).await % 100;
//!     c.perform_send(Sleep(Duration::from_millis(ms))).await;
//!     ms
//! }
//!
//! let mut root = seam_core::root![Std<fx::FillRandom>, Smol<fx::Sleep>];
//! println!("waited {} ms", seam_smol::block_on(back_off(&mut root)));
//! # }
//! ```
//!
//! # With the other crates
//!
//! A program on smol routes what smol does not handle to other handlers
//! in the same root, the wrapper crates' among them:
//!
//! | For | Use |
//! |---|---|
//! | the environment, arguments, randomness | `seam-std`'s `Std` |
//! | channels, locks and tasks on any executor, futures-io adapters, `within` | `seam-futures` |
//! | time zones, UUIDs, platform directories | `seam-jiff`, `seam-uuid`, `seam-dirs`, over `Std` |
//! | logging | `seam-tracing`'s `Tracing` |
//! | file watching | `seam-notify`'s `Notify`, which waits without blocking under any executor |
//! | HTTP | `seam-reqwest`'s `Reqwest`, which runs its requests on a Tokio runtime of its own (reqwest needs one) |
//! | SQL | `seam-sqlx`'s `Sqlx`, with its `smol` feature |
//! | Unix calls | `seam-rustix`'s `Rustix` or `seam-nix`'s `Nix` |
//! | the GPU | `seam-ash`'s `Ash`, which waits without blocking under any executor |
//! # Runtime
//!
//! smol needs no runtime object: [`block_on`] runs a future on the current
//! thread, and spawned tasks run on smol's global executor, whose threads
//! start on first use (`SMOL_THREADS` sets how many). Blocking work
//! (files, stdin) runs on smol's blocking pool.
//!
//! # Handles
//!
//! As in the other handlers, files and sockets are newtypes that expose no
//! smol I/O traits. When a scope succeeds a file is flushed and synced and
//! a stream's write half is shut down; when it fails, panics or is
//! cancelled, they are dropped. `Split` lends a stream as two clones of it
//! (smol's streams are shared handles), so a failed split loses nothing.
//! # Features
//!
//! | Feature | Gives |
//! |---|---|
//! | `time` | `Now`, `WallClock`, `Sleep`; and `Timeout` on the waiting operations |
//! | `fs` | files and directories, on smol's blocking pool |
//! | `net` | TCP, UDP, name resolution |
//! | `process` | child processes |
//! | `io-std` | stdin, stdout, stderr |
//! | `signal` | Ctrl-C, on Unix |
//! | `sync` | channels and shared cells |
//! | `rt` | `Spawn`, `SpawnSet` and friends, on smol's global executor |
//! | `full` | all of the above |
//!
//! None is on by default.
#![forbid(unsafe_code)]

use core::future::Future;
#[cfg(feature = "time")]
use core::time::Duration;

/// The smol handler's capability. It holds no state; it is `Copy`, so a
/// core can hand one to each task it spawns.
#[derive(Debug, Default, Clone, Copy)]
pub struct Smol;

impl Smol {
    /// Creates it. The same as writing `Smol`, a unit struct.
    pub const fn new() -> Self {
        Self
    }
}

/// Runs `future` to completion on the current thread, driving smol's
/// reactor. Build the root inside or outside it; `Smol` needs no runtime
/// object.
pub fn block_on<F: Future>(future: F) -> F::Output {
    smol::block_on(future)
}

// Every capability type opts in to seam's lifts.
impl seam_core::Capability for Smol {}

/// What an operation's body may produce: its outcome, or for an operation
/// returning seam's `io::Result`, std's, which converts.
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

/// Runs `fut` until it finishes or `after` passes. `None` if the time ran
/// out first. The future is polled before the timer, so a timeout of zero
/// is a non-blocking attempt.
#[cfg(feature = "time")]
#[allow(dead_code)] // used by whichever modules are on
pub(crate) async fn within<T>(after: Duration, fut: impl Future<Output = T>) -> Option<T> {
    smol::future::or(async { Some(fut.await) }, async {
        smol::Timer::after(after).await;
        None
    })
    .await
}

#[cfg(feature = "time")]
#[allow(dead_code)] // used by whichever modules are on
pub(crate) fn timed_out() -> std::io::Error {
    std::io::ErrorKind::TimedOut.into()
}

/// Implements the async capability traits for an operation, from one async
/// body: `AsyncPerform` and `SendPerform` for `perform`, and for `inspect`
/// (queries) also `AsyncInspect` and `SendInspect`.
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

/// Implements `Read` for a wrapper around a smol type that implements
/// `AsyncRead`: `read_ops!([generics] Type)`.
#[cfg(any(feature = "fs", feature = "net"))]
macro_rules! read_ops {
    ([$($g:tt)*] $ty:ty) => {
        async_op!(perform ['a, $($g)*] $ty, seam_core::ops::io::Read<'a>, |s, op| {
            smol::io::AsyncReadExt::read(&mut s.0, op.0).await
        });
    };
}

/// Implements `Write` and `Flush` for a wrapper around a smol type that
/// implements `AsyncWrite`: `write_ops!([generics] Type)`.
#[cfg(any(feature = "fs", feature = "net", feature = "io-std"))]
macro_rules! write_ops {
    ([$($g:tt)*] $ty:ty) => {
        async_op!(perform ['a, $($g)*] $ty, seam_core::ops::io::Write<'a>, |s, op| {
            smol::io::AsyncWriteExt::write(&mut s.0, op.0).await
        });
        async_op!(perform [$($g)*] $ty, seam_core::ops::io::Flush, |s, _op| {
            smol::io::AsyncWriteExt::flush(&mut s.0).await
        });
    };
}

#[cfg(feature = "fs")]
pub mod fs;
#[cfg(feature = "net")]
pub mod net;
#[cfg(feature = "process")]
pub mod process;
#[cfg(all(feature = "signal", unix))]
mod signal;
#[cfg(feature = "io-std")]
pub mod stdio;
#[cfg(feature = "sync")]
pub mod sync;
#[cfg(feature = "rt")]
pub mod task;
#[cfg(feature = "time")]
mod time;
