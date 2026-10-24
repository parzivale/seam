//! Operation types for common effects, one module per capability feature.
//!
//! Backends (`seam-std`, `seam-tokio`, `seam-test`) implement
//! [`Inspect`](crate::Inspect) and [`Perform`](crate::Perform) for these
//! types, so a core written against them runs on any backend.
//!
//! The operations are **primitives**: the smallest effects a backend must
//! provide. Everything that can be built from them is, in these modules, as
//! plain functions in three forms (sync, `_async`, `_send`): reading a
//! whole file, writing all of a buffer, reading lines, resolving every
//! address of a name, and so on. A few effects that could be built that way
//! stay primitives, each saying why: `CreateAtomic` and recursive
//! `RemoveDir` (safety), `CopyFile` (speed and permissions) and
//! `RunCommand` (reading two pipes at once).
//!
//! | Module | Feature | Needs std | Primitives | Built on them |
//! |---|---|---|---|---|
//! | `time` | `time` | no (`WallClock`: yes) | `Now`, `Sleep`, `WallClock`, `Timeout` | |
//! | `random` | `random` | no | `FillRandom` | `random_u64` |
//! | `io` | `io` (or `fs`, `net`, `process`, `io-std`) | no | `Read`, `Write`, `Flush`, `Seek` on handles | `read_exact`, `write_all`, `copy`, `BufReader`, `read_to_end` (alloc) |
//! | `fs` | `fs` | yes | `Open<'a, M, F>`, `CreateAtomic`, `ReadDir`, `Metadata`, `Stat`, `SetLen`, `CreateDir`, `RemoveDir`, ... | `read_file`, `write_file`, `create_dir_all`, `sweep_atomic` |
//! | `net` | `net` | no | `TcpConnect<S>`, `TcpBind<L>`, `Accept<S>`, `ResolveInto`, ... | `resolve` (alloc) |
//! | `env` | `env` | yes | `Var`, `SetVar`, `Args`, `CurrentDir`, ... | |
//! | `process` | `process` | yes | `RunCommand`, `SpawnChild`, `Wait`, `ReadErr`, `Exit`, ... | `read_err_to_end` |
//! | `stdio` | `io-std` | no | `OpenStdin`, `OpenStdout`, `OpenStderr` | `print`, `eprint` |
//! | `signal` | `signal` | no | `CtrlC` | |
//! | `sync` | `sync` | no | `Channel`, `SendMsg`, `RecvMsg`, `Share`, `Load`, `Update` | `try_send`, `try_recv`, `store` |
//! | `task` | `rt` | no (`BoxFuture`: alloc) | `Spawn`, `SpawnSet`, `SpawnWith`, `Join`, `YieldNow`, ... | |
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
//! use seam_core::io;
//! use seam_core::prelude::*;
//! use seam_core::ops::fs::{Open, WriteOnly};
//! use seam_core::ops::io::{Write, write_all};
//!
//! fn save<C, F>(c: &mut C, report: &str) -> io::Result<()>
//! where
//!     C: for<'a> Provide<Open<'a, WriteOnly, F>>,
//!     F: for<'a> Perform<Write<'a>>,
//! {
//!     c.acquire(Open::create("report.txt"), |_c, file| {
//!         write_all(file, report.as_bytes())
//!     })
//! }
//! # }
//! ```
//!
//! The caller picks `F` by passing a backend: `save(&mut Std::new(), ..)`
//! infers `seam_std::fs::File<WriteOnly>`.

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
