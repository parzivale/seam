//! Standard input, output and error.

use std::io;
use std::string::String;

/// Write bytes to standard output.
#[derive(Debug, Clone, Copy)]
pub struct Print<'a>(pub &'a [u8]);

/// Write bytes to standard error.
#[derive(Debug, Clone, Copy)]
pub struct EPrint<'a>(pub &'a [u8]);

/// Read one line from standard input, including the newline. `None` at end
/// of input.
#[derive(Debug, Clone, Copy, Default)]
pub struct ReadLine;

operations! {
    Print<'_> => io::Result<()>;
    EPrint<'_> => io::Result<()>;
    ReadLine => io::Result<Option<String>>;
}
