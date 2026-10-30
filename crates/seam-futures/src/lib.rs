//! The futures-rs backend for seam.
//!
//! futures-rs is the runtime-agnostic half of async Rust: channels, locks,
//! executors, the `AsyncRead`/`AsyncWrite` traits and `Stream`. This crate
//! wraps those pieces the way `seam-tokio` wraps Tokio, so they work under
//! any executor: Tokio, smol, futures-rs's own, or seam's simulated
//! runtime.
//!
//! - [`Futures`] (`sync`): channels from `futures::channel::mpsc` and shared
//!   cells behind `futures::lock::Mutex`, as `seam::root![Futures<fx::Channel,
//!   fx::Share>, ..]`. Nothing in them needs a runtime, so they run in the
//!   simulated runtime too, deterministically.
//! - [`Executor`] (`rt`): spawning tasks on any executor that implements
//!   `futures::task::Spawn` (a `ThreadPool`, a `LocalPool`'s spawner, ...),
//!   as `Executor<fx::Spawn, fx::SpawnSet>(pool)`.
//! - [`io`] (`io`): `Compat` makes any futures-io reader or writer a seam
//!   handle, and `AsyncReader`/`AsyncWriter` make a seam handle a futures-io
//!   one, for codecs and other ecosystem crates.
//! - [`stream`]: a receiver's messages, a handle's lines and a directory's
//!   entries as `Stream`s.
//! - Helpers that need concurrency inside one task, which only async code
//!   has: [`time::within`], [`net::copy_bidirectional_async`] and
//!   [`process::pipe_through_async`], each with a `_send` form.
//!
//! futures' own API is here too, so a core needs no direct dependency on
//! futures: its traits and combinators (`StreamExt`, `FutureExt`,
//! `AsyncReadExt`, ...), `select!`, `join!` and friends, and its `future`,
//! `stream`, `sink`, `io`, `task`, `channel` and `lock` modules (`io`,
//! `stream` and `task` together with this crate's own). It is built
//! without futures' `executor`, whose `ThreadPool` spawns threads: tasks
//! go through [`Executor`] and `Spawn`.
//!
//! futures-rs has no clock, network or filesystem, so neither does this
//! backend: take those from `Tokio`, `Smol`, `Std` or the simulation in
//! the same root.
//!
//! ```
//! # #[cfg(feature = "sync")] {
//! use seam_core::effects as fx;
//! use seam_core::prelude::*;
//! use seam_core::ops::sync::{Channel, RecvMsg, SendMsg};
//! use seam_futures::Futures;
//! use seam_futures::sync::{Receiver, Sender};
//!
//! async fn relay<C>(c: &mut C) -> u32
//! where
//!     C: Perform<Channel<Sender<u32>, Receiver<u32>>>,
//! {
//!     let (mut tx, mut rx) = c.perform(Channel::unbounded());
//!     tx.perform_async(SendMsg(7)).await.unwrap();
//!     rx.perform_async(RecvMsg::new()).await.unwrap()
//! }
//!
//! let mut root = seam_core::root![Futures<fx::Channel>];
//! assert_eq!(futures::executor::block_on(relay(&mut root)), 7);
//! # }
//! ```
//! # In a program
//!
//! Futures' parts sit beside a backend that has the rest. Here, smol gives
//! the clock, and [`time::within`] races a channel receive against it:
//!
//! ```no_run
//! # #[cfg(all(feature = "sync", feature = "time"))] {
//! use seam_core::effects as fx;
//! use seam_core::ops::sync::{Channel, RecvMsg, SendMsg};
//! use seam_core::prelude::*;
//! use seam_futures::Futures;
//! use seam_futures::sync::{Receiver, Sender};
//! use std::time::Duration;
//!
//! seam_smol::block_on(async {
//!     let mut root = seam_core::root![Futures<fx::Channel>, seam_smol::Smol<fx::Sleep>];
//!     let (mut tx, mut rx): (Sender<u32>, Receiver<u32>) = root.perform(Channel::unbounded());
//!     tx.perform_async(SendMsg(1)).await.unwrap();
//!     let got = seam_futures::time::within(&mut root, Duration::from_secs(1), rx.perform_async(RecvMsg::new())).await;
//!     println!("{got:?}");
//! });
//! # }
//! ```
//!
//! [`Executor`] spawns on any futures-rs executor:
//! `seam_core::root![Executor<fx::Spawn, fx::SpawnSet>(pool), ..]` with a
//! `futures::executor::ThreadPool`, say.
//!
//! Through the `seam` crate, this crate is `seam::futures` (feature
//! `futures`), with the capability groups enabled there.
//!
//! # Features
//!
//! | Feature | Gives |
//! |---|---|
//! | `sync` | [`Futures`]: channels and shared cells |
//! | `rt` | [`Executor`]: tasks on any futures-rs executor |
//! | `io` | [`io`]: futures-io adapters, both ways |
//! | `time` | [`time::within`] |
//! | `net` | [`net::copy_bidirectional_async`] |
//! | `process` | [`process::pipe_through_async`] |
//! | `fs` | directory entries as a `Stream` |
//! | `full` | all of the above |
//!
//! None is on by default.
//!
#![cfg_attr(
    not(feature = "rt"),
    doc = "[`Executor`]: https://docs.rs/seam-rs-futures/latest/seam_futures/struct.Executor.html"
)]
#![cfg_attr(
    not(feature = "io"),
    doc = "[`io`]: https://docs.rs/seam-rs-futures/latest/seam_futures/io/index.html"
)]
#![cfg_attr(
    not(feature = "time"),
    doc = "[`time::within`]: https://docs.rs/seam-rs-futures/latest/seam_futures/time/fn.within.html"
)]
#![cfg_attr(
    not(feature = "net"),
    doc = "[`net::copy_bidirectional_async`]: https://docs.rs/seam-rs-futures/latest/seam_futures/net/fn.copy_bidirectional_async.html"
)]
#![cfg_attr(
    not(feature = "process"),
    doc = "[`process::pipe_through_async`]: https://docs.rs/seam-rs-futures/latest/seam_futures/process/fn.pipe_through_async.html"
)]
#![cfg_attr(
    not(feature = "sync"),
    doc = "[`Futures`]: https://docs.rs/seam-rs-futures/latest/seam_futures/struct.Futures.html"
)]
#![forbid(unsafe_code)]

/// futures-rs's runtime-agnostic pieces, as a capability: channels and
/// shared cells. It holds no state and needs no runtime.
#[derive(Debug, Default, Clone, Copy)]
pub struct Futures;

impl Futures {
    /// Creates it. The same as writing `Futures`, a unit struct.
    pub const fn new() -> Self {
        Self
    }
}

// Every capability type opts in to seam's lifts.
impl seam_core::Capability for Futures {}

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

/// Defines an async helper twice: for async capabilities, and for async
/// ones whose futures are `Send`. Like seam-core's `three_ways!`, without
/// the sync form, which these helpers cannot have: they run several
/// operations at once.
///
/// In the body, `run!(handle, op)` performs and `acquire!(c, a, |c, r|
/// body)` acquires in the form being generated, and `call!(async, send;
/// (args))` calls another helper in it.
#[allow(unused_macros)]
macro_rules! two_ways {
    ($(
        $(#[$m:meta])*
        pub async fn $aname:ident, $sname:ident
        [$($g:tt)*] ($($args:tt)*) -> $ret:ty
        where
            async { $($ab:tt)* }
            send { $($nb:tt)* }
        $body:block
    )*) => {$(
        $(#[$m])*
        #[allow(clippy::needless_lifetimes)]
        pub async fn $aname<$($g)*>($($args)*) -> $ret where $($ab)* {
            #[allow(unused_macros)]
            macro_rules! run {
                ($c:expr, $op:expr) => { seam_core::future::AsyncPerform::perform_async($c, $op).await };
            }
            #[allow(unused_macros)]
            macro_rules! acquire {
                ($c:expr, $a:expr, |$cc:pat_param, $r:pat_param| $b:expr) => {
                    seam_core::AsyncProvide::acquire_async($c, $a, async |$cc, $r| $b).await
                };
            }
            #[allow(unused_macros)]
            macro_rules! call {
                ($as:path, $ss:path; $xs:tt) => { $as $xs };
            }
            $body
        }

        #[doc = concat!(
            "`", stringify!($aname), "`, with a `Send` future: for multithreaded runtimes."
        )]
        #[allow(clippy::needless_lifetimes)]
        pub async fn $sname<$($g)*>($($args)*) -> $ret where $($nb)* {
            #[allow(unused_macros)]
            macro_rules! run {
                ($c:expr, $op:expr) => { seam_core::future::SendPerform::perform_send($c, $op).await };
            }
            #[allow(unused_macros)]
            macro_rules! acquire {
                ($c:expr, $a:expr, |$cc:pat_param, $r:pat_param| $b:expr) => {
                    seam_core::SendScope::acquire_send($c, $a, async |$cc, $r| $b).await
                };
            }
            #[allow(unused_macros)]
            macro_rules! call {
                ($as:path, $ss:path; $xs:tt) => { $ss $xs };
            }
            $body
        }
    )*};
}

#[cfg(feature = "io")]
mod io_adapters;
#[cfg(feature = "net")]
pub mod net;
#[cfg(feature = "process")]
pub mod process;
pub mod stream;
#[cfg(feature = "sync")]
pub mod sync;
#[cfg(feature = "rt")]
mod tasks;
#[cfg(feature = "time")]
pub mod time;

/// futures' `io`: its traits, combinators and adapters; with the `io`
/// feature, this crate's adapters between them and seam's handles too.
pub mod io {
    #![allow(rustdoc::redundant_explicit_links)]
    #![cfg_attr(
        feature = "io",
        doc = "\n\nfutures-io, both ways.\n\n- [`Compat`](crate::io::Compat) makes a futures-io reader or writer a seam handle: it\n  performs `Read`, `Write`, `Flush`, `Seek` and (with `net`) `Shutdown`,\n  so seam's `io` helpers and any core bound on those operations work on\n  it. A test can hand a core a `futures::io::Cursor` this way.\n- [`AsyncReader`](crate::io::AsyncReader) and [`AsyncWriter`](crate::io::AsyncWriter) make a seam handle a futures-io\n  `AsyncRead` or `AsyncWrite`, for codecs, compression and other crates\n  written against futures-io. Each borrows the handle for as long as it\n  lives, inside the handle's scope.\n\n```\nuse futures::io::{AsyncReadExt, Cursor};\nuse seam_core::ops::io::read_to_end_async;\nuse seam_futures::io::{AsyncReader, Compat};\n\nfutures::executor::block_on(async {\n    // futures-io into seam...\n    let mut handle = Compat(Cursor::new(b\"round trip\".to_vec()));\n    // ...and back out.\n    let mut text = String::new();\n    AsyncReader::new(&mut handle).read_to_string(&mut text).await.unwrap();\n    assert_eq!(text, \"round trip\");\n});\n```"
    )]
    pub use futures::io::*;

    #[cfg(feature = "io")]
    pub use crate::io_adapters::*;
}

/// futures' `task`: wakers, contexts and the spawning traits; with the
/// `rt` feature, this crate's `Executor` too.
pub mod task {
    #![allow(rustdoc::redundant_explicit_links)]
    #![cfg_attr(
        feature = "rt",
        doc = "\n\nSpawning tasks on a futures-rs executor, and the [`JoinHandle`](crate::task::JoinHandle) and\n[`TaskSet`](crate::task::TaskSet) handles.\n\n[`Executor`](crate::task::Executor) wraps anything that implements `futures::task::Spawn`: a\n`futures::executor::ThreadPool`, a `LocalPool`'s spawner, or another\nruntime's adapter. Tasks are `Send + 'static` futures, as with\n`futures::task::SpawnExt::spawn`."
    )]
    pub use futures::task::*;

    #[cfg(feature = "rt")]
    pub use crate::tasks::*;
}

#[cfg(feature = "rt")]
pub use tasks::Executor;

// futures' API, all of it pure: built without `executor` (whose
// `ThreadPool` spawns threads; take `Spawn` through `Executor` instead),
// futures has nothing here that reaches the system. `io`, `stream` and
// `task` merge futures' modules with this crate's.
pub use futures::{
    AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncSeek, AsyncSeekExt, AsyncWrite,
    AsyncWriteExt, Future, FutureExt, Sink, SinkExt, Stream, StreamExt, TryFuture, TryFutureExt,
    TryStream, TryStreamExt, channel, future, join, lock, never, pending, pin_mut, poll, prelude,
    ready, select, select_biased, sink, stream_select, try_join,
};
