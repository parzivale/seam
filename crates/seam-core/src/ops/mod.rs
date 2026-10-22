//! Operation types for common effects, one module per capability feature.
//!
//! Backends (`seam-std`, `seam-tokio`, `seam-test`) implement
//! [`Inspect`](crate::Inspect) and [`Perform`](crate::Perform) for these
//! types, so a core written against them runs on any backend.
//!
//! | Module | Feature | Needs std | Examples |
//! |---|---|---|---|
//! | `time` | `time` | no (`WallClock`: yes) | `Now`, `Sleep`, `WallClock` |
//! | `random` | `random` | no | `RandomU64`, `FillRandom` |
//! | `fs` | `fs` | yes | `ReadFile`, `WriteFile`, `Open<F>` |
//! | `io` | `fs` or `net` | yes | `Read`, `WriteAll`, `Flush` on handles |
//! | `net` | `net` | yes | `TcpConnect<S>`, `TcpBind<L>`, `Resolve` |
//! | `env` | `env` | yes | `Var`, `Args`, `CurrentDir` |
//! | `process` | `process` | yes | `RunCommand`, `Exit` |
//! | `stdio` | `io-std` | yes | `Print`, `ReadLine` |
//! | `signal` | `signal` | yes | `CtrlC` |
//! | `task` | `rt` | no (`BoxFuture`: alloc) | `Spawn`, `Join`, `Abort`, `YieldNow` |
//!
//! # Handles
//!
//! Operations that create a resource (opening a file, connecting a socket)
//! are generic over the handle type, since each backend has its own. The
//! core names the handle as a type parameter with the bounds it needs, and
//! the operations on the handle are in `io` or `net`:
//!
//! ```
//! # #[cfg(feature = "fs")] {
//! use std::io;
//! use seam_core::Perform;
//! use seam_core::ops::fs::Open;
//! use seam_core::ops::io::WriteAll;
//!
//! fn save<C, F>(c: &mut C, report: &str) -> io::Result<()>
//! where
//!     C: Perform<Open<F>>,
//!     F: for<'a> Perform<WriteAll<'a>>,
//! {
//!     let mut file = c.perform(Open::create("report.txt"))?;
//!     file.perform(WriteAll(report.as_bytes()))
//! }
//! # }
//! ```
//!
//! The caller picks `F` by passing a backend: `save(&mut Std::new(), ..)`
//! infers `seam_std::fs::File`.

/// Implements `Operation` (and optionally `Query`) for a list of types.
#[allow(unused_macros)]
macro_rules! operations {
    ($($(#[$query:ident])? $ty:ty => $outcome:ty;)*) => {$(
        impl $crate::Operation for $ty {
            type Outcome = $outcome;
        }
        $(operations!(@$query $ty);)?
    )*};
    (@query $ty:ty) => {
        impl $crate::Query for $ty {}
    };
}

#[cfg(feature = "env")]
pub mod env;
#[cfg(feature = "fs")]
pub mod fs;
#[cfg(feature = "io")]
pub mod io;
#[cfg(feature = "net")]
pub mod net;
#[cfg(feature = "process")]
pub mod process;
#[cfg(feature = "random")]
pub mod random;
#[cfg(feature = "signal")]
pub mod signal;
#[cfg(feature = "io-std")]
pub mod stdio;
#[cfg(feature = "rt")]
pub mod task;
#[cfg(feature = "time")]
pub mod time;
