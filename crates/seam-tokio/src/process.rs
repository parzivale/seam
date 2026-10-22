use std::convert::Infallible;

use seam_core::Perform;
use seam_core::ops::process::{Exit, RunCommand};

use crate::Tokio;

async_op!(perform [] Tokio, RunCommand, |_s, op| {
    tokio::process::Command::from(op.0).output().await
});

impl Perform<Exit> for Tokio {
    fn perform(&mut self, op: Exit) -> Infallible {
        std::process::exit(op.0)
    }
}
