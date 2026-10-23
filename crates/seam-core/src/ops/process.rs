//! Child processes and exiting.
//!
//! Building a `std::process::Command` is pure; running it is the effect.
//! [`RunCommand`] runs one to completion. [`SpawnChild`] starts one and
//! lends it to a scope, to talk to while it runs. [`Exit`]'s outcome is
//! `Infallible`, so the compiler knows it never returns.
//!
//! # Talking to a child
//!
//! A child acts like a stream: writing (`Write`, `WriteAll`) goes to its
//! standard input, and reading (`Read`, `ReadToEnd`) comes from its
//! standard output. [`ReadErr`] and [`ReadErrToEnd`] read its standard
//! error, [`CloseStdin`] sends end of input, [`Wait`] waits for it to exit,
//! and [`Kill`] stops it.
//!
//! When the scope succeeds, the child's pipes are closed and the scope
//! waits for it to exit, so no child outlives its scope. When the scope
//! fails, panics or is cancelled, the child is killed.
//!
//! ```
//! use std::io;
//! use std::process::Command;
//! use seam_core::{Acquire, Perform, Provide};
//! use seam_core::ops::io::{ReadToEnd, WriteAll};
//! use seam_core::ops::process::{CloseStdin, SpawnChild, Wait};
//!
//! /// Feeds `input` through a filter program and returns what it printed.
//! fn filter<C, P>(c: &mut C, program: &str, input: &[u8]) -> io::Result<Vec<u8>>
//! where
//!     C: Provide<SpawnChild<P>>,
//!     P: for<'a> Perform<WriteAll<'a>> + for<'a> Perform<ReadToEnd<'a>>
//!         + Perform<CloseStdin> + Perform<Wait>,
//! {
//!     c.acquire(SpawnChild::new(Command::new(program)), |_, child| {
//!         child.perform(WriteAll(input))?;
//!         child.perform(CloseStdin);
//!         let mut out = Vec::new();
//!         child.perform(ReadToEnd(&mut out))?;
//!         let status = child.perform(Wait)?;
//!         if !status.success() {
//!             return Err(io::Error::other(format!("{program} failed: {status:?}")));
//!         }
//!         Ok(out)
//!     })
//! }
//! ```
//!
//! ```
//! use std::io;
//! use std::process::Command;
//! use seam_core::Perform;
//! use seam_core::ops::process::{Exit, RunCommand};
//!
//! /// The current git commit, or exit with status 2.
//! fn head<C: Perform<RunCommand> + Perform<Exit>>(c: &mut C) -> String {
//!     let mut cmd = Command::new("git");
//!     cmd.args(["rev-parse", "HEAD"]);
//!     match c.perform(RunCommand(cmd)) {
//!         Ok(out) if out.success() => String::from_utf8_lossy(&out.stdout).trim().to_owned(),
//!         _ => match c.perform(Exit(2)) {},
//!     }
//! }
//! ```

use core::convert::Infallible;
use core::marker::PhantomData;
use std::io;
use std::process::Command;
use std::vec::Vec;

/// Run a command to completion and collect its [`Output`].
#[derive(Debug)]
pub struct RunCommand(pub Command);

/// End the current process with an exit code. Never returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Exit(pub i32);

/// What a finished command produced.
///
/// A seam type rather than `std::process::Output`, whose `ExitStatus` cannot
/// be constructed portably, so that test backends can produce one.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Output {
    /// The exit code, or `None` if the process was killed by a signal.
    pub code: Option<i32>,
    /// Everything written to standard output.
    pub stdout: Vec<u8>,
    /// Everything written to standard error.
    pub stderr: Vec<u8>,
}

impl Output {
    /// Whether the process exited with code 0.
    pub fn success(&self) -> bool {
        self.code == Some(0)
    }
}

/// Start a command and lend it, as a child handle of type `C`, to a scope.
///
/// By default its standard input and output are piped, so the scope can
/// write to it and read from it, and its standard error is inherited.
/// Change that with [`stdin`](Self::stdin), [`stdout`](Self::stdout) and
/// [`stderr`](Self::stderr).
#[derive(Debug)]
pub struct SpawnChild<C> {
    /// What to run.
    pub command: Command,
    /// Where its standard input comes from.
    pub stdin: Pipe,
    /// Where its standard output goes.
    pub stdout: Pipe,
    /// Where its standard error goes.
    pub stderr: Pipe,
    handle: PhantomData<fn() -> C>,
}

impl<C> SpawnChild<C> {
    /// Describes starting `command`, with standard input and output piped
    /// and standard error inherited.
    pub fn new(command: Command) -> Self {
        Self {
            command,
            stdin: Pipe::Piped,
            stdout: Pipe::Piped,
            stderr: Pipe::Inherit,
            handle: PhantomData,
        }
    }

    /// Sets where standard input comes from.
    #[must_use]
    pub fn stdin(mut self, pipe: Pipe) -> Self {
        self.stdin = pipe;
        self
    }

    /// Sets where standard output goes.
    #[must_use]
    pub fn stdout(mut self, pipe: Pipe) -> Self {
        self.stdout = pipe;
        self
    }

    /// Sets where standard error goes.
    #[must_use]
    pub fn stderr(mut self, pipe: Pipe) -> Self {
        self.stderr = pipe;
        self
    }
}

/// Where one of a child's standard streams is connected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Pipe {
    /// A pipe to the scope: write to or read from the child.
    Piped,
    /// Nothing: end of input, or output discarded.
    Null,
    /// The parent's own stream.
    Inherit,
}

/// How a child ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct ExitStatus {
    /// The exit code, or `None` if the process was killed by a signal.
    pub code: Option<i32>,
}

impl ExitStatus {
    /// Whether the process exited with code 0.
    pub fn success(&self) -> bool {
        self.code == Some(0)
    }
}

/// Wait for a child to exit. Performed on the child. Closes its standard
/// input first, so a child reading it sees the end.
#[derive(Debug, Clone, Copy, Default)]
pub struct Wait;

/// Stop a child at once. Performed on the child; [`Wait`] then reports how
/// it ended. Killing a child that has already exited does nothing.
#[derive(Debug, Clone, Copy, Default)]
pub struct Kill;

/// The child's process id, if it is still known. Performed on the child.
#[derive(Debug, Clone, Copy, Default)]
pub struct ChildId;

/// Close a child's standard input, so it sees the end of its input.
/// Performed on the child.
#[derive(Debug, Clone, Copy, Default)]
pub struct CloseStdin;

/// Read some of a child's standard error. Performed on the child; `Read`
/// reads its standard output.
#[derive(Debug)]
pub struct ReadErr<'a>(pub &'a mut [u8]);

/// Read a child's standard error until it closes it, appending to the
/// buffer. Performed on the child.
#[derive(Debug)]
pub struct ReadErrToEnd<'a>(pub &'a mut Vec<u8>);

operations! {
    RunCommand => io::Result<Output>;
    Exit => Infallible;
    Wait => io::Result<ExitStatus>;
    Kill => io::Result<()>;
    #[query] ChildId => Option<u32>;
    CloseStdin => ();
    ReadErr<'_> => io::Result<usize>;
    ReadErrToEnd<'_> => io::Result<usize>;
}

impl<C> crate::Acquisition for SpawnChild<C> {
    type Resource = C;
    type Error = io::Error;
}

crate::root::in_kind!(crate::kinds::Process: [] RunCommand, [] Exit, [C] SpawnChild<C>);

/// Each gives up with `io::ErrorKind::TimedOut`.
#[cfg(feature = "time")]
mod timeouts {
    use super::*;
    use crate::ops::time::{TimesOut, io_timed_out};

    /// The child keeps running.
    impl TimesOut for Wait {
        type Expired = io::Result<ExitStatus>;
        fn timed_out(self) -> Self::Expired {
            Err(io_timed_out())
        }
    }

    impl TimesOut for ReadErr<'_> {
        type Expired = io::Result<usize>;
        fn timed_out(self) -> Self::Expired {
            Err(io_timed_out())
        }
    }

    /// What arrived before the deadline is left in the buffer.
    impl TimesOut for ReadErrToEnd<'_> {
        type Expired = io::Result<usize>;
        fn timed_out(self) -> Self::Expired {
            Err(io_timed_out())
        }
    }
}
