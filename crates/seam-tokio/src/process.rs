use std::convert::Infallible;

use seam_core::Perform;
use seam_core::ops::process::{Exit, Output, RunCommand};

use crate::Tokio;

async_op!(perform [] Tokio, RunCommand, |_s, op| {
    let o = tokio::process::Command::from(op.0).output().await?;
    Ok(Output { code: o.status.code(), stdout: o.stdout, stderr: o.stderr })
});

impl Perform<Exit> for Tokio {
    fn perform(&mut self, op: Exit) -> Infallible {
        std::process::exit(op.0)
    }
}
