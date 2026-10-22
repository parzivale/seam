use std::boxed::Box;
use std::convert::Infallible;
use std::ffi::OsString;
use std::io::{self, ErrorKind};
use std::process::Command;
use std::vec::Vec;

use seam_core::Perform;
use seam_core::ops::process::{Exit, Output, RunCommand};

use crate::{Sim, error};

type Handler = Box<dyn FnMut(&Command) -> io::Result<Output> + Send + Sync>;

#[derive(Default)]
pub(crate) struct Processes {
    handlers: Vec<(OsString, Handler)>,
    log: Vec<Invocation>,
}

/// A command the core ran, recorded for assertions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invocation {
    /// The program.
    pub program: OsString,
    /// Its arguments, without the program name.
    pub args: Vec<OsString>,
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
        handler: impl FnMut(&Command) -> io::Result<Output> + Send + Sync + 'static,
    ) -> &mut Self {
        let program = program.into();
        self.process.handlers.retain(|(p, _)| *p != program);
        self.process.handlers.push((program, Box::new(handler)));
        self
    }

    /// Every command the core has run, in order.
    pub fn invocations(&self) -> &[Invocation] {
        &self.process.log
    }
}

impl Perform<RunCommand> for Sim {
    fn perform(&mut self, op: RunCommand) -> io::Result<Output> {
        let cmd = op.0;
        self.process.log.push(Invocation {
            program: cmd.get_program().to_os_string(),
            args: cmd.get_args().map(|a| a.to_os_string()).collect(),
        });
        let (_, handler) = self
            .process
            .handlers
            .iter_mut()
            .find(|(p, _)| p == cmd.get_program())
            .ok_or_else(|| error(ErrorKind::NotFound, "no handler scripted for this program"))?;
        handler(&cmd)
    }
}

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
