//! Child processes and exiting.

use core::convert::Infallible;
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

operations! {
    RunCommand => io::Result<Output>;
    Exit => Infallible;
}
