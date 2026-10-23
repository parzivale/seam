//! Running commands, child processes, and exiting.

use std::convert::Infallible;
use std::io;
use std::process::Stdio;

use seam_core::ops::io::{Flush, Read, ReadToEnd, Write, WriteAll};
use seam_core::ops::process::{
    ChildId, CloseStdin, Exit, ExitStatus, Kill, Output, Pipe, ReadErr, ReadErrToEnd, RunCommand,
    SpawnChild, Wait,
};
use seam_core::{AsyncProvide, Inspect, Perform, Token};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{ChildStderr, ChildStdin, ChildStdout};

use crate::Tokio;

async_op!(perform [] Tokio, RunCommand, |_s, op| {
    let o = tokio::process::Command::from(op.0).output().await?;
    Ok(Output { code: o.status.code(), stdout: o.stdout, stderr: o.stderr })
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
    stdin: Option<ChildStdin>,
    stdout: Option<ChildStdout>,
    stderr: Option<ChildStderr>,
    status: Option<ExitStatus>,
}

fn stdio(pipe: Pipe) -> Stdio {
    match pipe {
        Pipe::Piped => Stdio::piped(),
        Pipe::Null => Stdio::null(),
        Pipe::Inherit => Stdio::inherit(),
    }
}

fn not_piped(stream: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::BrokenPipe,
        format!("the child's {stream} is not piped to this scope, or is closed"),
    )
}

impl Child {
    fn stdin(&mut self) -> io::Result<&mut ChildStdin> {
        self.stdin
            .as_mut()
            .ok_or_else(|| not_piped("standard input"))
    }

    fn stdout(&mut self) -> io::Result<&mut ChildStdout> {
        self.stdout
            .as_mut()
            .ok_or_else(|| not_piped("standard output"))
    }

    fn stderr(&mut self) -> io::Result<&mut ChildStderr> {
        self.stderr
            .as_mut()
            .ok_or_else(|| not_piped("standard error"))
    }

    /// Cancel-safe: the child keeps running, and can be waited for again.
    pub(crate) async fn wait(&mut self) -> io::Result<ExitStatus> {
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
    pub(crate) fn stdout_mut(&mut self) -> io::Result<&mut ChildStdout> {
        self.stdout()
    }

    #[cfg_attr(not(feature = "time"), allow(dead_code))]
    pub(crate) fn stderr_mut(&mut self) -> io::Result<&mut ChildStderr> {
        self.stderr()
    }
}

impl AsyncProvide<SpawnChild<Child>> for Tokio {
    async fn provide_async(&mut self, op: SpawnChild<Child>, _: Token) -> io::Result<Child> {
        let mut cmd = tokio::process::Command::from(op.command);
        cmd.stdin(stdio(op.stdin))
            .stdout(stdio(op.stdout))
            .stderr(stdio(op.stderr))
            .kill_on_drop(true);
        let mut inner = cmd.spawn()?;
        Ok(Child {
            stdin: inner.stdin.take(),
            stdout: inner.stdout.take(),
            stderr: inner.stderr.take(),
            inner,
            status: None,
        })
    }

    /// Closes the pipes, so a child blocked writing to one cannot hang the
    /// wait, then waits for it.
    async fn close_async(&mut self, mut child: Child) -> io::Result<()> {
        child.stdout = None;
        child.stderr = None;
        child.wait().await.map(drop)
    }
}

async_op!(perform ['a] Child, Write<'a>, |s, op| s.stdin()?.write(op.0).await);
async_op!(perform ['a] Child, WriteAll<'a>, |s, op| s.stdin()?.write_all(op.0).await);
async_op!(perform [] Child, Flush, |s, _op| s.stdin()?.flush().await);
async_op!(perform ['a] Child, Read<'a>, |s, op| s.stdout()?.read(op.0).await);
async_op!(perform ['a] Child, ReadToEnd<'a>, |s, op| s.stdout()?.read_to_end(op.0).await);
async_op!(perform ['a] Child, ReadErr<'a>, |s, op| s.stderr()?.read(op.0).await);
async_op!(perform ['a] Child, ReadErrToEnd<'a>, |s, op| s.stderr()?.read_to_end(op.0).await);
async_op!(perform [] Child, Wait, |s, _op| s.wait().await);

impl Perform<CloseStdin> for Child {
    fn perform(&mut self, _: CloseStdin) {
        self.stdin = None;
    }
}

/// Starts killing the child; [`Wait`] then reports how it ended.
impl Perform<Kill> for Child {
    fn perform(&mut self, _: Kill) -> io::Result<()> {
        if self.status.is_some() {
            return Ok(());
        }
        self.inner.start_kill()
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
