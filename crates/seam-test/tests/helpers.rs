//! seam-core's helpers, run on the simulation.
#![cfg(feature = "full")]

use std::net::Ipv4Addr;
use std::process::Command;
use std::time::Duration;

use seam_core::future::block_on;
use seam_core::io::{self, ErrorKind};
use seam_core::ops::env::{var_or, var_or_send, var_string};
use seam_core::ops::fs::{
    append_file, copy_dir_all, exists, is_dir, is_file, read_lines, read_to_string,
    remove_dir_contents, write_file_atomic,
};
use seam_core::ops::io::{read_to_end, write_all};
use seam_core::ops::net::{Accept, TcpBind, connect_any, connect_any_async};
use seam_core::ops::process::{Output, output_string, run_checked, run_checked_send};
use seam_core::ops::random::{choose, random_bool, random_range, shuffle};
use seam_core::ops::stdio::{eprintln, println, println_async, read_line};
use seam_core::ops::time::{Instant, Interval, Now, elapsed, sleep_until, tick};
use seam_core::{Inspect, Provide};
use seam_test::Sim;
use seam_test::net::{TcpListener, TcpStream};

#[test]
fn file_queries() -> io::Result<()> {
    let mut sim = Sim::new();
    sim.add_file("/a/file", "x").add_dir("/a/dir");
    assert!(exists(&sim, "/a/file")? && exists(&sim, "/a/dir")?);
    assert!(!exists(&sim, "/a/missing")?);
    assert!(is_file(&sim, "/a/file")? && !is_file(&sim, "/a/dir")?);
    assert!(is_dir(&sim, "/a/dir")? && !is_dir(&sim, "/a/file")?);
    assert!(!is_file(&sim, "/a/missing")? && !is_dir(&sim, "/a/missing")?);
    Ok(())
}

#[test]
fn appending_and_atomic_writes() -> io::Result<()> {
    let mut sim = Sim::new();
    sim.add_dir("/log");
    append_file(&mut sim, "/log/out", "one\n")?;
    append_file(&mut sim, "/log/out", "two\r\n")?;
    assert_eq!(read_lines(&sim, "/log/out")?, ["one", "two"]);

    write_file_atomic(&mut sim, "/log/state", "v1")?;
    write_file_atomic(&mut sim, "/log/state", "v2")?;
    assert_eq!(read_to_string(&sim, "/log/state")?, "v2");
    Ok(())
}

#[test]
fn copying_and_clearing_a_tree() -> io::Result<()> {
    let mut sim = Sim::new();
    sim.add_file("/src/top", "1")
        .add_file("/src/sub/deep/leaf", "2")
        .add_dir("/src/empty");
    assert_eq!(copy_dir_all(&mut sim, "/src", "/dst")?, 2);
    assert_eq!(sim.file("/dst/top").unwrap(), b"1");
    assert_eq!(sim.file("/dst/sub/deep/leaf").unwrap(), b"2");
    assert!(sim.exists("/dst/empty"));

    assert_eq!(remove_dir_contents(&mut sim, "/dst")?, 3);
    assert!(sim.exists("/dst") && !sim.exists("/dst/top") && !sim.exists("/dst/sub"));
    let missing = remove_dir_contents(&mut sim, "/missing");
    assert_eq!(missing.unwrap_err().kind(), ErrorKind::NotFound);
    Ok(())
}

#[test]
fn deadlines_and_intervals() {
    let mut sim = Sim::new();
    let start = sim.inspect(Now);
    sleep_until(&mut sim, start + Duration::from_secs(5));
    assert_eq!(elapsed(&sim, start), Duration::from_secs(5));
    // A deadline in the past returns at once.
    sleep_until(&mut sim, start);
    assert_eq!(elapsed(&sim, start), Duration::from_secs(5));

    let mut every = Interval::new(start, Duration::from_secs(2));
    // The first deadline has passed, so it returns at once; the ones at 2
    // and 4 seconds are skipped, and the next is at 6.
    assert_eq!(tick(&mut sim, &mut every), start);
    assert_eq!(every.next(), start + Duration::from_secs(6));
    assert_eq!(tick(&mut sim, &mut every), start + Duration::from_secs(6));
    assert_eq!(tick(&mut sim, &mut every), start + Duration::from_secs(8));
    assert_eq!(sim.inspect(Now), start + Duration::from_secs(8));
    assert_eq!(Instant::EPOCH, start);
}

#[test]
fn random_helpers_are_seeded_and_in_range() {
    let draws = |seed| {
        let mut sim = Sim::with_seed(seed);
        let ranged: Vec<u64> = (0..100).map(|_| random_range(&mut sim, 10..13)).collect();
        let mut deck: Vec<u32> = (0..10).collect();
        shuffle(&mut sim, &mut deck);
        let pick = *choose(&mut sim, &deck).unwrap();
        (ranged, deck, pick, random_bool(&mut sim))
    };
    let (ranged, deck, pick, _) = draws(3);
    assert!(ranged.iter().all(|n| (10..13).contains(n)));
    assert!((10..13).all(|n| ranged.contains(&n)));
    let mut sorted = deck.clone();
    sorted.sort();
    assert_eq!(sorted, (0..10).collect::<Vec<_>>());
    assert!(deck.contains(&pick));
    assert_eq!(draws(3), draws(3));

    let mut sim = Sim::new();
    assert_eq!(choose::<_, u8>(&mut sim, &[]), None);
    assert_eq!(random_range(&mut sim, 7..8), 7);
}

/// A fake that always draws zero is rejected forever by the unbiased
/// method; `random_range` still returns.
#[test]
fn random_range_terminates_on_a_constant_source() {
    struct Zero;
    impl seam_core::Capability for Zero {}
    impl seam_core::Perform<seam_core::ops::random::FillRandom<'_>> for Zero {
        fn perform(&mut self, op: seam_core::ops::random::FillRandom<'_>) {
            op.0.fill(0);
        }
    }
    assert_eq!(random_range(&mut Zero, 5..8), 5);
}

#[test]
fn env_strings() -> io::Result<()> {
    let mut sim = Sim::new();
    sim.set_var("USER", "ada");
    assert_eq!(var_string(&sim, "USER")?.as_deref(), Some("ada"));
    assert_eq!(var_string(&sim, "NOPE")?, None);
    assert_eq!(var_or(&sim, "NOPE", "anon")?, "anon");
    assert_eq!(block_on(var_or_send(&sim, "USER", "anon"))?, "ada");
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        sim.set_var("BAD", std::ffi::OsString::from_vec(vec![0xff]));
        assert_eq!(
            var_string(&sim, "BAD").unwrap_err().kind(),
            ErrorKind::InvalidData
        );
    }
    Ok(())
}

#[test]
fn printing_lines_and_reading_them() -> io::Result<()> {
    let mut sim = Sim::new();
    println(&mut sim, "out")?;
    block_on(println_async(&mut sim, "again"))?;
    eprintln(&mut sim, "err")?;
    assert_eq!(sim.stdout(), b"out\nagain\n");
    assert_eq!(sim.stderr(), b"err\n");

    sim.feed_stdin("first\r\nsecond\nlast");
    assert_eq!(read_line(&mut sim)?.as_deref(), Some("first"));
    // Nothing past the first line was consumed.
    assert_eq!(read_line(&mut sim)?.as_deref(), Some("second"));
    assert_eq!(read_line(&mut sim)?.as_deref(), Some("last"));
    assert_eq!(read_line(&mut sim)?, None);
    Ok(())
}

#[test]
fn checked_commands() -> io::Result<()> {
    let mut sim = Sim::new();
    sim.on_command("git", |_| {
        Ok(Output {
            code: Some(0),
            stdout: b"abc123\n".to_vec(),
            ..Output::default()
        })
    });
    sim.on_command("false", |_| {
        Ok(Output {
            code: Some(1),
            stderr: b"no such ref\n".to_vec(),
            ..Output::default()
        })
    });
    assert_eq!(output_string(&mut sim, Command::new("git"))?, "abc123");
    let err = run_checked(&mut sim, Command::new("false")).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Other);
    assert_eq!(
        err.to_string(),
        "the command exited with code 1: no such ref"
    );
    let err = block_on(run_checked_send(&mut sim, Command::new("missing"))).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::NotFound);
    Ok(())
}

#[test]
fn connect_any_skips_addresses_that_refuse() -> io::Result<()> {
    let mut sim = Sim::new();
    sim.add_host("svc.test", Ipv4Addr::new(10, 0, 0, 1))
        .add_host("svc.test", Ipv4Addr::new(10, 0, 0, 2));
    // Only the second address listens.
    sim.acquire(
        TcpBind::<TcpListener>::new(([10, 0, 0, 2], 80)),
        |sim, listener| {
            let mut calls = 0;
            connect_any(sim, "svc.test", 80, |_, s: &mut TcpStream| {
                calls += 1;
                write_all(s, b"hi")
            })?;
            assert_eq!(calls, 1);
            let got = listener.acquire(Accept::<TcpStream>::new(), |_, server| {
                let mut got = Vec::new();
                read_to_end(server, &mut got)?;
                io::Result::Ok(got)
            })?;
            assert_eq!(got, b"hi");

            // The async form, the same way.
            block_on(connect_any_async(
                sim,
                "svc.test",
                80,
                async |_, s: &mut TcpStream| write_all(s, b"!"),
            ))
        },
    )?;

    let err = connect_any(&mut sim, "nowhere.test", 80, |_, _: &mut TcpStream| Ok(()));
    assert_eq!(err.unwrap_err().kind(), ErrorKind::NotFound);
    Ok(())
}

#[test]
fn connect_any_runs_its_body_once() -> io::Result<()> {
    let mut sim = Sim::new();
    sim.add_host("svc.test", Ipv4Addr::new(10, 0, 0, 1))
        .add_host("svc.test", Ipv4Addr::new(10, 0, 0, 2));
    // The first address listens. Were a failing body retried, the second
    // would refuse, and that error would be returned instead.
    sim.acquire(
        TcpBind::<TcpListener>::new(([10, 0, 0, 1], 80)),
        |sim, _| {
            let mut calls = 0;
            let result = connect_any(sim, "svc.test", 80, |_, _: &mut TcpStream| {
                calls += 1;
                io::Result::<()>::Err(io::Error::other("the body failed"))
            });
            assert_eq!(result.unwrap_err().to_string(), "the body failed");
            assert_eq!(calls, 1);
            Ok(())
        },
    )
}
