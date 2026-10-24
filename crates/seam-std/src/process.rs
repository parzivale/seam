//! Running commands, child processes, and exiting.

use std::convert::Infallible;
use std::io::{Read as _, Write as _};
use std::process::{ChildStderr, ChildStdin, ChildStdout, Stdio};

use seam_core::ops::io::{Flush, Read, Write};
use seam_core::ops::process::{
    ChildId, CloseStdin, Exit, ExitStatus, Kill, Output, Pipe, ReadErr, RunCommand, SpawnChild,
    Wait,
};
use seam_core::{Inspect, Perform, Provide, Token};

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

/// No allocation: a kind and a static message.
fn not_piped(message: &'static str) -> seam_core::io::Error {
    seam_core::io::Error::new(seam_core::io::ErrorKind::BrokenPipe, message)
}

fn status(s: std::process::ExitStatus) -> ExitStatus {
    ExitStatus { code: s.code() }
}

impl Child {
    fn stdin(&mut self) -> seam_core::io::Result<&mut ChildStdin> {
        self.stdin.as_mut().ok_or_else(|| {
            not_piped("the child's standard input is not piped to this scope, or is closed")
        })
    }

    fn stdout(&mut self) -> seam_core::io::Result<&mut ChildStdout> {
        self.stdout
            .as_mut()
            .ok_or_else(|| not_piped("the child's standard output is not piped to this scope"))
    }

    fn stderr(&mut self) -> seam_core::io::Result<&mut ChildStderr> {
        self.stderr
            .as_mut()
            .ok_or_else(|| not_piped("the child's standard error is not piped to this scope"))
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

impl Provide<SpawnChild<Child>> for Std {
    fn provide(&mut self, op: SpawnChild<Child>, _: Token) -> seam_core::io::Result<Child> {
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

    fn close(&mut self, child: Child) -> seam_core::io::Result<()> {
        child.finish()
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
