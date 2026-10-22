use std::convert::Infallible;
use std::io;
use std::process::Output;

use seam_core::Perform;
use seam_core::ops::process::{Exit, RunCommand};

use crate::Std;

impl Perform<RunCommand> for Std {
    fn perform(&mut self, mut op: RunCommand) -> io::Result<Output> {
        op.0.output()
    }
}

impl Perform<Exit> for Std {
    fn perform(&mut self, op: Exit) -> Infallible {
        std::process::exit(op.0)
    }
}
