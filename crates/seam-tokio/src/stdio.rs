//! Standard input, output and error, as the [`Stdin`], [`Stdout`] and
//! [`Stderr`] handles.

use seam_core::ops::stdio::{OpenStderr, OpenStdin, OpenStdout};
use seam_core::{AsyncProvide, Token};
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

impl AsyncProvide<OpenStdin<Stdin>> for Tokio {
    async fn provide_async(
        &mut self,
        _: OpenStdin<Stdin>,
        _: Token,
    ) -> seam_core::io::Result<Stdin> {
        Ok(Stdin(tokio::io::stdin()))
    }

    async fn close_async(&mut self, _: Stdin) -> seam_core::io::Result<()> {
        Ok(())
    }
}

impl AsyncProvide<OpenStdout<Stdout>> for Tokio {
    async fn provide_async(
        &mut self,
        _: OpenStdout<Stdout>,
        _: Token,
    ) -> seam_core::io::Result<Stdout> {
        Ok(Stdout(tokio::io::stdout()))
    }

    async fn close_async(&mut self, mut out: Stdout) -> seam_core::io::Result<()> {
        crate::sio(out.0.flush().await)
    }
}

impl AsyncProvide<OpenStderr<Stderr>> for Tokio {
    async fn provide_async(
        &mut self,
        _: OpenStderr<Stderr>,
        _: Token,
    ) -> seam_core::io::Result<Stderr> {
        Ok(Stderr(tokio::io::stderr()))
    }

    async fn close_async(&mut self, mut err: Stderr) -> seam_core::io::Result<()> {
        crate::sio(err.0.flush().await)
    }
}

// Every capability type opts in to seam's lifts.
impl seam_core::Capability for Stdin {}
impl seam_core::Capability for Stdout {}
impl seam_core::Capability for Stderr {}
