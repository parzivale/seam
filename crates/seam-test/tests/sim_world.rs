//! Worlds of simulated effects: each effect simulated on its own key, one
//! state per type, world files read and written by the states.
#![cfg(all(feature = "std", feature = "time"))]

use core::time::Duration;
use std::vec::Vec;

use seam_core::effects as fx;
use seam_core::future::{AsyncPerform, SendPerform, block_on};
use seam_core::ops::time::{Now, Sleep, WallClock};
use seam_core::{Capability, Effect, Inspect, Operation, Perform, io};
use seam_test::sim::{SimState, Simulate, Simulated, States};
use seam_test::world_file::{self, Node};

/// A core, generic as always: it never names the world.
fn nap<C: Perform<Sleep> + Inspect<Now>>(c: &mut C, secs: u64) -> Duration {
    c.perform(Sleep(Duration::from_secs(secs)));
    c.inspect(Now).since_epoch()
}

#[test]
fn a_core_runs_on_a_world() {
    let mut world = seam_test::world![fx::Now, fx::Sleep];
    assert_eq!(nap(&mut world, 5), Duration::from_secs(5));
    assert_eq!(nap(&mut world, 5), Duration::from_secs(10));
}

#[test]
fn effects_that_share_a_state_share_one_value() {
    let mut world = seam_test::world![fx::Now, fx::Sleep, fx::WallClock];
    world.load_world("clock wall=1000").unwrap();
    world.perform(Sleep(Duration::from_secs(3)));
    let wall = world.inspect(WallClock);
    assert_eq!(wall, std::time::UNIX_EPOCH + Duration::from_secs(1003));
    world.state(|clock: &mut seam_test::sim::Clock| {
        assert_eq!(clock.now, Duration::from_secs(3));
    });
}

#[test]
fn to_world_writes_the_world_out() {
    let mut world = seam_test::world![fx::Now, fx::Sleep];
    world.load_world("clock now=2").unwrap();
    world.perform(Sleep(Duration::from_millis(1500)));
    assert_eq!(
        world.to_world(),
        "clock now-ns=3500000000 wall-ns=3500000000\n"
    );
    // And reads back the same.
    let mut copy = seam_test::world![fx::Now, fx::Sleep];
    copy.load_world(&world.to_world()).unwrap();
    assert_eq!(copy.to_world(), world.to_world());
}

#[test]
fn checking_is_a_subset_of_to_world() {
    let mut world = seam_test::world![fx::Now, fx::Sleep];
    world.perform(Sleep(Duration::from_secs(4)));
    world.check_world("clock now=4").unwrap(); // `wall` is not listed, so not checked
    let mismatch = world.check_world("\n\nclock now=5").unwrap_err();
    assert_eq!(mismatch.failures.len(), 1);
    assert_eq!(mismatch.failures[0].line, 3);
    assert_eq!(
        mismatch.failures[0].message,
        "clock: expected 5s since the start, found 4s"
    );
}

#[test]
fn a_block_no_state_reads_is_an_error() {
    let mut world = seam_test::world![fx::Now];
    let err = world.load_world("clok now=1").unwrap_err();
    assert_eq!(
        err.to_string(),
        "line 1, column 1: unknown `clok` in the world (did you mean `clock`?)"
    );
}

#[test]
fn async_and_send_cores_run_on_a_world() {
    async fn later<C: SendPerform<Sleep> + Send>(c: &mut C) {
        c.perform_send(Sleep(Duration::from_secs(1))).await;
    }
    async fn soon<C: AsyncPerform<Sleep>>(c: &mut C) {
        c.perform_async(Sleep(Duration::from_secs(1))).await;
    }
    let mut world = seam_test::world![fx::Now, fx::Sleep];
    block_on(later(&mut world));
    block_on(soon(&mut world));
    assert_eq!(world.inspect(Now).since_epoch(), Duration::from_secs(2));
}

// ---- a program's own effect, simulated the same way ----

#[derive(Debug, Clone, PartialEq, Eq)]
struct BindMount {
    source: String,
    target: String,
}
impl Operation for BindMount {
    type Outcome = io::Result<()>;
}
impl Effect for BindMount {
    type Key = BindMount;
}

struct Unmount(String);
impl Operation for Unmount {
    type Outcome = io::Result<()>;
}
impl Effect for Unmount {
    type Key = Unmount;
}

/// The mounts in place: the state both effects work on.
#[derive(Debug, Default)]
struct Mounts(Vec<BindMount>);

impl SimState for Mounts {
    const BLOCKS: &'static [&'static str] = &["mounts"];

    fn load(&mut self, block: &Node) -> world_file::Result<()> {
        for node in block.block()? {
            node.arity(2, Some(2), "a source and a target")?;
            self.0.push(BindMount {
                source: node.args()[0].string("the source")?.into(),
                target: node.args()[1].string("the target")?.into(),
            });
        }
        Ok(())
    }

    fn to_world(&self) -> Vec<Node> {
        let mut block = Node::new("mounts");
        for m in &self.0 {
            block = block.with_child(Node::new("bind").with_arg(&m.source).with_arg(&m.target));
        }
        std::vec![block]
    }
    // No `check`: the default, a subset of `to_world`, is what mounts need.
}

impl Simulated for BindMount {
    type State = Mounts;
}
impl Simulate<BindMount> for BindMount {
    fn run(states: &mut States, op: BindMount) -> io::Result<()> {
        states.state::<Mounts>().0.push(op);
        Ok(())
    }
}

impl Simulated for Unmount {
    type State = Mounts;
}
impl Simulate<Unmount> for Unmount {
    fn run(states: &mut States, op: Unmount) -> io::Result<()> {
        let mounts = &mut states.state::<Mounts>().0;
        match mounts.iter().rposition(|m| m.target == op.0) {
            Some(i) => {
                mounts.remove(i);
                Ok(())
            }
            None => Err(io::Error::new(io::ErrorKind::InvalidInput, "not mounted")),
        }
    }
}

fn remount<C: Perform<BindMount> + Perform<Unmount>>(c: &mut C) -> io::Result<()> {
    c.perform(Unmount("/mnt/old".into()))?;
    c.perform(BindMount {
        source: "/srv/new".into(),
        target: "/mnt/new".into(),
    })
}

#[test]
fn a_programs_own_effect_in_one_world_with_seams() {
    let mut world = seam_test::world![fx::Now, fx::Sleep, BindMount, Unmount];
    world
        .load_world(
            r#"
            clock now=1
            mounts { bind "/srv/old" "/mnt/old"; bind "/srv/keep" "/mnt/keep" }
            "#,
        )
        .unwrap();
    remount(&mut world).unwrap();
    nap(&mut world, 1);
    world
        .check_world(
            r#"
            clock now=2
            mounts { bind "/srv/keep" "/mnt/keep"; bind "/srv/new" "/mnt/new" }
            "#,
        )
        .unwrap();
    let mismatch = world
        .check_world(r#"mounts { bind "/srv/old" "/mnt/old" }"#)
        .unwrap_err();
    assert_eq!(
        mismatch.failures[0].message,
        r#"expected `bind "/srv/old" "/mnt/old"`, found nothing"#
    );
    assert_eq!(
        world.to_world(),
        "clock now=2 wall=2\nmounts {\n    bind \"/srv/keep\" \"/mnt/keep\"\n    bind \"/srv/new\" \"/mnt/new\"\n}\n"
    );
}

// ---- a world inside a root, beside other parts ----

struct Fixed;
impl Capability for Fixed {}
impl Perform<BindMount> for Fixed {
    fn perform(&mut self, _: BindMount) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn a_world_is_a_part_of_a_root() {
    use seam_test::sim::World;
    let world = seam_test::world![fx::Now, fx::Sleep];
    let mut root = seam_core::root![
        World < fx::Now,
        fx::Sleep > ::clone(&world),
        Fixed<BindMount>,
    ];
    assert_eq!(nap(&mut root, 7), Duration::from_secs(7));
    root.perform(BindMount {
        source: "a".into(),
        target: "b".into(),
    })
    .unwrap();
    // The test's own handle sees what the root's part did.
    world.check_world("clock now=7").unwrap();
}

// ---- a simulated effect built from other simulated effects ----

/// Stores a value under a key.
#[cfg(feature = "fs")]
struct Put(String, String);
#[cfg(feature = "fs")]
impl Operation for Put {
    type Outcome = io::Result<()>;
}
#[cfg(feature = "fs")]
impl Effect for Put {
    type Key = Put;
}

/// Reads a value back.
#[cfg(feature = "fs")]
struct Get(String);
#[cfg(feature = "fs")]
impl Operation for Get {
    type Outcome = io::Result<String>;
}
#[cfg(feature = "fs")]
impl seam_core::Query for Get {}
#[cfg(feature = "fs")]
impl Effect for Get {
    type Key = Get;
}

/// The store keeps its values in the simulated filesystem, so it has no
/// state of its own: its handlers perform the filesystem's effects.
#[cfg(feature = "fs")]
impl Simulated for Put {
    type State = seam_test::sim::Stateless;
}
#[cfg(feature = "fs")]
impl Simulate<Put> for Put {
    fn run(states: &mut States, op: Put) -> io::Result<()> {
        seam_core::ops::fs::write_file(states, &format!("/kv/{}", op.0), &op.1)
    }
}
#[cfg(feature = "fs")]
impl Simulated for Get {
    type State = seam_test::sim::Stateless;
}
#[cfg(feature = "fs")]
impl Simulate<Get> for Get {
    fn run(states: &mut States, op: Get) -> io::Result<String> {
        seam_core::ops::fs::read_to_string(&*states, &format!("/kv/{}", op.0))
    }
}

#[cfg(feature = "fs")]
#[test]
fn a_simulated_effect_built_from_others() {
    let mut world = seam_test::world![fx::View, Put, Get];
    world.load_world(r#"fs { dir "/kv" }"#).unwrap();
    world.perform(Put("k".into(), "v".into())).unwrap();
    assert_eq!(world.inspect(Get("k".into())).unwrap(), "v");
    // The value is a file of the world's filesystem, as a world file shows.
    world.check_world(r#"fs { file "/kv/k" "v" }"#).unwrap();
}
