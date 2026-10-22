//! Operation types for common effects, one module per capability feature.
//!
//! Backends (`seam-std`, `seam-tokio`) implement [`Inspect`](crate::Inspect)
//! and [`Perform`](crate::Perform) for these types, so a core written against
//! them runs on any backend, or on test fakes.
//!
//! Operations that create a resource (opening a file, connecting a socket)
//! are generic over the handle type, since each backend has its own. Bounds
//! name the handle as a type parameter, and inference fills it in:
//!
//! ```ignore
//! fn run<C, F>(c: &mut C) -> io::Result<()>
//! where
//!     C: Perform<Open<F>>,
//!     F: for<'a> Perform<WriteAll<'a>>,
//! { /* ... */ }
//! ```

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
#[cfg(feature = "time")]
pub mod time;
