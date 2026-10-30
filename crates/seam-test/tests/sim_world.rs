//! The simulation is a handler like any other: cores take it where they
//! take a handler, roots list it as a part, and a program's own fakes sit
//! beside it, reading and writing their own world-file blocks.
#![cfg(all(feature = "std", feature = "time"))]

use core::time::Duration;

use seam_core::effects as fx;
use seam_core::future::{AsyncPerform, SendPerform, block_on};
use seam_core::ops::time::{Now, Sleep};
use seam_core::{Inspect, Perform};
use seam_test::Sim;

/// A core, generic as always: it never names its handler.
fn nap<C: Perform<Sleep> + Inspect<Now>>(c: &mut C, secs: u64) -> Duration {
    c.perform(Sleep(Duration::from_secs(secs)));
    c.inspect(Now).since_epoch()
}

#[test]
fn a_core_runs_on_the_simulation_as_on_a_handler() {
    let mut sim = Sim::new();
    assert_eq!(nap(&mut sim, 5), Duration::from_secs(5));
    assert_eq!(nap(&mut sim, 5), Duration::from_secs(10));
}

#[test]
fn and_in_a_root_beside_other_parts() {
    let sim = Sim::new();
    let mut root = seam_core::root! {Sim<fx::Now, fx::Sleep>::clone(&sim)};
    assert_eq!(nap(&mut root, 7), Duration::from_secs(7));
    // Every clone is the same world.
    sim.check_world("clock now=7").unwrap();
}

#[test]
fn async_and_send_cores_run_on_it() {
    async fn later<C: SendPerform<Sleep> + Send>(c: &mut C) {
        c.perform_send(Sleep(Duration::from_secs(1))).await;
    }
    async fn soon<C: AsyncPerform<Sleep>>(c: &mut C) {
        c.perform_async(Sleep(Duration::from_secs(1))).await;
    }
    let mut sim = Sim::new();
    block_on(later(&mut sim));
    block_on(soon(&mut sim));
    assert_eq!(sim.inspect(Now).since_epoch(), Duration::from_secs(2));
}

#[test]
fn to_world_writes_the_world_out() {
    let mut sim = Sim::new();
    sim.load_world("clock now=2").unwrap();
    sim.perform(Sleep(Duration::from_millis(1500)));
    let text = sim.to_world();
    assert!(
        text.contains("clock now-ns=3500000000 wall-ns=3500000000\n"),
        "{text}"
    );
    // And reads back the same.
    let mut copy = Sim::new();
    copy.load_world(&text).unwrap();
    assert_eq!(copy.to_world(), text);
}

#[test]
fn a_block_nothing_reads_is_an_error() {
    let err = Sim::new().load_world("clok now=1").unwrap_err();
    assert_eq!(
        err.to_string(),
        "line 1, column 1: unknown `clok` in the world (did you mean `clock`?)"
    );
}

/// A part built over the simulation: its handler performs the simulation's
/// own effects through the `Sim` it holds.
#[cfg(feature = "fs")]
#[test]
fn a_fake_built_over_the_simulation() {
    use seam_core::{Capability, Effect, Operation, io};

    struct Put(String, String);
    impl Operation for Put {
        type Outcome = io::Result<()>;
    }
    impl Effect for Put {
        type Key = Put;
    }

    /// Keeps each value in a file of the simulated filesystem.
    struct Disk(Sim);
    impl Capability for Disk {}
    impl Perform<Put> for Disk {
        fn perform(&mut self, op: Put) -> io::Result<()> {
            seam_core::ops::fs::write_file(&mut self.0, &format!("/kv/{}", op.0), &op.1)
        }
    }

    let mut sim = Sim::new();
    sim.add_dir("/kv");
    let mut root = seam_core::root![Sim<fx::View>::clone(&sim), Disk<Put>(sim.clone())];
    root.perform(Put("k".into(), "v".into())).unwrap();
    assert_eq!(
        seam_core::ops::fs::read_to_string(&root, "/kv/k").unwrap(),
        "v"
    );
    sim.check_world(r#"fs { file "/kv/k" "v" }"#).unwrap();
}
