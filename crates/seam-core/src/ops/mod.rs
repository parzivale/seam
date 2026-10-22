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
//! | `sync` | `sync` | no | `Channel`, `SendMsg`, `RecvMsg`, `Share`, `Update` |
//! | `task` | `rt` | no (`BoxFuture`: alloc) | `Spawn`, `SpawnSet`, `SpawnWith`, `Join`, `YieldNow` |
//!
//! # Resources
//!
//! Effects that produce something lasting (opening a file, connecting a
//! socket, spawning a task) are [`Acquisition`](crate::Acquisition)s, not
//! operations: a core never owns the file or socket, it acquires one for the
//! length of a scope (see [`acquire`](crate::acquire)). They are generic over
//! the backend's handle type; the core names it as a type parameter with the
//! bounds it needs, and performs the operations in `io` or `net` on it:
//!
//! ```
//! # #[cfg(feature = "fs")] {
//! use std::io;
//! use seam_core::prelude::*;
//! use seam_core::ops::fs::Open;
//! use seam_core::ops::io::WriteAll;
//!
//! fn save<C, F>(c: &mut C, report: &str) -> io::Result<()>
//! where
//!     C: Provide<Open<F>>,
//!     F: for<'a> Perform<WriteAll<'a>>,
//! {
//!     c.acquire(Open::create("report.txt"), |_c, file| {
//!         file.perform(WriteAll(report.as_bytes()))
//!     })
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
#[cfg(feature = "sync")]
pub mod sync;
#[cfg(feature = "rt")]
pub mod task;
#[cfg(feature = "time")]
pub mod time;
