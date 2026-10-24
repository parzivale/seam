//! Child processes on a real Tokio runtime.
#![cfg(all(feature = "full", unix))]

use seam_core::io;
use seam_core::ops::io::{read_to_end_send, write_all_send};
use std::process::Command;
use std::time::{Duration, Instant};

use seam_core::future::SendPerform;

use seam_core::ops::process::{ChildId, CloseStdin, Kill, Pipe, ReadErr, SpawnChild, Wait};
use seam_core::ops::time::Timeout;
use seam_core::{AsyncProvide, Inspect, Perform};
use seam_tokio::Tokio;
use seam_tokio::process::Child;

fn sh(script: &str) -> Command {
    let mut cmd = Command::new("sh");
    cmd.args(["-c", script]);
    cmd
}

#[tokio::test]
async fn write_close_read_wait() -> io::Result<()> {
    let out = Tokio::new()
        .acquire_async(
            SpawnChild::<Child>::new(Command::new("cat")),
            async |_, child| {
                write_all_send(child, b"hello child").await?;
                child.perform(CloseStdin);
                let mut out = Vec::new();
                read_to_end_send(child, &mut out).await?;
                assert!(child.perform_send(Wait).await?.success());
                io::Result::Ok(out)
            },
        )
        .await?;
    assert_eq!(out, b"hello child");
    Ok(())
}

#[tokio::test]
async fn stderr_exit_codes_timeouts_and_kill() -> io::Result<()> {
    let spawn = SpawnChild::<Child>::new(sh("echo err >&2; sleep 30")).stderr(Pipe::Piped);
    Tokio::new()
        .acquire_async(spawn, async |_, child| {
            let mut buf = [0; 16];
            let n = child
                .perform_send(Timeout::new(Duration::from_secs(10), ReadErr(&mut buf)))
                .await?;
            assert_eq!(&buf[..n], b"err\n");
            let e = child
                .perform_send(Timeout::new(Duration::from_millis(200), ReadErr(&mut buf)))
                .await
                .unwrap_err();
            assert_eq!(
                e.kind(),
                io::ErrorKind::TimedOut,
                "stderr stays open while it sleeps"
            );
            let e = child
                .perform_send(Timeout::new(Duration::from_millis(50), Wait))
                .await
                .unwrap_err();
            assert_eq!(e.kind(), io::ErrorKind::TimedOut);
            assert!(child.inspect(ChildId).is_some());
            child.perform(Kill)?;
            let status = child.perform_send(Wait).await?;
            assert_eq!(status.code, None, "killed by a signal");
            io::Result::Ok(())
        })
        .await
}

/// A scope that is cancelled (here, by a timeout around it) kills its child.
#[tokio::test]
async fn a_cancelled_scope_kills_the_child() {
    let start = Instant::now();
    let mut pid = None;
    let mut tokio = Tokio::new();
    let scope = tokio.acquire_async(
        SpawnChild::<Child>::new(sh("sleep 30")),
        async |_, child| {
            pid = child.inspect(ChildId);
            child.perform_send(Wait).await.map(drop)
        },
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(200), scope)
            .await
            .is_err()
    );
    assert!(start.elapsed() < Duration::from_secs(10));
    let pid = pid.unwrap();
    // Killing is immediate; reaping happens in the background.
    let mut gone = false;
    for _ in 0..100 {
        let alive = Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap()
            .success();
        let zombie = std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .map(|s| s.split_whitespace().nth(2) == Some("Z"))
            .unwrap_or(false);
        if !alive || zombie {
            gone = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(gone, "process {pid} was killed");
}
