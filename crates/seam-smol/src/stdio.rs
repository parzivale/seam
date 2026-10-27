//! Standard input, output and error, as the [`Stdin`], [`Stdout`] and
//! [`Stderr`] handles.

use seam_core::ops::stdio::{OpenStderr, OpenStdin, OpenStdout};
use seam_core::{Closing, Opening};
use smol::io::AsyncWriteExt;

use crate::Smol;

/// Standard input, acquired with [`OpenStdin`]. Performs `Read`.
///
/// Each read runs on smol's blocking pool and never reads more than the
/// buffer it is given, so nothing is held back between handles. A read
/// cancelled while it waits loses whatever that blocking read then
/// returns.
#[derive(Debug)]
pub struct Stdin(());

/// Standard output, acquired with [`OpenStdout`]. Performs `Write` and
/// `Flush`; flushed when its scope succeeds.
#[derive(Debug)]
pub struct Stdout(smol::Unblock<std::io::Stdout>);

/// Standard error, acquired with [`OpenStderr`]. Performs `Write` and
/// `Flush`; flushed when its scope succeeds.
#[derive(Debug)]
pub struct Stderr(smol::Unblock<std::io::Stderr>);

async_op!(perform ['a] Stdin, seam_core::ops::io::Read<'a>, |_s, op| {
    let len = op.0.len();
    let (buf, read) = smol::unblock(move || {
        let mut buf = std::vec![0; len];
        let read = std::io::Read::read(&mut std::io::stdin(), &mut buf);
        (buf, read)
    })
    .await;
    let n = read?;
    op.0[..n].copy_from_slice(&buf[..n]);
    std::io::Result::Ok(n)
});
write_ops!([] Stdout);
write_ops!([] Stderr);

async_op!(perform [] Smol, Opening<OpenStdin<Stdin>>, |_s, Opening(_, _)| {
    seam_core::io::Result::Ok(Stdin(()))
});
async_op!(perform [] Smol, Closing<OpenStdin<Stdin>>, |_s, Closing(_, _)| {
    seam_core::io::Result::Ok(())
});

async_op!(perform [] Smol, Opening<OpenStdout<Stdout>>, |_s, Opening(_, _)| {
    seam_core::io::Result::Ok(Stdout(smol::Unblock::new(std::io::stdout())))
});
async_op!(perform [] Smol, Closing<OpenStdout<Stdout>>, |_s, Closing(mut out, _)| {
    crate::sio(out.0.flush().await)
});

async_op!(perform [] Smol, Opening<OpenStderr<Stderr>>, |_s, Opening(_, _)| {
    seam_core::io::Result::Ok(Stderr(smol::Unblock::new(std::io::stderr())))
});
async_op!(perform [] Smol, Closing<OpenStderr<Stderr>>, |_s, Closing(mut err, _)| {
    crate::sio(err.0.flush().await)
});

// Every capability type opts in to seam's lifts.
impl seam_core::Capability for Stdin {}
impl seam_core::Capability for Stdout {}
impl seam_core::Capability for Stderr {}
