//! Child processes and exiting.
//!
//! Building a `std::process::Command` is pure; running it is the effect.
//! [`RunCommand`] runs one to completion. [`SpawnChild`] starts one and
//! lends it to a scope, to talk to while it runs. [`Exit`]'s outcome is
//! `Infallible`, so the compiler knows it never returns.
//!
//! # Talking to a child
//!
//! A child acts like a stream: `Write` goes to its standard input and
//! `Read` comes from its standard output, so the [`io`](super::io) helpers
//! work on it ([`write_all`](super::io::write_all),
//! [`read_to_end`](super::io::read_to_end), ...). [`ReadErr`] reads its
//! standard error ([`read_err_to_end`] all of it), [`CloseStdin`] sends end
//! of input, [`Wait`] waits for it to exit, and [`Kill`] stops it.
//!
//! [`RunCommand`] is a primitive, not a scope built from these: collecting
//! both outputs means reading the two pipes at once, or the child can fill
//! one while the core waits on the other, and a core cannot read two at
//! once.
//!
//! On top of [`RunCommand`], [`run_checked`] fails when the command does,
//! and [`output_string`] returns what it printed, as text.
//!
//! [`SplitChild`] lends the three pipes separately, to use at once.
//!
//! When the scope succeeds, the child's pipes are closed and the scope
//! waits for it to exit, so no child outlives its scope. When the scope
//! fails, panics or is cancelled, the child is killed.
//!
//! ```
//! use seam_core::io;
//! use std::process::Command;
//! use seam_core::{Perform, Provide};
//! use seam_core::ops::io::{Read, Write, read_to_end, write_all};
//! use seam_core::ops::process::{CloseStdin, SpawnChild, Wait};
//!
//! /// Feeds `input` through a filter program and returns what it printed.
//! fn filter<C, P>(c: &mut C, program: &str, input: &[u8]) -> io::Result<Vec<u8>>
//! where
//!     C: Provide<SpawnChild<P>>,
//!     P: for<'a> Perform<Write<'a>> + for<'a> Perform<Read<'a>>
//!         + Perform<CloseStdin> + Perform<Wait>,
//! {
//!     c.acquire(SpawnChild::new(Command::new(program)), |_, child| {
//!         write_all(child, input)?;
//!         child.perform(CloseStdin);
//!         let mut out = Vec::new();
//!         read_to_end(child, &mut out)?;
//!         let status = child.perform(Wait)?;
//!         if !status.success() {
//!             return Err(io::Error::other("the filter failed"));
//!         }
//!         Ok(out)
//!     })
//! }
//! ```
//!
//! ```
//! use seam_core::io;
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

use crate::io;
use crate::three_ways::three_ways;
use core::convert::Infallible;
use core::marker::PhantomData;
use std::process::Command;
use std::string::String;
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
/// be constructed portably, so that test handlers can produce one.
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

/// Split a child into its three pipes, lent together to a scope, so they
/// can be used at once: feed its input while draining both outputs, from
/// futures joined in one task or from scoped threads. Acquired from the
/// child, inside its own scope; the scope's body still has the child, to
/// [`Wait`] for it or [`Kill`] it.
///
/// A pipe that is not [`Pipe::Piped`] is still lent, and fails as the child
/// would: writes and reads with `io::ErrorKind::BrokenPipe`. Closing the
/// scope gives the pipes back to the child; a failed scope drops them,
/// which closes them.
pub struct SplitChild<I, O, E> {
    pipes: PhantomData<MakesPipes<I, O, E>>,
}

/// What a [`SplitChild`] lends, as a function type: `Send`, `Sync` and
/// covariant whatever the pipes are.
type MakesPipes<I, O, E> = fn() -> ChildPipes<I, O, E>;

/// The pipes of a [`SplitChild`].
#[derive(Debug)]
pub struct ChildPipes<I, O, E> {
    /// Writes to the child's standard input; [`CloseStdin`] on it ends
    /// the input.
    pub stdin: I,
    /// Reads the child's standard output.
    pub stdout: O,
    /// Reads the child's standard error, with `Read` (not [`ReadErr`]).
    pub stderr: E,
}

impl<I, O, E> SplitChild<I, O, E> {
    /// Describes splitting a child.
    pub const fn new() -> Self {
        Self { pipes: PhantomData }
    }
}

impl<I, O, E> Default for SplitChild<I, O, E> {
    fn default() -> Self {
        Self::new()
    }
}

impl<I, O, E> Clone for SplitChild<I, O, E> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<I, O, E> Copy for SplitChild<I, O, E> {}

impl<I, O, E> core::fmt::Debug for SplitChild<I, O, E> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("SplitChild")
    }
}

impl<I, O, E> crate::Acquisition for SplitChild<I, O, E> {
    type Resource = ChildPipes<I, O, E>;
    type Error = io::Error;
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

operations! {
    RunCommand => io::Result<Output>;
    Exit => Infallible;
    Wait => io::Result<ExitStatus>;
    Kill => io::Result<()>;
    #[query] ChildId => Option<u32>;
    CloseStdin => ();
    ReadErr<'_> => io::Result<usize>;
}

impl<C> crate::Acquisition for SpawnChild<C> {
    type Resource = C;
    type Error = io::Error;
}

crate::root::keys!(
    [] RunCommand => RunCommand,
    [] Exit => Exit,
    [C] SpawnChild<C> => SpawnChild,
);

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
}

three_ways! {
    /// Reads a child's standard error until it closes it, appending to
    /// `out`. Returns how many bytes were read.
    pub fn read_err_to_end, read_err_to_end_async, read_err_to_end_send
    [H] (child: &mut H, out: &mut Vec<u8>) -> io::Result<usize>
    where
        sync { H: for<'a> crate::Perform<ReadErr<'a>> }
        async { H: for<'a> crate::future::AsyncPerform<ReadErr<'a>> }
        send { H: for<'a> crate::future::SendPerform<ReadErr<'a>> + Send }
    {
        let start = out.len();
        let mut chunk = [0; 4096];
        loop {
            match run!(child, ReadErr(&mut chunk)) {
                Ok(0) => return Ok(out.len() - start),
                Ok(n) => out.extend_from_slice(&chunk[..n]),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
    }
}

three_ways! {
    /// Runs `command` to completion, like [`RunCommand`], but fails with
    /// `io::ErrorKind::Other` unless it exits with code 0. The error's
    /// message holds the exit code and the start of its standard error.
    pub fn run_checked, run_checked_async, run_checked_send
    [C] (c: &mut C, command: Command) -> io::Result<Output>
    where
        sync { C: crate::Perform<RunCommand> }
        async { C: crate::future::AsyncPerform<RunCommand> }
        send { C: crate::future::SendPerform<RunCommand> + Send }
    {
        let output = run!(c, RunCommand(command))?;
        if output.success() {
            return Ok(output);
        }
        Err(failed(&output))
    }

    /// Runs `command` with [`run_checked`] and returns its standard output
    /// as text, with trailing whitespace (the final newline) removed. Fails
    /// with `io::ErrorKind::InvalidData` if the output is not UTF-8.
    pub fn output_string, output_string_async, output_string_send
    [C] (c: &mut C, command: Command) -> io::Result<String>
    where
        sync { C: crate::Perform<RunCommand> }
        async { C: crate::future::AsyncPerform<RunCommand> }
        send { C: crate::future::SendPerform<RunCommand> + Send }
    {
        let output = call!(run_checked, run_checked_async, run_checked_send; (c, command))?;
        let mut text = String::from_utf8(output.stdout).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidData, "the command's output is not valid UTF-8")
        })?;
        text.truncate(text.trim_end().len());
        Ok(text)
    }
}

/// The error for a command that did not succeed.
fn failed(output: &Output) -> io::Error {
    use core::fmt::Write as _;
    let mut message = String::new();
    match output.code {
        Some(code) => {
            let _ = write!(message, "the command exited with code {code}");
        }
        None => message.push_str("the command was killed by a signal"),
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stderr = stderr.trim();
    if !stderr.is_empty() {
        let end = stderr
            .char_indices()
            .nth(200)
            .map_or(stderr.len(), |(i, _)| i);
        let _ = write!(message, ": {}", &stderr[..end]);
    }
    // seam's own errors hold only static messages; this one is built at
    // run time, so it travels as a `std::io::Error`.
    io::Error::from(std::io::Error::other(message))
}
