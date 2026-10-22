//! Child processes and exiting.
//!
//! Building a `std::process::Command` is pure; running it is the effect.
//! [`Exit`]'s outcome is `Infallible`, so the compiler knows it never
//! returns.
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
