//! The Tokio backend for seam.
//!
//! [`Tokio`] is the root capability: it performs seam operations with Tokio
//! and implements the async traits in [`seam_core::future`], including the
//! `Send` variants needed on a multithreaded runtime. Operations Tokio has
//! no async version of (clocks, environment, randomness) are implemented
//! synchronously and lift to async through the blanket impls.
//!
//! As in `seam-std`, handles are newtypes that expose no Tokio I/O traits.
//!
//! Feature names follow Tokio's (`rt`, `rt-multi-thread`, `fs`, `net`,
//! `time`, `process`, `io-std`, `signal`, `test-util`, `full`), plus `env`
//! and `random`. Operations must run inside a Tokio runtime; with `rt`,
//! [`block_on`] starts one.

/// The root capability over Tokio.
///
/// It holds no state. Every operation it supports is a real effect, and must
/// run inside a Tokio runtime.
#[derive(Debug, Default)]
#[non_exhaustive]
pub struct Tokio;

impl Tokio {
    /// Creates the root capability. Call this at the edge of the program.
    pub fn new() -> Self {
        Self
    }
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
                async move { $body }
            }
        }

        impl<$($gen)*> seam_core::future::SendPerform<$op> for $ty {
            fn perform_send(
                &mut self,
                $o: $op,
            ) -> impl core::future::Future<Output = <$op as seam_core::Operation>::Outcome> + Send
            {
                let $s = self;
                async move { $body }
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
                async move { $body }
            }
        }

        impl<$($gen)*> seam_core::future::SendInspect<$op> for $ty {
            fn inspect_send(
                &self,
                $o: $op,
            ) -> impl core::future::Future<Output = <$op as seam_core::Operation>::Outcome> + Send
            {
                let $s = self;
                async move { $body }
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
#[cfg(feature = "rt")]
mod runtime;
#[cfg(feature = "signal")]
mod signal;
#[cfg(feature = "io-std")]
mod stdio;
#[cfg(feature = "time")]
mod time;

#[cfg(feature = "rt")]
pub use runtime::block_on;
#[cfg(feature = "rt-multi-thread")]
pub use runtime::block_on_multi_thread;
