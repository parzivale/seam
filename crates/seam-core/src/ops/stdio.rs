//! Standard input, output and error, as handles.
//!
//! Each stream is an acquisition, lending a handle for a scope:
//! [`OpenStdin`] a handle to [`Read`] from, [`OpenStdout`] and
//! [`OpenStderr`] handles to [`Write`] to and [`Flush`]. Everything else is
//! the [`io`](super::io) helpers on those handles: lines through
//! [`BufReader`](super::io::BufReader), fixed-size frames through
//! [`read_exact`](super::io::read_exact), and [`print`] and [`eprint`] here.
//!
//! ```
//! use seam_core::io;
//! use seam_core::prelude::*;
//! use seam_core::ops::io::{BufReader, Flush, Read, Write, write_all};
//! use seam_core::ops::stdio::{OpenStdin, OpenStdout};
//!
//! /// Asks a yes/no question. End of input counts as no.
//! fn confirm<C, I, O>(c: &mut C, question: &str) -> io::Result<bool>
//! where
//!     C: Provide<OpenStdin<I>> + Provide<OpenStdout<O>>,
//!     I: for<'a> Perform<Read<'a>>,
//!     O: for<'a> Perform<Write<'a>> + Perform<Flush>,
//! {
//!     c.acquire(OpenStdout::new(), |_, out| {
//!         write_all(out, format!("{question} [y/N] ").as_bytes())?;
//!         out.perform(Flush)
//!     })?;
//!     c.acquire(OpenStdin::new(), |_, input| {
//!         let mut answer = [0; 8];
//!         let len = BufReader::<64>::new()
//!             .read_line(input, &mut answer)?
//!             .map_or(0, |line| line.len);
//!         Ok(matches!(answer[..len].trim_ascii(), b"y" | b"Y" | b"yes"))
//!     })
//! }
//! ```
//!
//! A `BufReader` keeps what it read past the end of a line. Read stdin
//! through one reader for as long as it is in use; one made per call, as
//! above, drops anything the user typed ahead.

#[allow(unused_imports)]
use super::io::{Flush, Read, Write};
use crate::io;
use crate::three_ways::three_ways;
use core::marker::PhantomData;

macro_rules! streams {
    ($($(#[$m:meta])* $ty:ident;)*) => {$(
        $(#[$m])*
        pub struct $ty<H> {
            handle: PhantomData<fn() -> H>,
        }

        impl<H> $ty<H> {
            /// Describes opening the stream.
            pub const fn new() -> Self {
                Self { handle: PhantomData }
            }
        }

        impl<H> Default for $ty<H> {
            fn default() -> Self {
                Self::new()
            }
        }

        impl<H> Clone for $ty<H> {
            fn clone(&self) -> Self {
                *self
            }
        }

        impl<H> Copy for $ty<H> {}

        impl<H> core::fmt::Debug for $ty<H> {
            fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                f.write_str(stringify!($ty))
            }
        }

        impl<H> crate::Acquisition for $ty<H> {
            type Resource = H;
            type Error = io::Error;
        }
    )*};
}

streams! {
    /// Lend standard input, as a handle of type `H` that performs [`Read`].
    /// Reads return what has arrived, without waiting for a whole line.
    OpenStdin;
    /// Lend standard output, as a handle of type `H` that performs [`Write`]
    /// and [`Flush`]. A scope that succeeds flushes it.
    OpenStdout;
    /// Lend standard error, as a handle of type `H` that performs [`Write`]
    /// and [`Flush`]. A scope that succeeds flushes it.
    OpenStderr;
}

crate::root::keys!(
    [H] OpenStdin<H> => OpenStdin,
    [H] OpenStdout<H> => OpenStdout,
    [H] OpenStderr<H> => OpenStderr,
);

three_ways! {
    /// Writes `bytes` to standard output, and flushes it (closing the
    /// handle does).
    pub fn print, print_async, print_send
    [C, H] (c: &mut C, bytes: &[u8]) -> io::Result<()>
    where
        sync {
            C: crate::Provide<OpenStdout<H>>,
            H: for<'a> crate::Perform<Write<'a>>,
        }
        async {
            C: crate::AsyncProvide<OpenStdout<H>>,
            H: for<'a> crate::future::AsyncPerform<Write<'a>>,
        }
        send {
            C: crate::SendProvide<OpenStdout<H>> + Send,
            H: for<'a> crate::future::SendPerform<Write<'a>> + Send,
        }
    {
        acquire!(c, OpenStdout::new(), |_, out| {
            call!(super::io::write_all, super::io::write_all_async, super::io::write_all_send; (out, bytes))
        })
    }

    /// Writes `bytes` to standard error, and flushes it (closing the handle
    /// does).
    pub fn eprint, eprint_async, eprint_send
    [C, H] (c: &mut C, bytes: &[u8]) -> io::Result<()>
    where
        sync {
            C: crate::Provide<OpenStderr<H>>,
            H: for<'a> crate::Perform<Write<'a>>,
        }
        async {
            C: crate::AsyncProvide<OpenStderr<H>>,
            H: for<'a> crate::future::AsyncPerform<Write<'a>>,
        }
        send {
            C: crate::SendProvide<OpenStderr<H>> + Send,
            H: for<'a> crate::future::SendPerform<Write<'a>> + Send,
        }
    {
        acquire!(c, OpenStderr::new(), |_, out| {
            call!(super::io::write_all, super::io::write_all_async, super::io::write_all_send; (out, bytes))
        })
    }
}
