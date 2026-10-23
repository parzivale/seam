//! Standard input, output and error.
//!
//! ```
//! use seam_core::io;
//! use seam_core::Perform;
//! use seam_core::ops::stdio::{Print, ReadLine};
//!
//! /// Asks a yes/no question. End of input counts as no.
//! fn confirm<C>(c: &mut C, question: &str) -> io::Result<bool>
//! where
//!     C: for<'a> Perform<Print<'a>> + Perform<ReadLine>,
//! {
//!     c.perform(Print(format!("{question} [y/N] ").as_bytes()))?;
//!     let answer = c.perform(ReadLine)?.unwrap_or_default();
//!     Ok(matches!(answer.trim(), "y" | "Y" | "yes"))
//! }
//! ```

use crate::io;
#[cfg(feature = "alloc")]
use alloc::string::String;

/// Write bytes to standard output.
#[derive(Debug, Clone, Copy)]
pub struct Print<'a>(pub &'a [u8]);

/// Write bytes to standard error.
#[derive(Debug, Clone, Copy)]
pub struct EPrint<'a>(pub &'a [u8]);

/// Read one line from standard input, including the newline. `None` at end
/// of input. Needs the `alloc` feature.
#[cfg(feature = "alloc")]
#[derive(Debug, Clone, Copy, Default)]
pub struct ReadLine;

/// Read one line from standard input into the buffer, including the
/// newline, without allocating. `None` at end of input. The bytes are not
/// checked to be UTF-8.
///
/// A line longer than the buffer fills it and comes back with
/// [`complete`](Line::complete) false; the next read continues the same
/// line. An empty buffer reads nothing.
///
/// ```
/// use seam_core::io;
/// use seam_core::Perform;
/// use seam_core::ops::stdio::{Line, ReadLineInto};
///
/// /// Counts the newlines on standard input, as `wc -l` does, however long
/// /// the lines are.
/// fn count_lines<C>(c: &mut C) -> io::Result<u64>
/// where
///     C: for<'a> Perform<ReadLineInto<'a>>,
/// {
///     let mut buf = [0; 256];
///     let mut lines = 0;
///     while let Some(Line { len, .. }) = c.perform(ReadLineInto(&mut buf))? {
///         lines += u64::from(buf[..len].ends_with(b"\n"));
///     }
///     Ok(lines)
/// }
/// ```
#[derive(Debug)]
pub struct ReadLineInto<'a>(pub &'a mut [u8]);

/// What [`ReadLineInto`] read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Line {
    /// How many bytes were read, from the start of the buffer.
    pub len: usize,
    /// Whether the line ended: with a newline, which is then the last byte
    /// read, or with the end of input. False when the buffer filled first.
    pub complete: bool,
}

operations! {
    Print<'_> => io::Result<()>;
    EPrint<'_> => io::Result<()>;
    ReadLineInto<'_> => io::Result<Option<Line>>;
}

#[cfg(feature = "alloc")]
operations! {
    ReadLine => io::Result<Option<String>>;
}

crate::root::in_kind!(crate::kinds::Stdio: ['a] Print<'a>, ['a] EPrint<'a>, ['a] ReadLineInto<'a>);
#[cfg(feature = "alloc")]
crate::root::in_kind!(crate::kinds::Stdio: [] ReadLine);
