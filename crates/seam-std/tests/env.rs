//! Changing the real environment. One test in its own binary, so that no
//! other test thread reads the environment while it changes: the promise
//! `WritableEnv::new` asks for.
#![cfg(all(feature = "env", feature = "process"))]
#![allow(unsafe_code)]

use std::process::Command;

use seam_core::io;
use seam_core::ops::env::{RemoveVar, SetVar, Var};
use seam_core::ops::process::RunCommand;
use seam_core::{Inspect, Perform};
use seam_std::Std;
use seam_std::env::WritableEnv;

#[test]
fn writable_env_changes_this_process_and_its_children() -> io::Result<()> {
    // SAFETY: this binary runs one test, so no other thread touches the
    // environment while it changes.
    let mut env = unsafe { WritableEnv::new() };
    env.perform(SetVar::new("SEAM_ENV_TEST", "hello"))?;
    assert_eq!(env.inspect(Var::new("SEAM_ENV_TEST")), Some("hello".into()));
    assert_eq!(std::env::var("SEAM_ENV_TEST").as_deref(), Ok("hello"));

    let mut cmd = Command::new("sh");
    cmd.args(["-c", "printf %s \"$SEAM_ENV_TEST\""]);
    assert_eq!(Std::new().perform(RunCommand(cmd))?.stdout, b"hello");

    env.perform(RemoveVar::new("SEAM_ENV_TEST"))?;
    assert_eq!(Std::new().inspect(Var::new("SEAM_ENV_TEST")), None);

    let err = env.perform(SetVar::new("A=B", "x")).unwrap_err();
    assert_eq!(
        err.kind(),
        io::ErrorKind::InvalidInput,
        "checked, not a panic"
    );
    Ok(())
}
