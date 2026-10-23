use seam_core::io;
use std::collections::VecDeque;
use std::string::String;
use std::vec::Vec;

use seam_core::Perform;
use seam_core::ops::stdio::{EPrint, Print, ReadLine};

use crate::Sim;

#[derive(Debug, Default)]
pub(crate) struct Stdio {
    stdin: VecDeque<u8>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

impl Sim {
    /// Appends bytes to standard input. Input is a finite script: once it is
    /// consumed, `ReadLine` reports end of input.
    pub fn feed_stdin(&mut self, bytes: impl AsRef<[u8]>) -> &mut Self {
        self.stdio.stdin.extend(bytes.as_ref());
        self
    }

    /// Everything the core has printed to standard output.
    pub fn stdout(&self) -> &[u8] {
        &self.stdio.stdout
    }

    /// Everything the core has printed to standard error.
    pub fn stderr(&self) -> &[u8] {
        &self.stdio.stderr
    }

    /// Takes standard output so far, leaving it empty.
    pub fn take_stdout(&mut self) -> Vec<u8> {
        core::mem::take(&mut self.stdio.stdout)
    }

    /// Takes standard error so far, leaving it empty.
    pub fn take_stderr(&mut self) -> Vec<u8> {
        core::mem::take(&mut self.stdio.stderr)
    }
}

impl Perform<Print<'_>> for Sim {
    fn perform(&mut self, op: Print<'_>) -> io::Result<()> {
        self.stdio.stdout.extend_from_slice(op.0);
        Ok(())
    }
}

impl Perform<EPrint<'_>> for Sim {
    fn perform(&mut self, op: EPrint<'_>) -> io::Result<()> {
        self.stdio.stderr.extend_from_slice(op.0);
        Ok(())
    }
}

impl Perform<ReadLine> for Sim {
    fn perform(&mut self, _: ReadLine) -> io::Result<Option<String>> {
        let stdin = &mut self.stdio.stdin;
        if stdin.is_empty() {
            return Ok(None);
        }
        let end = stdin
            .iter()
            .position(|&b| b == b'\n')
            .map_or(stdin.len(), |i| i + 1);
        let line: Vec<u8> = stdin.drain(..end).collect();
        String::from_utf8(line)
            .map(Some)
            .map_err(|_| crate::error(io::ErrorKind::InvalidData, "stdin line is not valid UTF-8"))
    }
}
