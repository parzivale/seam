//! Standard input, output and error, as the [`Stdin`], [`Stdout`] and
//! [`Stderr`] handles.

use seam_core::ops::stdio::{OpenStderr, OpenStdin, OpenStdout};
use seam_core::{Closing, Opening};
use tokio::io::AsyncWriteExt;

use crate::Tokio;

/// Standard input, acquired with [`OpenStdin`]. Performs `Read`.
///
/// Tokio reads standard input on its blocking pool, and never reads more
/// than the buffer it is given, so nothing is held back between handles. A
/// read cancelled while it waits loses whatever that blocking read then
/// returns.
#[derive(Debug)]
pub struct Stdin(tokio::io::Stdin);

/// Standard output, acquired with [`OpenStdout`]. Performs `Write` and
/// `Flush`; flushed when its scope succeeds.
#[derive(Debug)]
pub struct Stdout(tokio::io::Stdout);

/// Standard error, acquired with [`OpenStderr`]. Performs `Write` and
/// `Flush`; flushed when its scope succeeds.
#[derive(Debug)]
pub struct Stderr(tokio::io::Stderr);

read_ops!([] Stdin);
write_ops!([] Stdout);
write_ops!([] Stderr);

async_op!(perform [] Tokio, Opening<OpenStdin<Stdin>>, |_s, Opening(_, _)| {
    seam_core::io::Result::Ok(Stdin(tokio::io::stdin()))
});
async_op!(perform [] Tokio, Closing<OpenStdin<Stdin>>, |_s, Closing(_, _)| {
    seam_core::io::Result::Ok(())
});

async_op!(perform [] Tokio, Opening<OpenStdout<Stdout>>, |_s, Opening(_, _)| {
    seam_core::io::Result::Ok(Stdout(tokio::io::stdout()))
});
async_op!(perform [] Tokio, Closing<OpenStdout<Stdout>>, |_s, Closing(mut out, _)| {
    crate::sio(out.0.flush().await)
});

async_op!(perform [] Tokio, Opening<OpenStderr<Stderr>>, |_s, Opening(_, _)| {
    seam_core::io::Result::Ok(Stderr(tokio::io::stderr()))
});
async_op!(perform [] Tokio, Closing<OpenStderr<Stderr>>, |_s, Closing(mut err, _)| {
    crate::sio(err.0.flush().await)
});

// Every capability type opts in to seam's lifts.
impl seam_core::Capability for Stdin {}
impl seam_core::Capability for Stdout {}
impl seam_core::Capability for Stderr {}
