//! Scripted commands, child processes, and exiting.
//!
//! Commands are scripted per program with [`Sim::on_command`] (or
//! [`Sim::on_command_with_stdin`]): a handler sees the command, and the
//! input written to it, and returns what the program printed and its exit
//! code. Both [`RunCommand`] and [`SpawnChild`] use them.
//!
//! A spawned [`Child`] is a filter. Its handler runs the first time the
//! core reads its output, waits for it, or closes its scope, and sees the
//! standard input written up to then; writing after that fails with
//! `BrokenPipe`. A child with no piped standard input runs at once. Output
//! it inherits goes to the `Sim`'s own standard output or error when it
//! runs at spawn, and otherwise when its scope closes; nothing is printed
//! for a child that is killed or abandoned. Nothing in a `Sim` waits, so
//! timed operations on a child behave like untimed ones.
#![allow(clippy::disallowed_types)] // the scripts and log are shared with child handles

use std::boxed::Box;
use std::collections::VecDeque;
use std::convert::Infallible;
use std::ffi::OsString;
use std::format;
use std::io::{self, ErrorKind};
use std::process::Command;
use std::string::{String, ToString};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::vec::Vec;

use seam_core::ops::io::{Flush, Read, ReadToEnd, Write, WriteAll};
use seam_core::ops::process::{
    ChildId, CloseStdin, Exit, ExitStatus, Kill, Output, Pipe, ReadErr, ReadErrToEnd, RunCommand,
    SpawnChild, Wait,
};
use seam_core::{Inspect, Perform, Provide, Token};

use crate::{Sim, error};

type Handler = Box<dyn FnMut(&Command, &[u8]) -> io::Result<Output> + Send + Sync>;

/// The scripts and the log, shared with the children they run.
#[derive(Default)]
struct Inner {
    handlers: Vec<(OsString, Handler)>,
    log: Vec<Invocation>,
}

#[derive(Default, Clone)]
pub(crate) struct Processes {
    inner: Arc<Mutex<Inner>>,
    /// Fixed responses from world files, shared with their handlers.
    rules: Rules,
}

impl Processes {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// A fixed response to a program, with its arguments if they must match.
struct Rule {
    program: OsString,
    args: Option<Vec<OsString>>,
    output: Output,
}

type Rules = Arc<Mutex<Vec<Rule>>>;

/// A command the core ran or spawned, recorded for assertions.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Invocation {
    /// The program.
    pub program: OsString,
    /// Its arguments, without the program name.
    pub args: Vec<OsString>,
    /// What was written to its standard input: empty for `RunCommand`, and
    /// for a spawned child, what it was given before it ran.
    pub stdin: Vec<u8>,
}

/// The panic payload a simulated [`Exit`] unwinds with. Use [`catch_exit`]
/// to turn it back into an exit code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Exited(pub i32);

impl Sim {
    /// Scripts what running `program` does. The handler sees the full
    /// command and returns its output. A program with no handler fails to
    /// start with `NotFound`. A later handler for the same program replaces
    /// the earlier one.
    pub fn on_command(
        &mut self,
        program: impl Into<OsString>,
        mut handler: impl FnMut(&Command) -> io::Result<Output> + Send + Sync + 'static,
    ) -> &mut Self {
        self.on_command_with_stdin(program, move |cmd, _| handler(cmd))
    }

    /// Like [`on_command`](Self::on_command), with the handler also seeing
    /// what was written to the program's standard input: for a spawned
    /// child, everything written before it ran; for `RunCommand`, nothing.
    ///
    /// ```
    /// # #[cfg(feature = "process")] {
    /// use std::process::Command;
    /// use seam_core::ops::io::{ReadToEnd, WriteAll};
    /// use seam_core::ops::process::{CloseStdin, Output, SpawnChild};
    /// use seam_core::{Acquire, Perform};
    /// use seam_test::Sim;
    /// use seam_test::process::Child;
    ///
    /// let mut sim = Sim::new();
    /// sim.on_command_with_stdin("tr", |_, input| {
    ///     Ok(Output { code: Some(0), stdout: input.to_ascii_uppercase(), ..Output::default() })
    /// });
    /// let shouted = sim.acquire(SpawnChild::<Child>::new(Command::new("tr")), |_, child| {
    ///     child.perform(WriteAll(b"quiet"))?;
    ///     child.perform(CloseStdin);
    ///     let mut out = Vec::new();
    ///     child.perform(ReadToEnd(&mut out))?;
    ///     std::io::Result::Ok(out)
    /// }).unwrap();
    /// assert_eq!(shouted, b"QUIET");
    /// assert_eq!(sim.invocations()[0].stdin, b"quiet");
    /// # }
    /// ```
    pub fn on_command_with_stdin(
        &mut self,
        program: impl Into<OsString>,
        handler: impl FnMut(&Command, &[u8]) -> io::Result<Output> + Send + Sync + 'static,
    ) -> &mut Self {
        let program = program.into();
        let mut inner = self.process.lock();
        inner.handlers.retain(|(p, _)| *p != program);
        inner.handlers.push((program, Box::new(handler)));
        drop(inner);
        self
    }

    /// Scripts a fixed response: running `program`, with exactly `args` if
    /// given and with any arguments otherwise, produces `output`. Later
    /// responses win, and one for exact arguments wins over one for any.
    /// Replaces a handler from [`on_command`](Self::on_command) for the
    /// same program.
    pub(crate) fn script_command(
        &mut self,
        program: OsString,
        args: Option<Vec<OsString>>,
        output: Output,
    ) {
        let rules = Arc::clone(&self.process.rules);
        rules
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(Rule {
                program: program.clone(),
                args,
                output,
            });
        self.on_command(program, move |cmd: &Command| {
            let rules = rules.lock().unwrap_or_else(PoisonError::into_inner);
            let args: Vec<OsString> = cmd.get_args().map(|a| a.to_os_string()).collect();
            let for_program = || {
                rules
                    .iter()
                    .rev()
                    .filter(|r| r.program == cmd.get_program())
            };
            for_program()
                .find(|r| r.args.as_ref() == Some(&args))
                .or_else(|| for_program().find(|r| r.args.is_none()))
                .map(|r| r.output.clone())
                .ok_or_else(|| {
                    error(
                        ErrorKind::NotFound,
                        "the world file scripts this program, but not with these arguments",
                    )
                })
        });
    }

    /// Every command the core has run or spawned, in order.
    pub fn invocations(&self) -> Vec<Invocation> {
        self.process.lock().log.clone()
    }
}

/// Records `cmd` and returns its index in the log, failing with `NotFound`
/// if no handler is scripted for it.
fn record(inner: &mut Inner, cmd: &Command) -> io::Result<usize> {
    inner.log.push(Invocation {
        program: cmd.get_program().to_os_string(),
        args: cmd.get_args().map(|a| a.to_os_string()).collect(),
        stdin: Vec::new(),
    });
    if !inner.handlers.iter().any(|(p, _)| p == cmd.get_program()) {
        return Err(error(
            ErrorKind::NotFound,
            "no handler scripted for this program",
        ));
    }
    Ok(inner.log.len() - 1)
}

fn run(inner: &mut Inner, cmd: &Command, stdin: &[u8]) -> io::Result<Output> {
    let (_, handler) = inner
        .handlers
        .iter_mut()
        .find(|(p, _)| p == cmd.get_program())
        .ok_or_else(|| error(ErrorKind::NotFound, "no handler scripted for this program"))?;
    handler(cmd, stdin)
}

impl Perform<RunCommand> for Sim {
    fn perform(&mut self, op: RunCommand) -> io::Result<Output> {
        let cmd = op.0;
        let mut inner = self.process.lock();
        record(&mut inner, &cmd)?;
        run(&mut inner, &cmd, &[])
    }
}

/// A simulated child process, acquired with [`SpawnChild`]: a filter whose
/// scripted handler runs when its output is first needed. See the
/// [module docs](self).
pub struct Child {
    processes: Processes,
    entry: usize,
    command: Command,
    pipes: [Pipe; 3],
    stdin: Vec<u8>,
    stdin_open: bool,
    ran: Option<Ran>,
    waited: bool,
}

/// What a child produced, once it ran.
struct Ran {
    stdout: VecDeque<u8>,
    stderr: VecDeque<u8>,
    /// Output for the `Sim`'s own streams, not yet delivered.
    inherited: (Vec<u8>, Vec<u8>),
    code: Option<i32>,
    /// Why the handler failed, if it did.
    failed: Option<(ErrorKind, String)>,
}

impl core::fmt::Debug for Child {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Child")
            .field("command", &self.command)
            .field("ran", &self.ran.is_some())
            .finish_non_exhaustive()
    }
}

const STDIN: usize = 0;
const STDOUT: usize = 1;
const STDERR: usize = 2;

fn not_piped(stream: &str) -> io::Error {
    io::Error::new(
        ErrorKind::BrokenPipe,
        format!("the child's {stream} is not piped to this scope, or is closed"),
    )
}

impl Child {
    /// Runs the handler, if it has not run: on the input so far.
    fn run(&mut self) -> &mut Ran {
        if self.ran.is_none() {
            self.stdin_open = false;
            let mut inner = self.processes.lock();
            if let Some(entry) = inner.log.get_mut(self.entry) {
                entry.stdin = self.stdin.clone();
            }
            let result = run(&mut inner, &self.command, &self.stdin);
            drop(inner);
            let ran = match result {
                Ok(out) => {
                    let route = |pipe: Pipe, bytes: Vec<u8>| match pipe {
                        Pipe::Piped => (VecDeque::from(bytes), Vec::new()),
                        Pipe::Inherit => (VecDeque::new(), bytes),
                        Pipe::Null => (VecDeque::new(), Vec::new()),
                    };
                    let (stdout, inherited_out) = route(self.pipes[STDOUT], out.stdout);
                    let (stderr, inherited_err) = route(self.pipes[STDERR], out.stderr);
                    Ran {
                        stdout,
                        stderr,
                        inherited: (inherited_out, inherited_err),
                        code: out.code,
                        failed: None,
                    }
                }
                Err(e) => Ran {
                    stdout: VecDeque::new(),
                    stderr: VecDeque::new(),
                    inherited: (Vec::new(), Vec::new()),
                    code: None,
                    failed: Some((e.kind(), e.to_string())),
                },
            };
            self.ran = Some(ran);
        }
        self.ran.as_mut().expect("it has run")
    }

    fn output(&mut self, stream: usize) -> io::Result<&mut VecDeque<u8>> {
        let name = if stream == STDOUT {
            "standard output"
        } else {
            "standard error"
        };
        if self.pipes[stream] != Pipe::Piped {
            return Err(not_piped(name));
        }
        let ran = self.run();
        if let Some((kind, msg)) = &ran.failed {
            return Err(io::Error::new(*kind, msg.clone()));
        }
        Ok(if stream == STDOUT {
            &mut ran.stdout
        } else {
            &mut ran.stderr
        })
    }

    fn read(&mut self, stream: usize, buf: &mut [u8]) -> io::Result<usize> {
        let out = self.output(stream)?;
        let n = buf.len().min(out.len());
        for (dst, src) in buf.iter_mut().zip(out.drain(..n)) {
            *dst = src;
        }
        Ok(n)
    }

    fn read_to_end(&mut self, stream: usize, buf: &mut Vec<u8>) -> io::Result<usize> {
        let out = self.output(stream)?;
        let n = out.len();
        buf.extend(out.drain(..));
        Ok(n)
    }

    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.pipes[STDIN] != Pipe::Piped || !self.stdin_open {
            return Err(if self.ran.is_some() && self.pipes[STDIN] == Pipe::Piped {
                error(
                    ErrorKind::BrokenPipe,
                    "the simulated child has already run on the input it was given",
                )
            } else {
                not_piped("standard input")
            });
        }
        self.stdin.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn wait(&mut self) -> io::Result<ExitStatus> {
        let ran = self.run();
        if let Some((kind, msg)) = &ran.failed {
            return Err(io::Error::new(*kind, msg.clone()));
        }
        let code = ran.code;
        self.waited = true;
        Ok(ExitStatus { code })
    }

    /// Output the child inherited, for the `Sim` to print.
    fn take_inherited(&mut self) -> (Vec<u8>, Vec<u8>) {
        self.ran
            .as_mut()
            .map(|r| core::mem::take(&mut r.inherited))
            .unwrap_or_default()
    }
}

impl Sim {
    #[allow(unused_variables)]
    fn deliver(&mut self, (out, err): (Vec<u8>, Vec<u8>)) {
        #[cfg(feature = "io-std")]
        {
            let _ = self.perform(seam_core::ops::stdio::Print(&out));
            let _ = self.perform(seam_core::ops::stdio::EPrint(&err));
        }
    }
}

impl Provide<SpawnChild<Child>> for Sim {
    fn provide(&mut self, op: SpawnChild<Child>, _: Token) -> io::Result<Child> {
        let entry = record(&mut self.process.lock(), &op.command)?;
        let mut child = Child {
            processes: self.process.clone(),
            entry,
            command: op.command,
            pipes: [op.stdin, op.stdout, op.stderr],
            stdin: Vec::new(),
            stdin_open: op.stdin == Pipe::Piped,
            ran: None,
            waited: false,
        };
        if op.stdin != Pipe::Piped {
            // No input will come: it runs now.
            child.run();
            let inherited = child.take_inherited();
            self.deliver(inherited);
        }
        Ok(child)
    }

    /// Runs the child if it has not run, and prints what it inherited.
    fn close(&mut self, mut child: Child) -> io::Result<()> {
        child.run();
        let inherited = child.take_inherited();
        self.deliver(inherited);
        Ok(())
    }
}

impl Perform<Write<'_>> for Child {
    fn perform(&mut self, op: Write<'_>) -> io::Result<usize> {
        self.write(op.0)
    }
}

impl Perform<WriteAll<'_>> for Child {
    fn perform(&mut self, op: WriteAll<'_>) -> io::Result<()> {
        self.write(op.0).map(drop)
    }
}

impl Perform<Flush> for Child {
    fn perform(&mut self, _: Flush) -> io::Result<()> {
        Ok(())
    }
}

/// Runs the child, if it has not run, on the input so far.
impl Perform<Read<'_>> for Child {
    fn perform(&mut self, op: Read<'_>) -> io::Result<usize> {
        self.read(STDOUT, op.0)
    }
}

impl Perform<ReadToEnd<'_>> for Child {
    fn perform(&mut self, op: ReadToEnd<'_>) -> io::Result<usize> {
        self.read_to_end(STDOUT, op.0)
    }
}

impl Perform<ReadErr<'_>> for Child {
    fn perform(&mut self, op: ReadErr<'_>) -> io::Result<usize> {
        self.read(STDERR, op.0)
    }
}

impl Perform<ReadErrToEnd<'_>> for Child {
    fn perform(&mut self, op: ReadErrToEnd<'_>) -> io::Result<usize> {
        self.read_to_end(STDERR, op.0)
    }
}

impl Perform<CloseStdin> for Child {
    fn perform(&mut self, _: CloseStdin) {
        self.stdin_open = false;
    }
}

impl Perform<Wait> for Child {
    fn perform(&mut self, _: Wait) -> io::Result<ExitStatus> {
        self.wait()
    }
}

/// A child that has not run yet never will: it ends with no output, as if
/// killed by a signal. One that has run has already exited.
impl Perform<Kill> for Child {
    fn perform(&mut self, _: Kill) -> io::Result<()> {
        if self.ran.is_none() {
            self.stdin_open = false;
            if let Some(entry) = self.processes.lock().log.get_mut(self.entry) {
                entry.stdin = self.stdin.clone();
            }
            self.ran = Some(Ran {
                stdout: VecDeque::new(),
                stderr: VecDeque::new(),
                inherited: (Vec::new(), Vec::new()),
                code: None,
                failed: None,
            });
        }
        Ok(())
    }
}

/// A made-up process id, until the child has been waited for.
impl Inspect<ChildId> for Child {
    fn inspect(&self, _: ChildId) -> Option<u32> {
        (!self.waited).then(|| 1000 + u32::try_from(self.entry).unwrap_or(u32::MAX - 1000))
    }
}

/// Nothing in a `Sim` waits, so these behave as without a timeout.
#[cfg(feature = "time")]
mod timeouts {
    use seam_core::ops::time::Timeout;

    use super::*;

    macro_rules! untimed {
        ($($op:ty => $out:ty;)*) => {$(
            impl Perform<Timeout<$op>> for Child {
                fn perform(&mut self, op: Timeout<$op>) -> $out {
                    self.perform(op.op)
                }
            }
        )*};
    }

    untimed! {
        Wait => io::Result<ExitStatus>;
        Read<'_> => io::Result<usize>;
        ReadToEnd<'_> => io::Result<usize>;
        ReadErr<'_> => io::Result<usize>;
        ReadErrToEnd<'_> => io::Result<usize>;
    }
}

// Every capability type opts in to seam's lifts.
impl seam_core::Capability for Child {}

/// Unwinds with [`Exited`] instead of ending the process. The panic hook is
/// not run, so nothing is printed.
impl Perform<Exit> for Sim {
    fn perform(&mut self, op: Exit) -> Infallible {
        std::panic::resume_unwind(Box::new(Exited(op.0)))
    }
}

/// Runs `f`, catching a simulated [`Exit`] as `Err(code)`. Other panics
/// carry on unwinding.
pub fn catch_exit<R>(f: impl FnOnce() -> R) -> Result<R, i32> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).map_err(|payload| {
        match payload.downcast::<Exited>() {
            Ok(exited) => exited.0,
            Err(other) => std::panic::resume_unwind(other),
        }
    })
}
