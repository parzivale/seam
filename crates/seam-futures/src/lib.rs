//! The futures-rs backend for seam.
//!
//! futures-rs is the runtime-agnostic half of async Rust: channels, locks,
//! executors, the `AsyncRead`/`AsyncWrite` traits and `Stream`. This crate
//! wraps those pieces the way `seam-tokio` wraps Tokio, so they work under
//! any executor: Tokio, futures-rs's own, or seam's simulated runtime.
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
//! futures-rs has no clock, network or filesystem, so neither does this
//! backend: take those from `Tokio`, `Std` or the simulation in the same
//! root.
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
pub mod io;
#[cfg(feature = "net")]
pub mod net;
#[cfg(feature = "process")]
pub mod process;
pub mod stream;
#[cfg(feature = "sync")]
pub mod sync;
#[cfg(feature = "rt")]
pub mod task;
#[cfg(feature = "time")]
pub mod time;

#[cfg(feature = "rt")]
pub use task::Executor;
