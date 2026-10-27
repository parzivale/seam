//! Standard input, output and error: scripted input, captured output, and
//! the [`Stdin`], [`Stdout`] and [`Stderr`] handles.

use seam_core::io;
use std::collections::VecDeque;
use std::vec::Vec;

use seam_core::effects as fx;
use seam_core::ops::io::{Flush, Read, Write};
use seam_core::ops::stdio::{OpenStderr, OpenStdin, OpenStdout};
use seam_core::{Closing, Opening, Perform};

use crate::sim::{Inner, Shared, SimState, Simulate, Simulated, States, World};
use crate::world_file::{self, Failure, Node, show};

/// The simulated standard streams: input still to be read, and everything
/// written to output and error.
///
/// Its block is `stdio { stdin ..; stdout ..; stderr .. }`, each with its
/// bytes as a string or `hex=`. Loaded, `stdin` is input to read, and
/// `stdout` and `stderr` what is already written. As expectations, `stdout`
/// and `stderr` must be exactly what was written.
#[derive(Debug, Clone, Default)]
pub struct Stdio {
    stdin: VecDeque<u8>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

impl Stdio {
    /// Everything written to standard output.
    pub fn stdout(&self) -> &[u8] {
        &self.stdout
    }

    /// Everything written to standard error.
    pub fn stderr(&self) -> &[u8] {
        &self.stderr
    }
}

impl<G> World<G> {
    /// Appends bytes to standard input. Input is a finite script: once it is
    /// consumed, reads report the end of input.
    pub fn feed_stdin(&mut self, bytes: impl AsRef<[u8]>) -> &mut Self {
        self.lock().state::<Stdio>().stdin.extend(bytes.as_ref());
        self
    }

    /// Everything the core has printed to standard output.
    pub fn stdout(&self) -> Vec<u8> {
        self.lock().state::<Stdio>().stdout.clone()
    }

    /// Everything the core has printed to standard error.
    pub fn stderr(&self) -> Vec<u8> {
        self.lock().state::<Stdio>().stderr.clone()
    }

    /// Takes standard output so far, leaving it empty.
    pub fn take_stdout(&mut self) -> Vec<u8> {
        core::mem::take(&mut self.lock().state::<Stdio>().stdout)
    }

    /// Takes standard error so far, leaving it empty.
    pub fn take_stderr(&mut self) -> Vec<u8> {
        core::mem::take(&mut self.lock().state::<Stdio>().stderr)
    }
}

impl Inner {
    /// Prints what a simulated child inherited.
    #[cfg(feature = "process")]
    pub(crate) fn print_inherited(&mut self, out: &[u8], err: &[u8]) {
        let stdio = self.state::<Stdio>();
        stdio.stdout.extend_from_slice(out);
        stdio.stderr.extend_from_slice(err);
    }
}

impl SimState for Stdio {
    const BLOCKS: &'static [&'static str] = &["stdio"];

    fn load(&mut self, block: &Node) -> world_file::Result<()> {
        for child in block.block()? {
            let bytes = || child.contents(0, "its bytes, or `hex=`");
            match child.name() {
                "stdin" => self.stdin.extend(bytes()?),
                "stdout" => self.stdout.extend(bytes()?),
                "stderr" => self.stderr.extend(bytes()?),
                _ => return Err(child.unknown("`stdio`", &["stdin", "stdout", "stderr"])),
            }
        }
        Ok(())
    }

    fn to_world(&self) -> Vec<Node> {
        let stdin: Vec<u8> = self.stdin.iter().copied().collect();
        let block = Node::new("stdio")
            .with_child(Node::new("stdin").with_bytes(&stdin))
            .with_child(Node::new("stdout").with_bytes(&self.stdout))
            .with_child(Node::new("stderr").with_bytes(&self.stderr));
        std::vec![block]
    }

    fn check(&self, block: &Node, failures: &mut Vec<Failure>) -> world_file::Result<()> {
        for child in block.block()? {
            let got: &[u8] = match child.name() {
                "stdout" => &self.stdout,
                "stderr" => &self.stderr,
                _ => {
                    return Err(child.unknown("`stdio` expectations", &["stdout", "stderr"]));
                }
            };
            let want = child.contents(0, "the expected output, or `hex=`")?;
            if got != want {
                failures.push(child.failure(std::format!(
                    "{}: expected {}, found {}",
                    child.name(),
                    show(&want),
                    show(got)
                )));
            }
        }
        Ok(())
    }
}

/// Standard input, acquired with [`OpenStdin`]. Performs `Read`; never
/// waits, since input is a finite script: once it is consumed, reads return
/// 0, the end of input.
#[derive(Debug)]
pub struct Stdin(Shared);

/// Standard output, acquired with [`OpenStdout`]. Performs `Write` and
/// `Flush`; what it writes lands at once, in [`World::stdout`].
#[derive(Debug)]
pub struct Stdout(Shared);

/// Standard error, acquired with [`OpenStderr`]. Performs `Write` and
/// `Flush`; what it writes lands at once, in [`World::stderr`].
#[derive(Debug)]
pub struct Stderr(Shared);

impl Perform<Read<'_>> for Stdin {
    fn perform(&mut self, op: Read<'_>) -> io::Result<usize> {
        let mut states = self.0.lock();
        let stdin = &mut states.state::<Stdio>().stdin;
        let n = stdin.len().min(op.0.len());
        for (dst, src) in op.0.iter_mut().zip(stdin.drain(..n)) {
            *dst = src;
        }
        Ok(n)
    }
}

impl Perform<Write<'_>> for Stdout {
    fn perform(&mut self, op: Write<'_>) -> io::Result<usize> {
        self.0
            .lock()
            .state::<Stdio>()
            .stdout
            .extend_from_slice(op.0);
        Ok(op.0.len())
    }
}

impl Perform<Write<'_>> for Stderr {
    fn perform(&mut self, op: Write<'_>) -> io::Result<usize> {
        self.0
            .lock()
            .state::<Stdio>()
            .stderr
            .extend_from_slice(op.0);
        Ok(op.0.len())
    }
}

/// Writes land at once, so there is nothing to flush.
impl Perform<Flush> for Stdout {
    fn perform(&mut self, _: Flush) -> io::Result<()> {
        Ok(())
    }
}

/// Writes land at once, so there is nothing to flush.
impl Perform<Flush> for Stderr {
    fn perform(&mut self, _: Flush) -> io::Result<()> {
        Ok(())
    }
}

macro_rules! open {
    ($($op:ident => $handle:ident),*) => {$(
        impl Simulated for fx::$op {
            type State = Stdio;
        }

        impl Simulate<Opening<$op<$handle>>> for fx::$op {
            fn run(states: &mut States<'_>, Opening(_, _): Opening<$op<$handle>>) -> io::Result<$handle> {
                Ok($handle(states.shared()))
            }
        }

        impl Simulate<Closing<$op<$handle>>> for fx::$op {
            fn run(_: &mut States<'_>, Closing(_, _): Closing<$op<$handle>>) -> io::Result<()> {
                Ok(())
            }
        }
    )*};
}
open!(OpenStdin => Stdin, OpenStdout => Stdout, OpenStderr => Stderr);

// Every capability type opts in to seam's lifts.
impl seam_core::Capability for Stdin {}
impl seam_core::Capability for Stdout {}
impl seam_core::Capability for Stderr {}
