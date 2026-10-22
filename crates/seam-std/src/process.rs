use std::convert::Infallible;
use std::io;

use seam_core::Perform;
use seam_core::ops::process::{Exit, Output, RunCommand};

use crate::Std;

impl Perform<RunCommand> for Std {
    fn perform(&mut self, mut op: RunCommand) -> io::Result<Output> {
        op.0.output().map(|o| Output {
            code: o.status.code(),
            stdout: o.stdout,
            stderr: o.stderr,
        })
    }
}

impl Perform<Exit> for Std {
    fn perform(&mut self, op: Exit) -> Infallible {
        std::process::exit(op.0)
    }
}
