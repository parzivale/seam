//! Child processes and exiting.

use core::convert::Infallible;
use std::io;
use std::process::{Command, Output};

/// Run a command to completion and collect its output.
#[derive(Debug)]
pub struct RunCommand(pub Command);

/// End the current process with an exit code. Never returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Exit(pub i32);

operations! {
    RunCommand => io::Result<Output>;
    Exit => Infallible;
}
