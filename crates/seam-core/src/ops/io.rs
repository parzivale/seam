//! Reading and writing through an open handle (a file, a socket).
//!
//! These borrow their buffer, so bound them for every lifetime:
//!
//! ```
//! use seam_core::io;
//! use seam_core::Perform;
//! use seam_core::ops::io::{Flush, Read, WriteAll};
//!
//! /// Copies everything from `src` to `dst`, in 4 KiB chunks.
//! fn pump<R, W>(src: &mut R, dst: &mut W) -> io::Result<u64>
//! where
//!     R: for<'a> Perform<Read<'a>>,
//!     W: for<'a> Perform<WriteAll<'a>> + Perform<Flush>,
//! {
//!     let mut buf = [0; 4096];
//!     let mut total = 0;
//!     loop {
//!         let n = src.perform(Read(&mut buf))?;
//!         if n == 0 {
//!             dst.perform(Flush)?;
//!             return Ok(total);
//!         }
//!         dst.perform(WriteAll(&buf[..n]))?;
//!         total += n as u64;
//!     }
//! }
//! ```

use crate::io;
#[cfg(feature = "alloc")]
use alloc::vec::Vec;

/// Read into the buffer. Returns how many bytes were read; 0 means end of
/// input.
#[derive(Debug)]
pub struct Read<'a>(pub &'a mut [u8]);

/// Read everything that is left, appending it to the vector. Returns how
/// many bytes were read. Needs the `alloc` feature.
#[cfg(feature = "alloc")]
#[derive(Debug)]
pub struct ReadToEnd<'a>(pub &'a mut Vec<u8>);

/// Fill the whole buffer. Fails with `io::ErrorKind::UnexpectedEof` if
/// input ends first; what arrived is then left in the buffer, but how much
/// is unspecified.
#[derive(Debug)]
pub struct ReadExact<'a>(pub &'a mut [u8]);

/// Write some of the buffer. Returns how many bytes were written.
#[derive(Debug, Clone, Copy)]
pub struct Write<'a>(pub &'a [u8]);

/// Write the whole buffer.
#[derive(Debug, Clone, Copy)]
pub struct WriteAll<'a>(pub &'a [u8]);

/// Flush buffered writes.
#[derive(Debug, Clone, Copy, Default)]
pub struct Flush;

operations! {
    Read<'_> => io::Result<usize>;
    ReadExact<'_> => io::Result<()>;
    Write<'_> => io::Result<usize>;
    WriteAll<'_> => io::Result<()>;
    Flush => io::Result<()>;
}

#[cfg(feature = "alloc")]
operations! {
    ReadToEnd<'_> => io::Result<usize>;
}

/// Each gives up with `io::ErrorKind::TimedOut`.
#[cfg(feature = "time")]
mod timeouts {
    use super::*;
    use crate::ops::time::{TimesOut, io_timed_out};

    /// Nothing was read.
    impl TimesOut for Read<'_> {
        type Expired = io::Result<usize>;
        fn timed_out(self) -> Self::Expired {
            Err(io_timed_out())
        }
    }

    /// The deadline covers the whole read. What arrived before it is left
    /// in the buffer, as when `ReadExact` fails for any other reason.
    impl TimesOut for ReadExact<'_> {
        type Expired = io::Result<()>;
        fn timed_out(self) -> Self::Expired {
            Err(io_timed_out())
        }
    }

    /// The deadline covers the whole read. What arrived before it is left
    /// in the buffer, as when `ReadToEnd` fails for any other reason.
    #[cfg(feature = "alloc")]
    impl TimesOut for ReadToEnd<'_> {
        type Expired = io::Result<usize>;
        fn timed_out(self) -> Self::Expired {
            Err(io_timed_out())
        }
    }

    /// Nothing was written.
    impl TimesOut for Write<'_> {
        type Expired = io::Result<usize>;
        fn timed_out(self) -> Self::Expired {
            Err(io_timed_out())
        }
    }

    /// The deadline covers the whole write. Part of the buffer may have
    /// been written, as when `WriteAll` fails for any other reason.
    impl TimesOut for WriteAll<'_> {
        type Expired = io::Result<()>;
        fn timed_out(self) -> Self::Expired {
            Err(io_timed_out())
        }
    }
}
