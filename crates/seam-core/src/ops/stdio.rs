//! Standard input, output and error, as handles.
//!
//! Each stream is an acquisition, lending a handle for a scope:
//! [`OpenStdin`] a handle to [`Read`] from, [`OpenStdout`] and
//! [`OpenStderr`] handles to [`Write`] to and [`Flush`]. Everything else is
//! the [`io`](super::io) helpers on those handles: lines through
//! [`BufReader`](super::io::BufReader), fixed-size frames through
//! [`read_exact`](super::io::read_exact), and [`print`], [`println`],
//! [`eprint`], [`eprintln`] and [`read_line`] (`alloc`) here.
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
//! above, drops anything the user typed ahead. [`read_line`] reads a byte
//! at a time instead, so it never reads past the line it returns: slower,
//! but safe to call once per prompt.

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

three_ways! {
    /// Writes `text` and a newline to standard output, in one scope, and
    /// flushes it.
    pub fn println, println_async, println_send
    [C, H] (c: &mut C, text: &str) -> io::Result<()>
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
            call!(super::io::write_all, super::io::write_all_async, super::io::write_all_send; (out, text.as_bytes()))?;
            call!(super::io::write_all, super::io::write_all_async, super::io::write_all_send; (out, b"\n"))
        })
    }

    /// Writes `text` and a newline to standard error, in one scope, and
    /// flushes it.
    pub fn eprintln, eprintln_async, eprintln_send
    [C, H] (c: &mut C, text: &str) -> io::Result<()>
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
            call!(super::io::write_all, super::io::write_all_async, super::io::write_all_send; (out, text.as_bytes()))?;
            call!(super::io::write_all, super::io::write_all_async, super::io::write_all_send; (out, b"\n"))
        })
    }
}

#[cfg(feature = "alloc")]
three_ways! {
    /// Reads one line from standard input, without its `\n` or `\r\n`.
    /// `None` at the end of input; a last line with no newline is returned
    /// as is. Needs `alloc`.
    ///
    /// Reads a byte at a time, so nothing past the line is consumed and
    /// the next call (or any other reader of stdin) sees the rest. Fails
    /// with `io::ErrorKind::InvalidData` if the line is not UTF-8.
    pub fn read_line, read_line_async, read_line_send
    [C, H] (c: &mut C) -> io::Result<Option<alloc::string::String>>
    where
        sync {
            C: crate::Provide<OpenStdin<H>>,
            H: for<'a> crate::Perform<Read<'a>>,
        }
        async {
            C: crate::AsyncProvide<OpenStdin<H>>,
            H: for<'a> crate::future::AsyncPerform<Read<'a>>,
        }
        send {
            C: crate::SendProvide<OpenStdin<H>> + Send,
            H: for<'a> crate::future::SendPerform<Read<'a>> + Send,
        }
    {
        let line = acquire!(c, OpenStdin::new(), |_, input| {
            let mut line = alloc::vec::Vec::new();
            let mut byte = [0];
            loop {
                match run!(input, Read(&mut byte)) {
                    Ok(0) if line.is_empty() => return Ok(None),
                    Ok(0) => break,
                    Ok(_) if byte[0] == b'\n' => break,
                    Ok(_) => line.push(byte[0]),
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                    Err(e) => return Err(e),
                }
            }
            Ok(Some(line))
        })?;
        let Some(mut line) = line else { return Ok(None) };
        if line.last() == Some(&b'\r') {
            line.pop();
        }
        alloc::string::String::from_utf8(line).map(Some).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidData, "the line is not valid UTF-8")
        })
    }
}
