//! Scripted commands, child processes, and exiting.
//!
//! Commands are scripted per program with [`Sim::on_command`](crate::Sim::on_command) (or
//! [`Sim::on_command_with_stdin`](crate::Sim::on_command_with_stdin)): a handler sees the command, and the
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
use std::format;
use std::process::Command;
use std::string::String;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::vec::Vec;

use seam_core::ops::io::{Flush, Read, Write};
use seam_core::ops::process::{
    ChildId, ChildPipes, CloseStdin, Exit, ExitStatus, Kill, Output, Pipe, ReadErr, RunCommand,
    SpawnChild, SplitChild, Wait,
};
use seam_core::{Closing, Inspect, Opening, Perform};

use crate::Sim;
use crate::error;
use crate::sim::{Inner, States, World};
use crate::world_file::{self, Failure, Node, show};

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

/// The simulated processes: the scripts that answer commands, and the log
/// of every command run or spawned.
///
/// Its block is `process { command PROGRAM [ARGS..] exit=N stdout=".."
/// stderr=".." }`: a fixed response. Written out, it lists the fixed
/// responses, and the log as `ran PROGRAM [ARGS..] stdin=".."` lines. As
/// expectations, the `ran` lines must be every command, in order.
#[derive(Default)]
pub struct Processes {
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
        self.lock()
            .process()
            .on_command(program.into(), Box::new(handler));
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
    ) -> &mut Self {
        self.lock().process().script(program, args, output);
        self
    }

    /// Every command the core has run or spawned, in order.
    pub fn invocations(&self) -> Vec<Invocation> {
        self.lock().process().log.clone()
    }
}

impl Processes {
    /// Scripts `program` with `handler`, replacing any earlier one.
    fn on_command(&mut self, program: OsString, handler: Handler) {
        let mut handlers = self.handlers.lock();
        handlers.retain(|(p, _)| *p != program);
        handlers.push((program, handler));
    }

    /// Scripts a fixed response; see [`Sim::script_command`].
    fn script(&mut self, program: OsString, args: Option<Vec<OsString>>, output: Output) {
        let rules = Arc::clone(&self.rules);
        rules
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(Rule {
                program: program.clone(),
                args,
                output,
            });
        self.on_command(
            program,
            Box::new(move |cmd: &Command, _: &[u8]| {
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
            }),
        );
    }
}

impl Inner {
    /// The processes.
    pub(crate) fn process(&mut self) -> &mut Processes {
        self.state::<Processes>()
    }

    /// Records `cmd`, failing with `NotFound` if no handler is scripted for
    /// it. Returns its index, and the cell for its input.
    fn record(&mut self, cmd: &Command) -> io::Result<(usize, Arc<OnceLock<Vec<u8>>>)> {
        let stdin = Arc::new(OnceLock::new());
        let process = self.process();
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
        let states = &mut States::new(&self.shared);
        let cmd = op.0;
        let (_, stdin) = states.inner().record(&cmd)?;
        let _ = stdin.set(Vec::new());
        let handlers = states.inner().process().handlers.clone();
        // The handler is the test's own code: it may use the world.
        states.unlock();
        handlers.run(&cmd, &[])
    }
}

/// A simulated child process, acquired with [`SpawnChild`]: a filter whose
/// scripted handler runs when its output is first needed. See the
/// [module docs](self).
///
/// [`SplitChild`] lends its pipes as [`ChildStdin`], [`ChildStdout`] and
/// [`ChildStderr`], which share it: the handler still runs when output is
/// first read, on the input written by then, so write and close the input
/// before reading.
pub struct Child(Arc<Mutex<ChildState>>);

/// A [`SplitChild`]'s pipe to a simulated child's standard input.
pub struct ChildStdin(Arc<Mutex<ChildState>>);

/// A [`SplitChild`]'s pipe from a simulated child's standard output.
pub struct ChildStdout(Arc<Mutex<ChildState>>);

/// A [`SplitChild`]'s pipe from a simulated child's standard error.
pub struct ChildStderr(Arc<Mutex<ChildState>>);

/// Locks a child's state. A handler that panicked leaves it usable.
fn lock(state: &Mutex<ChildState>) -> MutexGuard<'_, ChildState> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

macro_rules! debug_child {
    ($($ty:ident),*) => {$(
        impl core::fmt::Debug for $ty {
            fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                f.debug_tuple(stringify!($ty)).field(&*lock(&self.0)).finish()
            }
        }
    )*};
}

debug_child!(Child, ChildStdin, ChildStdout, ChildStderr);

/// A child's state, shared by the child and its split pipes.
struct ChildState {
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

impl core::fmt::Debug for ChildState {
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

impl ChildState {
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

impl Inner {
    #[allow(unused_variables)]
    fn deliver(&mut self, (out, err): (Vec<u8>, Vec<u8>)) {
        #[cfg(feature = "io-std")]
        self.print_inherited(&out, &err);
    }
}

impl Perform<Opening<SpawnChild<Child>>> for Sim {
    fn perform(&mut self, Opening(op, _): Opening<SpawnChild<Child>>) -> io::Result<Child> {
        let states = &mut States::new(&self.shared);
        let (entry, given) = states.inner().record(&op.command)?;
        let mut child = ChildState {
            handlers: states.inner().process().handlers.clone(),
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
            // No input will come: it runs now. The handler is the test's
            // own code: it may use the world.
            states.unlock();
            child.run();
            let inherited = child.take_inherited();
            states.inner().deliver(inherited);
        }
        Ok(Child(Arc::new(Mutex::new(child))))
    }
}

/// Runs the child if it has not run, and prints what it inherited.
impl Perform<Closing<SpawnChild<Child>>> for Sim {
    fn perform(&mut self, Closing(child, _): Closing<SpawnChild<Child>>) -> io::Result<()> {
        let states = &mut States::new(&self.shared);
        let mut child = lock(&child.0);
        child.run();
        let inherited = child.take_inherited();
        states.inner().deliver(inherited);
        Ok(())
    }
}

impl Perform<Write<'_>> for Child {
    fn perform(&mut self, op: Write<'_>) -> io::Result<usize> {
        lock(&self.0).write(op.0)
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
        lock(&self.0).read(STDOUT, op.0)
    }
}

impl Perform<ReadErr<'_>> for Child {
    fn perform(&mut self, op: ReadErr<'_>) -> io::Result<usize> {
        lock(&self.0).read(STDERR, op.0)
    }
}

impl Perform<CloseStdin> for Child {
    fn perform(&mut self, _: CloseStdin) {
        lock(&self.0).stdin_open = false;
    }
}

impl Perform<Wait> for Child {
    fn perform(&mut self, _: Wait) -> io::Result<ExitStatus> {
        lock(&self.0).wait()
    }
}

/// A child that has not run yet never will: it ends with no output, as if
/// killed by a signal. One that has run has already exited.
impl Perform<Kill> for Child {
    fn perform(&mut self, _: Kill) -> io::Result<()> {
        let mut child = lock(&self.0);
        if child.ran.is_none() {
            child.stdin_open = false;
            child.record_input();
            child.ran = Some(Ran {
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

/// The pipes share the child, so dropping them, even when the scope fails,
/// changes nothing.
impl Perform<Opening<SplitChild<ChildStdin, ChildStdout, ChildStderr>>> for Child {
    fn perform(
        &mut self,
        Opening(_, _): Opening<SplitChild<ChildStdin, ChildStdout, ChildStderr>>,
    ) -> io::Result<ChildPipes<ChildStdin, ChildStdout, ChildStderr>> {
        Ok(ChildPipes {
            stdin: ChildStdin(Arc::clone(&self.0)),
            stdout: ChildStdout(Arc::clone(&self.0)),
            stderr: ChildStderr(Arc::clone(&self.0)),
        })
    }
}

impl Perform<Closing<SplitChild<ChildStdin, ChildStdout, ChildStderr>>> for Child {
    fn perform(
        &mut self,
        Closing(_, _): Closing<SplitChild<ChildStdin, ChildStdout, ChildStderr>>,
    ) -> io::Result<()> {
        Ok(())
    }
}

impl Perform<Write<'_>> for ChildStdin {
    fn perform(&mut self, op: Write<'_>) -> io::Result<usize> {
        lock(&self.0).write(op.0)
    }
}

impl Perform<Flush> for ChildStdin {
    fn perform(&mut self, _: Flush) -> io::Result<()> {
        Ok(())
    }
}

impl Perform<CloseStdin> for ChildStdin {
    fn perform(&mut self, _: CloseStdin) {
        lock(&self.0).stdin_open = false;
    }
}

/// Runs the child, if it has not run, on the input so far.
impl Perform<Read<'_>> for ChildStdout {
    fn perform(&mut self, op: Read<'_>) -> io::Result<usize> {
        lock(&self.0).read(STDOUT, op.0)
    }
}

/// Runs the child, if it has not run, on the input so far.
impl Perform<Read<'_>> for ChildStderr {
    fn perform(&mut self, op: Read<'_>) -> io::Result<usize> {
        lock(&self.0).read(STDERR, op.0)
    }
}

/// A child dropped before it ran (its scope failed) leaves its input in the
/// log all the same.
impl Drop for ChildState {
    fn drop(&mut self) {
        self.record_input();
    }
}

/// A made-up process id, until the child has been waited for.
impl Inspect<ChildId> for Child {
    fn inspect(&self, _: ChildId) -> Option<u32> {
        let child = lock(&self.0);
        (!child.waited).then(|| 1000 + u32::try_from(child.entry).unwrap_or(u32::MAX - 1000))
    }
}

/// Nothing in a `Sim` waits, so these behave as without a timeout.
#[cfg(feature = "time")]
mod timeouts {
    use seam_core::ops::time::Timeout;

    use super::*;

    macro_rules! untimed {
        ($($ty:ty: $op:ty => $out:ty;)*) => {$(
            impl Perform<Timeout<$op>> for $ty {
                fn perform(&mut self, op: Timeout<$op>) -> $out {
                    self.perform(op.op)
                }
            }
        )*};
    }

    untimed! {
        Child: Wait => io::Result<ExitStatus>;
        Child: Read<'_> => io::Result<usize>;
        Child: ReadErr<'_> => io::Result<usize>;
        ChildStdout: Read<'_> => io::Result<usize>;
        ChildStderr: Read<'_> => io::Result<usize>;
    }
}

// Every capability type opts in to seam's lifts.
impl seam_core::Capability for Child {}
impl seam_core::Capability for ChildStdin {}
impl seam_core::Capability for ChildStdout {}
impl seam_core::Capability for ChildStderr {}

/// Unwinds with [`Exited`] instead of ending the process. The panic hook is
/// not run, so nothing is printed.
impl Perform<Exit> for Sim {
    fn perform(&mut self, op: Exit) -> Infallible {
        std::panic::resume_unwind(Box::new(Exited(op.0)))
    }
}

/// One `command PROGRAM [ARGS..] exit=N stdout=".." stderr=".."` line.
fn command(node: &Node) -> world_file::Result<(OsString, Option<Vec<OsString>>, Output)> {
    node.no_children()?;
    node.props_only(&["exit", "stdout", "stderr"])?;
    node.arity(1, None, "a program, then any arguments it must be run with")?;
    let program = OsString::from(node.args()[0].string("the program")?);
    let args = node.args()[1..]
        .iter()
        .map(|a| a.string("an argument").map(OsString::from))
        .collect::<world_file::Result<Vec<_>>>()?;
    let code = match node.prop("exit") {
        Some(v) => Some(v.uint::<i32>("`exit`")?),
        None => Some(0),
    };
    let bytes = |key: &str| -> world_file::Result<Vec<u8>> {
        Ok(match node.prop(key) {
            Some(v) => v.string(key)?.as_bytes().to_vec(),
            None => Vec::new(),
        })
    };
    let output = Output {
        code,
        stdout: bytes("stdout")?,
        stderr: bytes("stderr")?,
    };
    Ok((program, (!args.is_empty()).then_some(args), output))
}

/// One `ran PROGRAM [ARGS..] [stdin=".."]` line: the words, and the input.
fn ran(node: &Node) -> world_file::Result<(Vec<OsString>, Option<Vec<u8>>)> {
    node.no_children()?;
    node.props_only(&["stdin"])?;
    node.arity(1, None, "a program, then its arguments")?;
    let words = node
        .args()
        .iter()
        .map(|a| a.string("an argument").map(OsString::from))
        .collect::<world_file::Result<Vec<_>>>()?;
    let stdin = match node.prop("stdin") {
        Some(v) => Some(v.string("`stdin`")?.as_bytes().to_vec()),
        None => None,
    };
    Ok((words, stdin))
}

impl World for Processes {
    const BLOCKS: &'static [&'static str] = &["process"];

    /// `command ..` scripts a fixed response; `ran ..` adds to the log, as
    /// if the command had run.
    fn load(&mut self, block: &Node) -> world_file::Result<()> {
        for child in block.block()? {
            match child.name() {
                "command" => {
                    let (program, args, output) = command(child)?;
                    self.script(program, args, output);
                }
                "ran" => {
                    let (mut words, stdin) = ran(child)?;
                    let program = words.remove(0);
                    self.log.push(Invocation {
                        program,
                        args: words,
                        stdin: Arc::new(OnceLock::from(stdin.unwrap_or_default())),
                    });
                }
                _ => return Err(child.unknown("`process`", &["command", "ran"])),
            }
        }
        Ok(())
    }

    fn to_world(&self) -> Vec<Node> {
        let mut block = Node::new("process");
        for rule in self
            .rules
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
        {
            let mut node =
                Node::new("command").with_arg(rule.program.to_string_lossy().into_owned());
            for arg in rule.args.iter().flatten() {
                node = node.with_arg(arg.to_string_lossy().into_owned());
            }
            if let Some(code) = rule.output.code {
                node = node.with_prop("exit", code);
            }
            for (key, bytes) in [
                ("stdout", &rule.output.stdout),
                ("stderr", &rule.output.stderr),
            ] {
                if !bytes.is_empty() {
                    node = node.with_prop(key, String::from_utf8_lossy(bytes).into_owned());
                }
            }
            block = block.with_child(node);
        }
        for invocation in &self.log {
            let mut node =
                Node::new("ran").with_arg(invocation.program.to_string_lossy().into_owned());
            for arg in &invocation.args {
                node = node.with_arg(arg.to_string_lossy().into_owned());
            }
            if !invocation.stdin().is_empty() {
                node = node.with_prop(
                    "stdin",
                    String::from_utf8_lossy(invocation.stdin()).into_owned(),
                );
            }
            block = block.with_child(node);
        }
        std::vec![block]
    }

    /// The `ran` lines must be every command run or spawned, in order; a
    /// `stdin=` also checks what a child was given.
    fn check(&self, block: &Node, failures: &mut Vec<Failure>) -> world_file::Result<()> {
        let mut want = Vec::new();
        for child in block.block()? {
            if child.name() != "ran" {
                return Err(child.unknown("`process` expectations", &["ran"]));
            }
            let (words, stdin) = ran(child)?;
            want.push((child, words, stdin));
        }
        let got: Vec<Vec<OsString>> = self
            .log
            .iter()
            .map(|i| {
                core::iter::once(i.program.clone())
                    .chain(i.args.iter().cloned())
                    .collect()
            })
            .collect();
        let line = |words: &[OsString]| {
            words
                .iter()
                .map(|w| w.to_string_lossy())
                .collect::<Vec<_>>()
                .join(" ")
        };
        for (n, (node, words, stdin)) in want.iter().enumerate() {
            match got.get(n) {
                Some(g) if g == words => {
                    let given = self.log[n].stdin();
                    if let Some(stdin) = stdin.as_ref().filter(|s| *s != given) {
                        failures.push(node.failure(format!(
                            "command {}: expected stdin {}, but it was given {}",
                            n + 1,
                            show(stdin),
                            show(given)
                        )));
                    }
                }
                Some(g) => failures.push(node.failure(format!(
                    "command {}: expected `{}`, but it ran `{}`",
                    n + 1,
                    line(words),
                    line(g)
                ))),
                None => failures.push(node.failure(format!(
                    "command {}: expected `{}`, but only {} ran",
                    n + 1,
                    line(words),
                    got.len()
                ))),
            }
        }
        if got.len() > want.len() {
            let extra: Vec<String> = got[want.len()..]
                .iter()
                .map(|g| format!("`{}`", line(g)))
                .collect();
            failures.push(block.failure(format!(
                "more commands ran than expected: {}",
                extra.join(", ")
            )));
        }
        Ok(())
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
