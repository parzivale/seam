//! Standard input, output and error: scripted input, captured output, and
//! the [`Stdin`], [`Stdout`] and [`Stderr`] handles.

use seam_core::io;
use std::collections::VecDeque;
use std::vec::Vec;

use seam_core::ops::io::{Flush, Read, Write};
use seam_core::ops::stdio::{OpenStderr, OpenStdin, OpenStdout};
use seam_core::{Perform, Provide, Token};

use crate::Sim;
use crate::world::Shared;

/// The streams, in the shared world, so that handles reach them while the
/// `Sim` that opened them is borrowed.
#[derive(Debug, Default)]
pub(crate) struct Stdio {
    stdin: VecDeque<u8>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

impl Sim {
    /// Appends bytes to standard input. Input is a finite script: once it is
    /// consumed, reads report the end of input.
    pub fn feed_stdin(&mut self, bytes: impl AsRef<[u8]>) -> &mut Self {
        self.world.lock().stdio.stdin.extend(bytes.as_ref());
        self
    }

    /// Everything the core has printed to standard output.
    pub fn stdout(&self) -> Vec<u8> {
        self.world.lock().stdio.stdout.clone()
    }

    /// Everything the core has printed to standard error.
    pub fn stderr(&self) -> Vec<u8> {
        self.world.lock().stdio.stderr.clone()
    }

    /// Takes standard output so far, leaving it empty.
    pub fn take_stdout(&mut self) -> Vec<u8> {
        core::mem::take(&mut self.world.lock().stdio.stdout)
    }

    /// Takes standard error so far, leaving it empty.
    pub fn take_stderr(&mut self) -> Vec<u8> {
        core::mem::take(&mut self.world.lock().stdio.stderr)
    }

    /// Prints what a simulated child inherited.
    #[cfg(feature = "process")]
    pub(crate) fn print_inherited(&mut self, out: &[u8], err: &[u8]) {
        let mut world = self.world.lock();
        world.stdio.stdout.extend_from_slice(out);
        world.stdio.stderr.extend_from_slice(err);
    }
}

/// Standard input, acquired with [`OpenStdin`]. Performs `Read`; never
/// waits, since input is a finite script: once it is consumed, reads return
/// 0, the end of input.
#[derive(Debug)]
pub struct Stdin(Shared);

/// Standard output, acquired with [`OpenStdout`]. Performs `Write` and
/// `Flush`; what it writes lands at once, in [`Sim::stdout`].
#[derive(Debug)]
pub struct Stdout(Shared);

/// Standard error, acquired with [`OpenStderr`]. Performs `Write` and
/// `Flush`; what it writes lands at once, in [`Sim::stderr`].
#[derive(Debug)]
pub struct Stderr(Shared);

impl Perform<Read<'_>> for Stdin {
    fn perform(&mut self, op: Read<'_>) -> io::Result<usize> {
        let mut world = self.0.lock();
        let stdin = &mut world.stdio.stdin;
        let n = stdin.len().min(op.0.len());
        for (dst, src) in op.0.iter_mut().zip(stdin.drain(..n)) {
            *dst = src;
        }
        Ok(n)
    }
}

impl Perform<Write<'_>> for Stdout {
    fn perform(&mut self, op: Write<'_>) -> io::Result<usize> {
        self.0.lock().stdio.stdout.extend_from_slice(op.0);
        Ok(op.0.len())
    }
}

impl Perform<Write<'_>> for Stderr {
    fn perform(&mut self, op: Write<'_>) -> io::Result<usize> {
        self.0.lock().stdio.stderr.extend_from_slice(op.0);
        Ok(op.0.len())
    }
}

/// Writes land at once, so there is nothing to flush.
impl Perform<Flush> for Stdout {
    fn perform(&mut self, _: Flush) -> io::Result<()> {
        Ok(())
    }
}

/// Writes land at once, so there is nothing to flush.
impl Perform<Flush> for Stderr {
    fn perform(&mut self, _: Flush) -> io::Result<()> {
        Ok(())
    }
}

macro_rules! open {
    ($($op:ident => $handle:ident),*) => {$(
        impl Provide<$op<$handle>> for Sim {
            fn provide(&mut self, _: $op<$handle>, _: Token) -> io::Result<$handle> {
                Ok($handle(self.world.clone()))
            }

            fn close(&mut self, _: $handle) -> io::Result<()> {
                Ok(())
            }
        }
    )*};
}
open!(OpenStdin => Stdin, OpenStdout => Stdout, OpenStderr => Stderr);

// Every capability type opts in to seam's lifts.
impl seam_core::Capability for Stdin {}
impl seam_core::Capability for Stdout {}
impl seam_core::Capability for Stderr {}
