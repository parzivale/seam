//! Reading and writing through an open handle (a file, a socket).

use std::io;
use std::vec::Vec;

/// Read into the buffer. Returns how many bytes were read; 0 means end of
/// input.
#[derive(Debug)]
pub struct Read<'a>(pub &'a mut [u8]);

/// Read everything that is left, appending it to the vector. Returns how
/// many bytes were read.
#[derive(Debug)]
pub struct ReadToEnd<'a>(pub &'a mut Vec<u8>);

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
    ReadToEnd<'_> => io::Result<usize>;
    Write<'_> => io::Result<usize>;
    WriteAll<'_> => io::Result<()>;
    Flush => io::Result<()>;
}
