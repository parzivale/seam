//! Standard input, output and error, as the [`Stdin`], [`Stdout`] and
//! [`Stderr`] handles.

use seam_core::ops::stdio::{OpenStderr, OpenStdin, OpenStdout};
use seam_core::{Provide, Token};

use crate::Std;

/// Standard input, acquired with [`OpenStdin`]. Performs `Read`, through
/// std's own buffer, which is shared by every handle, so nothing is lost
/// between scopes.
#[derive(Debug)]
pub struct Stdin(std::io::Stdin);

/// Standard output, acquired with [`OpenStdout`]. Performs `Write` and
/// `Flush`; flushed when its scope succeeds.
#[derive(Debug)]
pub struct Stdout(std::io::Stdout);

/// Standard error, acquired with [`OpenStderr`]. Performs `Write` and
/// `Flush`; flushed when its scope succeeds.
#[derive(Debug)]
pub struct Stderr(std::io::Stderr);

crate::read_ops!([] Stdin);
crate::write_ops!([] Stdout);
crate::write_ops!([] Stderr);

impl Provide<OpenStdin<Stdin>> for Std {
    fn provide(&mut self, _: OpenStdin<Stdin>, _: Token) -> seam_core::io::Result<Stdin> {
        Ok(Stdin(std::io::stdin()))
    }

    fn close(&mut self, _: Stdin) -> seam_core::io::Result<()> {
        Ok(())
    }
}

impl Provide<OpenStdout<Stdout>> for Std {
    fn provide(&mut self, _: OpenStdout<Stdout>, _: Token) -> seam_core::io::Result<Stdout> {
        Ok(Stdout(std::io::stdout()))
    }

    fn close(&mut self, mut out: Stdout) -> seam_core::io::Result<()> {
        crate::sio(std::io::Write::flush(&mut out.0))
    }
}

impl Provide<OpenStderr<Stderr>> for Std {
    fn provide(&mut self, _: OpenStderr<Stderr>, _: Token) -> seam_core::io::Result<Stderr> {
        Ok(Stderr(std::io::stderr()))
    }

    fn close(&mut self, mut err: Stderr) -> seam_core::io::Result<()> {
        crate::sio(std::io::Write::flush(&mut err.0))
    }
}

// Every capability type opts in to seam's lifts.
impl seam_core::Capability for Stdin {}
impl seam_core::Capability for Stdout {}
impl seam_core::Capability for Stderr {}
