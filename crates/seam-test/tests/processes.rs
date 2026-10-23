//! Simulated child processes: filters scripted by handlers.
#![cfg(feature = "full")]

use seam_core::io;
use std::process::Command;

use seam_core::ops::io::{Read, ReadToEnd, WriteAll};
use seam_core::ops::process::{
    ChildId, CloseStdin, Kill, Output, Pipe, ReadErrToEnd, SpawnChild, Wait,
};
use seam_core::{Acquire, Inspect, Perform};
use seam_test::Sim;
use seam_test::process::Child;

/// A `tr a-z A-Z`: uppercases its input, and complains on stderr.
fn shouting() -> Sim {
    let mut sim = Sim::new();
    sim.on_command_with_stdin("tr", |_, input| {
        Ok(Output {
            code: Some(0),
            stdout: input.to_ascii_uppercase(),
            stderr: b"too loud\n".to_vec(),
        })
    });
    sim
}

#[test]
fn a_child_runs_on_its_input() -> io::Result<()> {
    let mut sim = shouting();
    let spawn = SpawnChild::<Child>::new(Command::new("tr")).stderr(Pipe::Piped);
    let (out, err, status) = sim.acquire(spawn, |_, child| {
        child.perform(WriteAll(b"hello "))?;
        child.perform(WriteAll(b"there"))?;
        child.perform(CloseStdin);
        let mut out = Vec::new();
        child.perform(ReadToEnd(&mut out))?;
        let mut err = Vec::new();
        child.perform(ReadErrToEnd(&mut err))?;
        let status = child.perform(Wait)?;
        io::Result::Ok((out, err, status))
    })?;
    assert_eq!(out, b"HELLO THERE");
    assert_eq!(err, b"too loud\n");
    assert!(status.success());
    let ran = &sim.invocations()[0];
    assert_eq!(
        (ran.program.to_str(), ran.stdin()),
        (Some("tr"), &b"hello there"[..])
    );
    Ok(())
}

#[test]
fn reading_runs_it_and_later_input_is_refused() -> io::Result<()> {
    let mut sim = shouting();
    sim.acquire(SpawnChild::<Child>::new(Command::new("tr")), |_, child| {
        child.perform(WriteAll(b"abc"))?;
        let mut buf = [0; 2];
        let n = child.perform(Read(&mut buf))?; // runs it, on "abc"
        assert_eq!(&buf[..n], b"AB");
        let n = child.perform(Read(&mut buf))?;
        assert_eq!(&buf[..n], b"C");
        assert_eq!(child.perform(Read(&mut buf))?, 0, "end of output");
        let err = child.perform(WriteAll(b"more")).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::BrokenPipe);
        Ok(())
    })
}

#[test]
fn inherited_output_goes_to_the_sims_streams() -> io::Result<()> {
    let mut sim = shouting();
    // Piped input: it runs when the scope closes, and its output is printed then.
    let spawn = SpawnChild::<Child>::new(Command::new("tr")).stdout(Pipe::Inherit);
    sim.acquire(spawn, |_, child| child.perform(WriteAll(b"loud")))?;
    // No input: it runs at once.
    let spawn = SpawnChild::<Child>::new(Command::new("tr"))
        .stdin(Pipe::Null)
        .stdout(Pipe::Null)
        .stderr(Pipe::Inherit);
    sim.acquire(spawn, |_, _| io::Result::Ok(()))?;
    assert_eq!(sim.stdout(), b"LOUD");
    assert_eq!(
        sim.stderr(),
        b"too loud\ntoo loud\n",
        "the first child inherited stderr too"
    );
    Ok(())
}

#[test]
fn unscripted_programs_do_not_start() {
    let mut sim = Sim::new();
    let err = sim
        .acquire(SpawnChild::<Child>::new(Command::new("nope")), |_, _| {
            io::Result::Ok(())
        })
        .unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::NotFound);
    assert_eq!(sim.invocations().len(), 1, "the attempt is recorded");
}

#[test]
fn a_killed_child_never_runs() -> io::Result<()> {
    let mut sim = shouting();
    sim.acquire(SpawnChild::<Child>::new(Command::new("tr")), |_, child| {
        child.perform(WriteAll(b"x"))?;
        assert!(child.inspect(ChildId).is_some());
        child.perform(Kill)?;
        assert_eq!(child.perform(Wait)?.code, None, "as if killed by a signal");
        assert_eq!(child.inspect(ChildId), None, "reaped");
        let mut out = Vec::new();
        child.perform(ReadToEnd(&mut out))?;
        assert!(out.is_empty());
        Ok(())
    })
}

#[test]
fn world_files_script_children_and_check_their_input() -> io::Result<()> {
    let mut sim = Sim::from_world(r#"process { command wc stdout="1\n" }"#).unwrap();
    sim.acquire(SpawnChild::<Child>::new(Command::new("wc")), |_, child| {
        child.perform(WriteAll(b"one line\n"))?;
        let mut out = Vec::new();
        child.perform(ReadToEnd(&mut out))?;
        assert_eq!(out, b"1\n");
        io::Result::Ok(())
    })?;
    sim.assert_world(r#"process { ran wc stdin="one line\n" }"#);
    let mismatch = sim
        .check_world(r#"process { ran wc stdin="two" }"#)
        .unwrap_err();
    assert!(
        mismatch.to_string().contains("expected stdin \"two\""),
        "{mismatch}"
    );
    Ok(())
}

/// On the runtime and simulated threads, children come from the same
/// scripts.
#[test]
fn children_on_the_runtime_and_threads() {
    use seam_core::AsyncAcquire;
    use seam_core::future::SendPerform;
    use seam_test::rt::Runtime;
    use seam_test::threads::Threads;

    let rt = Runtime::new(shouting());
    let out = rt.block_on(|mut sim| async move {
        sim.acquire_async(
            SpawnChild::<Child>::new(Command::new("tr")),
            async |_, child| {
                child.perform_send(WriteAll(b"async")).await?;
                let mut out = Vec::new();
                child.perform_send(ReadToEnd(&mut out)).await?;
                io::Result::Ok(out)
            },
        )
        .await
    });
    assert_eq!(out.unwrap(), b"ASYNC");

    let threads = Threads::new(shouting());
    let out = threads.run(|mut sim| {
        sim.acquire(SpawnChild::<Child>::new(Command::new("tr")), |_, child| {
            child.perform(WriteAll(b"threaded"))?;
            let mut out = Vec::new();
            child.perform(ReadToEnd(&mut out))?;
            io::Result::Ok(out)
        })
    });
    assert_eq!(out.unwrap(), b"THREADED");
}

/// A child whose scope fails before it runs still leaves its input in the
/// log, and the log is a plain slice of the `Sim`.
#[test]
fn an_abandoned_child_records_its_input() {
    let mut sim = shouting();
    let result = sim.acquire(SpawnChild::<Child>::new(Command::new("tr")), |_, child| {
        child.perform(WriteAll(b"half"))?;
        Err::<(), _>(io::Error::other("give up"))
    });
    assert!(result.is_err());
    let log: &[seam_test::Invocation] = sim.invocations();
    assert_eq!(log[0].stdin(), b"half");
}
