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
#![allow(clippy::disallowed_types)] // the scripts, and each child's input, are shared with child handles

use seam_core::io::{self, ErrorKind};
use std::boxed::Box;
use std::collections::VecDeque;
use std::convert::Infallible;
use std::ffi::OsString;
use std::process::Command;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::vec::Vec;

use seam_core::ops::io::{Flush, Read, Write};
use seam_core::ops::process::{
    ChildId, CloseStdin, Exit, ExitStatus, Kill, Output, Pipe, ReadErr, RunCommand, SpawnChild,
    Wait,
};
use seam_core::{Closing, Inspect, Opening, Perform};

use crate::{Sim, error};

type Handler = Box<dyn FnMut(&Command, &[u8]) -> io::Result<Output> + Send + Sync>;

/// The scripts, shared with the children they run.
#[derive(Default, Clone)]
struct Handlers(Arc<Mutex<Vec<(OsString, Handler)>>>);

impl Handlers {
    fn lock(&self) -> MutexGuard<'_, Vec<(OsString, Handler)>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn has(&self, program: &std::ffi::OsStr) -> bool {
        self.lock().iter().any(|(p, _)| p == program)
    }

    fn run(&self, cmd: &Command, stdin: &[u8]) -> io::Result<Output> {
        let mut handlers = self.lock();
        let (_, handler) = handlers
            .iter_mut()
            .find(|(p, _)| p == cmd.get_program())
            .ok_or_else(|| error(ErrorKind::NotFound, "no handler scripted for this program"))?;
        handler(cmd, stdin)
    }
}

#[derive(Default)]
pub(crate) struct Processes {
    handlers: Handlers,
    log: Vec<Invocation>,
    /// Fixed responses from world files, shared with their handlers.
    rules: Rules,
}

impl core::fmt::Debug for Processes {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Processes")
            .field("log", &self.log)
            .finish_non_exhaustive()
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
#[derive(Debug, Clone)]
pub struct Invocation {
    /// The program.
    pub program: OsString,
    /// Its arguments, without the program name.
    pub args: Vec<OsString>,
    /// Filled once, by the child, when it runs or ends.
    stdin: Arc<OnceLock<Vec<u8>>>,
}

impl Invocation {
    /// What was written to its standard input: nothing for `RunCommand`;
    /// for a spawned child, what it was given before it ran, or before it
    /// was killed or dropped. Empty while the child has yet to run.
    pub fn stdin(&self) -> &[u8] {
        self.stdin.get().map_or(&[], Vec::as_slice)
    }
}

impl PartialEq for Invocation {
    fn eq(&self, other: &Self) -> bool {
        self.program == other.program && self.args == other.args && self.stdin() == other.stdin()
    }
}

impl Eq for Invocation {}

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
    /// use seam_core::ops::io::{read_to_end, write_all};
    /// use seam_core::ops::process::{CloseStdin, Output, SpawnChild};
    /// use seam_core::{Provide, Perform};
    /// use seam_test::Sim;
    /// use seam_test::process::Child;
    ///
    /// let mut sim = Sim::new();
    /// sim.on_command_with_stdin("tr", |_, input| {
    ///     Ok(Output { code: Some(0), stdout: input.to_ascii_uppercase(), ..Output::default() })
    /// });
    /// let shouted = sim.acquire(SpawnChild::<Child>::new(Command::new("tr")), |_, child| {
    ///     write_all(child, b"quiet")?;
    ///     child.perform(CloseStdin);
    ///     let mut out = Vec::new();
    ///     read_to_end(child, &mut out)?;
    ///     std::io::Result::Ok(out)
    /// }).unwrap();
    /// assert_eq!(shouted, b"QUIET");
    /// assert_eq!(sim.invocations()[0].stdin(), b"quiet");
    /// # }
    /// ```
    pub fn on_command_with_stdin(
        &mut self,
        program: impl Into<OsString>,
        handler: impl FnMut(&Command, &[u8]) -> io::Result<Output> + Send + Sync + 'static,
    ) -> &mut Self {
        let program = program.into();
        let handlers = self.world.lock().process.handlers.clone();
        let mut handlers = handlers.lock();
        handlers.retain(|(p, _)| *p != program);
        handlers.push((program, Box::new(handler)));
        drop(handlers);
        self
    }

    /// Scripts a fixed response: running `program`, with exactly `args` if
    /// given and with any arguments otherwise, produces `output`. Later
    /// responses win, and one for exact arguments wins over one for any.
    /// Replaces a handler from [`on_command`](Self::on_command) for the
    /// same program.
    pub fn script_command(
        &mut self,
        program: OsString,
        args: Option<Vec<OsString>>,
        output: Output,
    ) {
        let rules = Arc::clone(&self.world.lock().process.rules);
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
        self.world.lock().process.log.clone()
    }

    /// Records `cmd`, failing with `NotFound` if no handler is scripted for
    /// it. Returns its index, and the cell for its input.
    fn record(&mut self, cmd: &Command) -> io::Result<(usize, Arc<OnceLock<Vec<u8>>>)> {
        let stdin = Arc::new(OnceLock::new());
        let mut world = self.world.lock();
        let process = &mut world.process;
        process.log.push(Invocation {
            program: cmd.get_program().to_os_string(),
            args: cmd.get_args().map(|a| a.to_os_string()).collect(),
            stdin: Arc::clone(&stdin),
        });
        if !process.handlers.has(cmd.get_program()) {
            return Err(error(
                ErrorKind::NotFound,
                "no handler scripted for this program",
            ));
        }
        Ok((process.log.len() - 1, stdin))
    }
}

impl Perform<RunCommand> for Sim {
    fn perform(&mut self, op: RunCommand) -> io::Result<Output> {
        let cmd = op.0;
        let (_, stdin) = self.record(&cmd)?;
        let _ = stdin.set(Vec::new());
        let handlers = self.world.lock().process.handlers.clone();
        handlers.run(&cmd, &[])
    }
}

/// A simulated child process, acquired with [`SpawnChild`]: a filter whose
/// scripted handler runs when its output is first needed. See the
/// [module docs](self).
pub struct Child {
    handlers: Handlers,
    entry: usize,
    /// The log's record of this child's input.
    given: Arc<OnceLock<Vec<u8>>>,
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
    failed: Option<(ErrorKind, Option<io::Error>)>,
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
    error(
        ErrorKind::BrokenPipe,
        match stream {
            "standard input" => {
                "the child's standard input is not piped to this scope, or is closed"
            }
            "standard output" => "the child's standard output is not piped to this scope",
            _ => "the child's standard error is not piped to this scope",
        },
    )
}

impl Child {
    /// Runs the handler, if it has not run: on the input so far.
    fn run(&mut self) -> &mut Ran {
        if self.ran.is_none() {
            self.stdin_open = false;
            self.record_input();
            let result = self.handlers.run(&self.command, &self.stdin);
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
                    failed: Some((e.kind(), Some(e))),
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
        if let Some((kind, e)) = &mut ran.failed {
            // The handler's own error the first time, then its kind.
            return Err(e.take().unwrap_or_else(|| (*kind).into()));
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
        if let Some((kind, e)) = &mut ran.failed {
            // The handler's own error the first time, then its kind.
            return Err(e.take().unwrap_or_else(|| (*kind).into()));
        }
        let code = ran.code;
        self.waited = true;
        Ok(ExitStatus { code })
    }

    /// Puts the input so far in the log, once.
    fn record_input(&self) {
        let _ = self.given.set(self.stdin.clone());
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
        self.print_inherited(&out, &err);
    }
}

impl Perform<Opening<SpawnChild<Child>>> for Sim {
    fn perform(&mut self, Opening(op, _): Opening<SpawnChild<Child>>) -> io::Result<Child> {
        let (entry, given) = self.record(&op.command)?;
        let mut child = Child {
            handlers: self.world.lock().process.handlers.clone(),
            entry,
            given,
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
}

/// Runs the child if it has not run, and prints what it inherited.
impl Perform<Closing<SpawnChild<Child>>> for Sim {
    fn perform(&mut self, Closing(mut child, _): Closing<SpawnChild<Child>>) -> io::Result<()> {
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

impl Perform<ReadErr<'_>> for Child {
    fn perform(&mut self, op: ReadErr<'_>) -> io::Result<usize> {
        self.read(STDERR, op.0)
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
            self.record_input();
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

/// A child dropped before it ran (its scope failed) leaves its input in the
/// log all the same.
impl Drop for Child {
    fn drop(&mut self) {
        self.record_input();
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
        ReadErr<'_> => io::Result<usize>;
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
