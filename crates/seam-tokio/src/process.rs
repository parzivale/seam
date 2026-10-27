//! Running commands, child processes, and exiting.

use std::convert::Infallible;
use std::io;
use std::process::Stdio;

use seam_core::ops::io::{Flush, Read, Write};
use seam_core::ops::process::{
    ChildId, ChildPipes, CloseStdin, Exit, ExitStatus, Kill, Output, Pipe, ReadErr, RunCommand,
    SpawnChild, SplitChild, Wait,
};
use seam_core::{Closing, Inspect, Opening, Perform};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{ChildStderr as RawStderr, ChildStdin as RawStdin, ChildStdout as RawStdout};

use crate::Tokio;

async_op!(perform [] Tokio, RunCommand, |_s, op| {
    let o = tokio::process::Command::from(op.0).output().await?;
    seam_core::io::Result::Ok(Output { code: o.status.code(), stdout: o.stdout, stderr: o.stderr })
});

impl Perform<Exit> for Tokio {
    fn perform(&mut self, op: Exit) -> Infallible {
        std::process::exit(op.0)
    }
}

/// A running child process, acquired with [`SpawnChild`]. Writing goes to
/// its standard input and reading comes from its standard output;
/// [`ReadErr`] reads its standard error.
///
/// When its scope succeeds, its pipes are closed and the scope waits for it
/// to exit. When the scope fails, panics or is cancelled, it is killed;
/// Tokio reaps it in the background.
#[derive(Debug)]
pub struct Child {
    inner: tokio::process::Child,
    stdin: Option<RawStdin>,
    stdout: Option<RawStdout>,
    stderr: Option<RawStderr>,
    status: Option<ExitStatus>,
}

/// A [`SplitChild`]'s pipe to the child's standard input. `None` inside if
/// it is not piped, or was closed.
#[derive(Debug)]
pub struct ChildStdin(Option<RawStdin>);

/// A [`SplitChild`]'s pipe from the child's standard output.
#[derive(Debug)]
pub struct ChildStdout(pub(crate) Option<RawStdout>);

/// A [`SplitChild`]'s pipe from the child's standard error.
#[derive(Debug)]
pub struct ChildStderr(pub(crate) Option<RawStderr>);

fn stdio(pipe: Pipe) -> Stdio {
    match pipe {
        Pipe::Piped => Stdio::piped(),
        Pipe::Null => Stdio::null(),
        Pipe::Inherit => Stdio::inherit(),
    }
}

fn not_piped(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::BrokenPipe, message)
}

fn stdin(pipe: &mut Option<RawStdin>) -> seam_core::io::Result<&mut RawStdin> {
    crate::sio(pipe.as_mut().ok_or_else(|| {
        not_piped("the child's standard input is not piped to this scope, or is closed")
    }))
}

pub(crate) fn stdout(pipe: &mut Option<RawStdout>) -> seam_core::io::Result<&mut RawStdout> {
    crate::sio(pipe.as_mut().ok_or_else(|| {
        not_piped("the child's standard output is not piped to this scope, or is split")
    }))
}

pub(crate) fn stderr(pipe: &mut Option<RawStderr>) -> seam_core::io::Result<&mut RawStderr> {
    crate::sio(pipe.as_mut().ok_or_else(|| {
        not_piped("the child's standard error is not piped to this scope, or is split")
    }))
}

impl Child {
    fn stdin(&mut self) -> seam_core::io::Result<&mut RawStdin> {
        stdin(&mut self.stdin)
    }

    fn stdout(&mut self) -> seam_core::io::Result<&mut RawStdout> {
        stdout(&mut self.stdout)
    }

    fn stderr(&mut self) -> seam_core::io::Result<&mut RawStderr> {
        stderr(&mut self.stderr)
    }

    /// Cancel-safe: the child keeps running, and can be waited for again.
    pub(crate) async fn wait(&mut self) -> seam_core::io::Result<ExitStatus> {
        self.stdin = None;
        if let Some(s) = self.status {
            return Ok(s);
        }
        let s = ExitStatus {
            code: self.inner.wait().await?.code(),
        };
        self.status = Some(s);
        Ok(s)
    }

    #[cfg_attr(not(feature = "time"), allow(dead_code))]
    pub(crate) fn stdout_mut(&mut self) -> seam_core::io::Result<&mut RawStdout> {
        self.stdout()
    }

    #[cfg_attr(not(feature = "time"), allow(dead_code))]
    pub(crate) fn stderr_mut(&mut self) -> seam_core::io::Result<&mut RawStderr> {
        self.stderr()
    }
}

async_op!(perform [] Tokio, Opening<SpawnChild<Child>>, |_s, Opening(op, _)| {
    let mut cmd = tokio::process::Command::from(op.command);
    cmd.stdin(stdio(op.stdin))
        .stdout(stdio(op.stdout))
        .stderr(stdio(op.stderr))
        .kill_on_drop(true);
    let mut inner = cmd.spawn()?;
    std::io::Result::Ok(Child {
        stdin: inner.stdin.take(),
        stdout: inner.stdout.take(),
        stderr: inner.stderr.take(),
        inner,
        status: None,
    })
});
// Closes the pipes, so a child blocked writing to one cannot hang the
// wait, then waits for it.
async_op!(perform [] Tokio, Closing<SpawnChild<Child>>, |_s, Closing(mut child, _)| {
    child.stdout = None;
    child.stderr = None;
    child.wait().await.map(drop)
});

async_op!(perform ['a] Child, Write<'a>, |s, op| s.stdin()?.write(op.0).await);
async_op!(perform [] Child, Flush, |s, _op| s.stdin()?.flush().await);
async_op!(perform ['a] Child, Read<'a>, |s, op| s.stdout()?.read(op.0).await);
async_op!(perform ['a] Child, ReadErr<'a>, |s, op| s.stderr()?.read(op.0).await);
async_op!(perform [] Child, Wait, |s, _op| s.wait().await);

// Takes the pipes out of the child; closing the scope puts them back.
async_op!(
    perform [] Child,
    Opening<SplitChild<ChildStdin, ChildStdout, ChildStderr>>,
    |s, Opening(_, _)| {
        seam_core::io::Result::Ok(ChildPipes {
            stdin: ChildStdin(s.stdin.take()),
            stdout: ChildStdout(s.stdout.take()),
            stderr: ChildStderr(s.stderr.take()),
        })
    }
);
async_op!(
    perform [] Child,
    Closing<SplitChild<ChildStdin, ChildStdout, ChildStderr>>,
    |s, Closing(pipes, _)| {
        // A child already waited for has had its input closed.
        if s.status.is_none() {
            s.stdin = pipes.stdin.0;
        }
        s.stdout = pipes.stdout.0;
        s.stderr = pipes.stderr.0;
        seam_core::io::Result::Ok(())
    }
);

async_op!(perform ['a] ChildStdin, Write<'a>, |s, op| stdin(&mut s.0)?.write(op.0).await);
async_op!(perform [] ChildStdin, Flush, |s, _op| stdin(&mut s.0)?.flush().await);
async_op!(perform ['a] ChildStdout, Read<'a>, |s, op| stdout(&mut s.0)?.read(op.0).await);
async_op!(perform ['a] ChildStderr, Read<'a>, |s, op| stderr(&mut s.0)?.read(op.0).await);

impl Perform<CloseStdin> for ChildStdin {
    fn perform(&mut self, _: CloseStdin) {
        self.0 = None;
    }
}

impl Perform<CloseStdin> for Child {
    fn perform(&mut self, _: CloseStdin) {
        self.stdin = None;
    }
}

/// Starts killing the child; [`Wait`] then reports how it ended.
impl Perform<Kill> for Child {
    fn perform(&mut self, _: Kill) -> seam_core::io::Result<()> {
        if self.status.is_some() {
            return Ok(());
        }
        crate::sio(self.inner.start_kill())
    }
}

/// `None` once the child has been waited for.
impl Inspect<ChildId> for Child {
    fn inspect(&self, _: ChildId) -> Option<u32> {
        self.inner.id()
    }
}

// Every capability type opts in to seam's lifts.
impl seam_core::Capability for Child {}
impl seam_core::Capability for ChildStdin {}
impl seam_core::Capability for ChildStdout {}
impl seam_core::Capability for ChildStderr {}
