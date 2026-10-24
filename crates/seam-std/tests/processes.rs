//! Child processes on the real OS.
#![cfg(all(feature = "full", unix))]

use seam_core::io;
use seam_core::ops::io::{read_to_end, write_all};
use seam_core::ops::process::read_err_to_end;
use std::process::Command;
use std::time::{Duration, Instant};

use seam_core::ops::io::Read;
use seam_core::ops::process::{ChildId, CloseStdin, Kill, Pipe, SpawnChild, Wait};
use seam_core::ops::time::Timeout;
use seam_core::{Inspect, Perform, Provide};
use seam_std::Std;
use seam_std::process::Child;

fn sleep(secs: u32) -> Command {
    let mut cmd = Command::new("sleep");
    cmd.arg(secs.to_string());
    cmd
}

fn sh(script: &str) -> Command {
    let mut cmd = Command::new("sh");
    cmd.args(["-c", script]);
    cmd
}

#[test]
fn write_close_read_wait() -> io::Result<()> {
    let out = Std::new().acquire(SpawnChild::<Child>::new(Command::new("cat")), |_, child| {
        write_all(child, b"hello child")?;
        child.perform(CloseStdin);
        let mut out = Vec::new();
        read_to_end(child, &mut out)?;
        assert!(child.perform(Wait)?.success());
        io::Result::Ok(out)
    })?;
    assert_eq!(out, b"hello child");
    Ok(())
}

#[test]
fn stderr_and_exit_codes() -> io::Result<()> {
    let spawn = SpawnChild::<Child>::new(sh("echo out; echo err >&2; exit 3")).stderr(Pipe::Piped);
    Std::new().acquire(spawn, |_, child| {
        let mut out = [0; 16];
        let n = child.perform(Read(&mut out))?;
        assert_eq!(&out[..n], b"out\n");
        let mut err = Vec::new();
        read_err_to_end(child, &mut err)?;
        assert_eq!(err, b"err\n");
        let status = child.perform(Wait)?;
        assert_eq!(status.code, Some(3));
        assert_eq!(child.inspect(ChildId), None, "reaped");
        Ok(())
    })
}

#[test]
fn a_stream_not_piped_says_so() -> io::Result<()> {
    let spawn = SpawnChild::<Child>::new(Command::new("true")).stdout(Pipe::Null);
    Std::new().acquire(spawn, |_, child| {
        let err = read_to_end(child, &mut Vec::new()).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::BrokenPipe);
        Ok(())
    })
}

/// A failed scope kills its child; a successful one waits for it.
#[test]
fn a_failed_scope_kills_the_child() {
    let start = Instant::now();
    let mut pid = None;
    let result = Std::new().acquire(SpawnChild::<Child>::new(sleep(30)), |_, child| {
        pid = child.inspect(ChildId);
        Err::<(), _>(io::Error::other("give up"))
    });
    assert!(result.is_err());
    assert!(
        start.elapsed() < Duration::from_secs(10),
        "the child was killed, not waited for"
    );
    let pid = pid.unwrap();
    let alive = Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap()
        .success();
    assert!(!alive, "process {pid} is gone");
}

#[test]
fn closing_does_not_hang_on_unread_output() -> io::Result<()> {
    // The child writes far more than a pipe holds; nobody reads it.
    let spawn = SpawnChild::<Child>::new(sh("head -c 10000000 /dev/zero"));
    let start = Instant::now();
    Std::new().acquire(spawn, |_, _| io::Result::Ok(()))?;
    assert!(start.elapsed() < Duration::from_secs(10));
    Ok(())
}

#[test]
fn timed_waits_and_kill() -> io::Result<()> {
    let spawn = SpawnChild::<Child>::new(sleep(30)).stdout(Pipe::Null);
    Std::new().acquire(spawn, |_, child| {
        let err = child
            .perform(Timeout::new(Duration::from_millis(50), Wait))
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::TimedOut, "still running");
        child.perform(Kill)?;
        let status = child.perform(Timeout::new(Duration::from_secs(10), Wait))?;
        assert_eq!(status.code, None, "killed by a signal");
        child.perform(Kill)?; // again: nothing to do
        Ok(())
    })
}
