//! Running commands, child processes, and exiting.

use std::convert::Infallible;
use std::io::{Read as _, Write as _};
use std::process::{
    ChildStderr as RawStderr, ChildStdin as RawStdin, ChildStdout as RawStdout, Stdio,
};

use seam_core::ops::io::{Flush, Read, Write};
use seam_core::ops::process::{
    ChildId, ChildPipes, CloseStdin, Exit, ExitStatus, Kill, Output, Pipe, ReadErr, RunCommand,
    SpawnChild, SplitChild, Wait,
};
use seam_core::{Closing, Inspect, Opening, Perform};

use crate::Std;

impl Perform<RunCommand> for Std {
    fn perform(&mut self, mut op: RunCommand) -> seam_core::io::Result<Output> {
        crate::sio(op.0.output().map(|o| Output {
            code: o.status.code(),
            stdout: o.stdout,
            stderr: o.stderr,
        }))
    }
}

impl Perform<Exit> for Std {
    fn perform(&mut self, op: Exit) -> Infallible {
        std::process::exit(op.0)
    }
}

/// A running child process, acquired with [`SpawnChild`]. Writing goes to
/// its standard input and reading comes from its standard output;
/// [`ReadErr`] reads its standard error.
///
/// When its scope succeeds, its pipes are closed and the scope waits for it
/// to exit. When the scope fails or panics, it is killed, and then waited
/// for so that it leaves no zombie.
#[derive(Debug)]
pub struct Child {
    inner: std::process::Child,
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
pub struct ChildStdout(Option<RawStdout>);

/// A [`SplitChild`]'s pipe from the child's standard error.
#[derive(Debug)]
pub struct ChildStderr(Option<RawStderr>);

fn stdio(pipe: Pipe) -> Stdio {
    match pipe {
        Pipe::Piped => Stdio::piped(),
        Pipe::Null => Stdio::null(),
        Pipe::Inherit => Stdio::inherit(),
    }
}

/// No allocation: a kind and a static message.
fn not_piped(message: &'static str) -> seam_core::io::Error {
    seam_core::io::Error::new(seam_core::io::ErrorKind::BrokenPipe, message)
}

fn stdin(pipe: &mut Option<RawStdin>) -> seam_core::io::Result<&mut RawStdin> {
    pipe.as_mut().ok_or_else(|| {
        not_piped("the child's standard input is not piped to this scope, or is closed")
    })
}

fn stdout(pipe: &mut Option<RawStdout>) -> seam_core::io::Result<&mut RawStdout> {
    pipe.as_mut().ok_or_else(|| {
        not_piped("the child's standard output is not piped to this scope, or is split")
    })
}

fn stderr(pipe: &mut Option<RawStderr>) -> seam_core::io::Result<&mut RawStderr> {
    pipe.as_mut().ok_or_else(|| {
        not_piped("the child's standard error is not piped to this scope, or is split")
    })
}

fn status(s: std::process::ExitStatus) -> ExitStatus {
    ExitStatus { code: s.code() }
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

    fn wait(&mut self) -> seam_core::io::Result<ExitStatus> {
        self.stdin = None;
        if let Some(s) = self.status {
            return Ok(s);
        }
        let s = status(self.inner.wait()?);
        self.status = Some(s);
        Ok(s)
    }

    /// Commits: closes the pipes, so a child blocked writing to one cannot
    /// hang the wait, then waits for it.
    fn finish(mut self) -> seam_core::io::Result<()> {
        self.stdout = None;
        self.stderr = None;
        self.wait().map(drop)
    }
}

/// Kills the child unless it was waited for, then reaps it.
impl Drop for Child {
    fn drop(&mut self) {
        if self.status.is_none() {
            let _ = self.inner.kill();
            let _ = self.inner.wait();
        }
    }
}

impl Perform<Opening<SpawnChild<Child>>> for Std {
    fn perform(
        &mut self,
        Opening(op, _): Opening<SpawnChild<Child>>,
    ) -> seam_core::io::Result<Child> {
        let mut cmd = op.command;
        cmd.stdin(stdio(op.stdin))
            .stdout(stdio(op.stdout))
            .stderr(stdio(op.stderr));
        let mut inner = cmd.spawn()?;
        Ok(Child {
            stdin: inner.stdin.take(),
            stdout: inner.stdout.take(),
            stderr: inner.stderr.take(),
            inner,
            status: None,
        })
    }
}

impl Perform<Closing<SpawnChild<Child>>> for Std {
    fn perform(
        &mut self,
        Closing(child, _): Closing<SpawnChild<Child>>,
    ) -> seam_core::io::Result<()> {
        child.finish()
    }
}

/// Takes the pipes out of the child; closing the scope puts them back.
impl Perform<Opening<SplitChild<ChildStdin, ChildStdout, ChildStderr>>> for Child {
    fn perform(
        &mut self,
        Opening(_, _): Opening<SplitChild<ChildStdin, ChildStdout, ChildStderr>>,
    ) -> seam_core::io::Result<ChildPipes<ChildStdin, ChildStdout, ChildStderr>> {
        Ok(ChildPipes {
            stdin: ChildStdin(self.stdin.take()),
            stdout: ChildStdout(self.stdout.take()),
            stderr: ChildStderr(self.stderr.take()),
        })
    }
}

impl Perform<Closing<SplitChild<ChildStdin, ChildStdout, ChildStderr>>> for Child {
    fn perform(
        &mut self,
        Closing(pipes, _): Closing<SplitChild<ChildStdin, ChildStdout, ChildStderr>>,
    ) -> seam_core::io::Result<()> {
        // A child already waited for has had its input closed.
        if self.status.is_none() {
            self.stdin = pipes.stdin.0;
        }
        self.stdout = pipes.stdout.0;
        self.stderr = pipes.stderr.0;
        Ok(())
    }
}

impl Perform<Write<'_>> for ChildStdin {
    fn perform(&mut self, op: Write<'_>) -> seam_core::io::Result<usize> {
        crate::sio(stdin(&mut self.0)?.write(op.0))
    }
}

impl Perform<Flush> for ChildStdin {
    fn perform(&mut self, _: Flush) -> seam_core::io::Result<()> {
        crate::sio(stdin(&mut self.0)?.flush())
    }
}

impl Perform<CloseStdin> for ChildStdin {
    fn perform(&mut self, _: CloseStdin) {
        self.0 = None;
    }
}

impl Perform<Read<'_>> for ChildStdout {
    fn perform(&mut self, op: Read<'_>) -> seam_core::io::Result<usize> {
        crate::sio(stdout(&mut self.0)?.read(op.0))
    }
}

impl Perform<Read<'_>> for ChildStderr {
    fn perform(&mut self, op: Read<'_>) -> seam_core::io::Result<usize> {
        crate::sio(stderr(&mut self.0)?.read(op.0))
    }
}

impl Perform<Write<'_>> for Child {
    fn perform(&mut self, op: Write<'_>) -> seam_core::io::Result<usize> {
        crate::sio(self.stdin()?.write(op.0))
    }
}

impl Perform<Flush> for Child {
    fn perform(&mut self, _: Flush) -> seam_core::io::Result<()> {
        crate::sio(self.stdin()?.flush())
    }
}

impl Perform<Read<'_>> for Child {
    fn perform(&mut self, op: Read<'_>) -> seam_core::io::Result<usize> {
        crate::sio(self.stdout()?.read(op.0))
    }
}

impl Perform<ReadErr<'_>> for Child {
    fn perform(&mut self, op: ReadErr<'_>) -> seam_core::io::Result<usize> {
        crate::sio(self.stderr()?.read(op.0))
    }
}

impl Perform<CloseStdin> for Child {
    fn perform(&mut self, _: CloseStdin) {
        self.stdin = None;
    }
}

impl Perform<Wait> for Child {
    fn perform(&mut self, _: Wait) -> seam_core::io::Result<ExitStatus> {
        self.wait()
    }
}

impl Perform<Kill> for Child {
    fn perform(&mut self, _: Kill) -> seam_core::io::Result<()> {
        if self.status.is_some() {
            return Ok(());
        }
        crate::sio(self.inner.kill())
    }
}

/// `None` once the child has been waited for.
impl Inspect<ChildId> for Child {
    fn inspect(&self, _: ChildId) -> Option<u32> {
        self.status.is_none().then(|| self.inner.id())
    }
}

/// std cannot wait for a child with a timeout, so a timed wait polls,
/// sleeping up to 10ms between tries.
#[cfg(feature = "time")]
impl Perform<seam_core::ops::time::Timeout<Wait>> for Child {
    fn perform(
        &mut self,
        op: seam_core::ops::time::Timeout<Wait>,
    ) -> seam_core::io::Result<ExitStatus> {
        use seam_core::ops::time::TimesOut;
        use std::time::{Duration, Instant};

        self.stdin = None;
        if let Some(s) = self.status {
            return Ok(s);
        }
        let deadline = Instant::now().checked_add(op.after);
        let mut nap = Duration::from_micros(500);
        loop {
            if let Some(s) = self.inner.try_wait()? {
                let s = status(s);
                self.status = Some(s);
                return Ok(s);
            }
            let left = deadline.map(|d| d.saturating_duration_since(Instant::now()));
            if left.is_some_and(|l| l.is_zero()) {
                return op.op.timed_out();
            }
            std::thread::sleep(left.map_or(nap, |l| l.min(nap)));
            nap = (nap * 2).min(Duration::from_millis(10));
        }
    }
}

// Every capability type opts in to seam's lifts.
impl seam_core::Capability for Child {}
impl seam_core::Capability for ChildStdin {}
impl seam_core::Capability for ChildStdout {}
impl seam_core::Capability for ChildStderr {}
