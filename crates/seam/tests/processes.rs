//! One child-process core, run against std, Tokio and the simulation.
#![cfg(all(
    feature = "std",
    feature = "tokio",
    feature = "test",
    feature = "full",
    unix
))]

use std::io;
use std::process::Command;

use seam::ops::io::{ReadToEnd, WriteAll};
use seam::ops::process::{CloseStdin, Output, SpawnChild, Wait};
use seam::prelude::*;

fn upper() -> Command {
    let mut cmd = Command::new("tr");
    cmd.args(["a-z", "A-Z"]);
    cmd
}

/// Feeds `input` through a filter and returns what it printed.
fn filter<C, P>(c: &mut C, cmd: Command, input: &[u8]) -> io::Result<Vec<u8>>
where
    C: Provide<SpawnChild<P>>,
    P: for<'a> Perform<WriteAll<'a>>
        + for<'a> Perform<ReadToEnd<'a>>
        + Perform<CloseStdin>
        + Perform<Wait>,
{
    c.acquire(SpawnChild::new(cmd), |_, child| {
        child.perform(WriteAll(input))?;
        child.perform(CloseStdin);
        let mut out = Vec::new();
        child.perform(ReadToEnd(&mut out))?;
        let status = child.perform(Wait)?;
        if !status.success() {
            return Err(io::Error::other(format!("filter failed: {status:?}")));
        }
        Ok(out)
    })
}

/// The same, async with `Send` bounds.
async fn filter_async<C, P>(c: &mut C, cmd: Command, input: &[u8]) -> io::Result<Vec<u8>>
where
    C: AsyncProvide<SpawnChild<P>> + Send,
    P: for<'a> SendPerform<WriteAll<'a>>
        + for<'a> SendPerform<ReadToEnd<'a>>
        + Perform<CloseStdin>
        + SendPerform<Wait>
        + Send,
{
    c.acquire_async(SpawnChild::new(cmd), async |_, child| {
        child.perform_send(WriteAll(input)).await?;
        child.perform(CloseStdin);
        let mut out = Vec::new();
        child.perform_send(ReadToEnd(&mut out)).await?;
        let status = child.perform_send(Wait).await?;
        if !status.success() {
            return Err(io::Error::other(format!("filter failed: {status:?}")));
        }
        Ok(out)
    })
    .await
}

fn simulated_tr() -> seam::test::Sim {
    let mut sim = seam::test::Sim::new();
    sim.on_command_with_stdin("tr", |_, input| {
        Ok(Output {
            code: Some(0),
            stdout: input.to_ascii_uppercase(),
            ..Output::default()
        })
    });
    sim
}

#[test]
fn on_std_and_in_the_simulation() {
    let real =
        filter::<_, seam::stdlib::process::Child>(&mut seam::stdlib::Std::new(), upper(), b"seam")
            .unwrap();
    let simulated =
        filter::<_, seam::test::process::Child>(&mut simulated_tr(), upper(), b"seam").unwrap();
    assert_eq!(real, b"SEAM");
    assert_eq!(simulated, real);
}

#[test]
fn on_tokio_and_the_simulated_runtime() {
    let real = seam::tokio::block_on(async {
        let mut root = seam::root! {
            tokio: seam::tokio::Tokio::new() => [seam::kinds::Process],
        };
        filter_async::<_, seam::tokio::process::Child>(&mut root, upper(), b"seam").await
    })
    .unwrap();
    let simulated = seam::test::rt::Runtime::new(simulated_tr())
        .block_on(|mut sim| async move {
            filter_async::<_, seam::test::process::Child>(&mut sim, upper(), b"seam").await
        })
        .unwrap();
    assert_eq!(real, b"SEAM");
    assert_eq!(simulated, real);
}
