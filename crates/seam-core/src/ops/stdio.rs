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

operations! {
    Print<'_> => io::Result<()>;
    EPrint<'_> => io::Result<()>;
}

#[cfg(feature = "alloc")]
operations! {
    ReadLine => io::Result<Option<String>>;
}

crate::root::in_kind!(crate::kinds::Stdio: ['a] Print<'a>, ['a] EPrint<'a>);
#[cfg(feature = "alloc")]
crate::root::in_kind!(crate::kinds::Stdio: [] ReadLine);
