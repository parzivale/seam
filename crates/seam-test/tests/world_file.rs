//! World files: loading every directive, ordering, errors, expectations.
#![cfg(feature = "full")]

use seam_core::io;
use seam_core::ops::fs::write_file;
use seam_core::ops::io::read_to_end;
use seam_core::ops::net::resolve;
use seam_core::ops::random::random_u64;
use seam_core::ops::stdio::{OpenStdin, print};
use std::net::IpAddr;
use std::process::Command;
use std::time::{Duration, UNIX_EPOCH};

use seam_core::ops::env::{Args, CurrentDir, TempDir, Var};
use seam_core::ops::fs::{NextEntry, ReadDir};
use seam_core::ops::process::RunCommand;
use seam_core::ops::signal::CtrlC;

use seam_core::ops::time::{Now, Sleep, WallClock};
use seam_core::{Inspect, Perform, Provide};
use seam_test::Sim;

/// Lists `dir` through `ReadDir`, one entry at a time.
fn list<C, D>(c: &mut C, dir: impl AsRef<std::path::Path>) -> io::Result<Vec<std::path::PathBuf>>
where
    C: for<'a> seam_core::Provide<ReadDir<'a, D>>,
    D: seam_core::Perform<NextEntry>,
{
    seam_core::Provide::acquire(c, ReadDir::new(dir.as_ref()), |_, entries| {
        let mut paths = Vec::new();
        while let Some(path) = seam_core::Perform::perform(entries, NextEntry)? {
            paths.push(path);
        }
        Ok(paths)
    })
}

const WORLD: &str = r#"
// A world with one of everything.
seed 7
parallelism 4
clock now=5 wall=1767225600

env {
    var USER ada
    var GREETING "hello there"
    args word_count "/a.txt" "/my docs/b.txt"
    cwd "/home/ada"
    temp "/scratch"
}

fs {
    dir "/home/ada" {
        file notes.txt "buy milk"
        dir projects {
            file seam.md """
                sans-effect
                  indented

                """
        }
    }
    dir "/empty"
    file "/data/blob.bin" hex="00ff 10ab"
}

net { host example.com "93.184.216.34" }

process {
    command git exit=0 stdout="clean\n"
    command make test exit=2 stderr="1 failed\n"
}

stdio {
    stdin "y\n"
    stdin "n\n"
}

signal { ctrl-c 2 }
"#;

#[test]
fn loads_every_directive() {
    let mut sim = Sim::from_world(WORLD).unwrap();

    assert_eq!(random_u64(&mut sim), random_u64(&mut Sim::with_seed(7)));
    assert_eq!(
        sim.inspect(seam_core::ops::task::AvailableParallelism)
            .get(),
        4
    );
    assert_eq!(sim.inspect(Now).since_epoch(), Duration::from_secs(5));
    assert_eq!(
        sim.inspect(WallClock),
        UNIX_EPOCH + Duration::from_secs(1_767_225_600)
    );

    assert_eq!(sim.inspect(Var::new("USER")).unwrap(), "ada");
    assert_eq!(sim.inspect(Var::new("GREETING")).unwrap(), "hello there");
    assert_eq!(
        sim.inspect(Args),
        ["word_count", "/a.txt", "/my docs/b.txt"]
    );
    assert_eq!(
        sim.inspect(CurrentDir).unwrap(),
        std::path::Path::new("/home/ada")
    );
    assert_eq!(sim.inspect(TempDir), std::path::Path::new("/scratch"));

    assert_eq!(sim.file("/home/ada/notes.txt").unwrap(), b"buy milk");
    assert_eq!(
        sim.file("/home/ada/projects/seam.md").unwrap(),
        b"sans-effect\n  indented\n",
        "dedented by the closing quotes; the blank line gives a final newline"
    );
    assert!(sim.exists("/empty"));
    assert_eq!(
        sim.file("/data/blob.bin").unwrap(),
        [0x00, 0xff, 0x10, 0xab]
    );

    let ip: IpAddr = "93.184.216.34".parse().unwrap();
    let resolved = resolve(&sim, "example.com", 80).unwrap();
    assert_eq!(resolved[0].ip(), ip);

    let out = sim.perform(RunCommand(Command::new("git"))).unwrap();
    assert_eq!(
        (out.code, out.stdout.as_slice()),
        (Some(0), &b"clean\n"[..])
    );
    let mut make = Command::new("make");
    make.arg("test");
    let out = sim.perform(RunCommand(make)).unwrap();
    assert_eq!(
        (out.code, out.stderr.as_slice()),
        (Some(2), &b"1 failed\n"[..])
    );
    let mut other = Command::new("make");
    other.arg("build");
    let err = sim.perform(RunCommand(other)).unwrap_err();
    assert_eq!(
        err.kind(),
        io::ErrorKind::NotFound,
        "only `make test` is scripted"
    );

    let mut stdin = Vec::new();
    sim.acquire(OpenStdin::<seam_test::stdio::Stdin>::new(), |_, h| {
        read_to_end(h, &mut stdin)
    })
    .unwrap();
    assert_eq!(stdin, b"y\nn\n");
    assert!(sim.perform(CtrlC).is_ok());
    assert!(sim.perform(CtrlC).is_ok());
    assert!(sim.perform(CtrlC).is_err(), "exactly two were raised");
}

#[test]
fn later_wins_and_worlds_layer() {
    let base = r#"
        env { var MODE base; var KEEP yes }
        process { command git stdout="base\n" }
        fs { file "/etc/app.conf" "port = 80" }
    "#;
    let overrides = r#"
        env { var MODE test }
        process {
            command git stdout="override\n"
            command git status stdout="clean\n"
        }
        fs { file "/etc/app.conf" "port = 8080" }
    "#;
    let mut sim = Sim::from_world(base).unwrap();
    sim.load_world(overrides).unwrap();
    assert_eq!(sim.inspect(Var::new("MODE")).unwrap(), "test");
    assert_eq!(sim.inspect(Var::new("KEEP")).unwrap(), "yes");
    assert_eq!(sim.file("/etc/app.conf").unwrap(), b"port = 8080");

    let out = sim.perform(RunCommand(Command::new("git"))).unwrap();
    assert_eq!(out.stdout, b"override\n", "the later response wins");
    let mut status = Command::new("git");
    status.arg("status");
    let out = sim.perform(RunCommand(status)).unwrap();
    assert_eq!(out.stdout, b"clean\n", "exact arguments win over any");
}

#[test]
fn order_of_files_does_not_change_listings() {
    let mut a = Sim::from_world(r#"fs { file "/d/b" "2"; file "/d/a" "1" }"#).unwrap();
    let mut b = Sim::from_world(r#"fs { file "/d/a" "1"; file "/d/b" "2" }"#).unwrap();
    assert_eq!(list(&mut a, "/d").unwrap(), list(&mut b, "/d").unwrap());
}

fn load_error(text: &str) -> String {
    match Sim::from_world(text) {
        Ok(_) => panic!("the world should not load: {text}"),
        Err(e) => e.to_string(),
    }
}

#[test]
fn errors_say_where_and_what() {
    assert_eq!(
        load_error("env {\n    var USER ada\n}\nfs {\n    fil \"/a\" \"x\"\n}"),
        "line 5, column 5: unknown `fil` in `fs` (did you mean `file`?)"
    );
    assert_eq!(
        load_error("stdo {}"),
        "line 1, column 1: unknown `stdo` in the world (did you mean `stdio`?)"
    );
    assert!(
        load_error("world {}").contains("(expected one of: `seed`, `parallelism`, `clock`, `env`"),
        "with nothing close, every option is listed"
    );
    assert_eq!(
        load_error("fs { file /a \"x\" }"),
        "line 1, column 11: a bare word cannot contain `/` in KDL: quote paths, as in \"/home/ada\""
    );
    assert!(load_error("env { var USER }").contains("`var` takes a name and a value"));
    assert!(load_error("fs { dir \"/a\" { file \"/b\" \"x\" } }").contains("relative"));
    assert!(load_error("fs { file \"/a\" }").contains("`file` takes a path and its contents"));
    assert!(load_error("fs { file \"/a\" \"x\" hex=\"00\" }").contains("not both"));
    assert!(load_error("fs { file \"/a\" hex=\"0\" }").contains("even number"));
    assert!(load_error("net { host example.com nowhere }").contains("is not an IP address"));
    assert!(load_error("process { command git exit=\"zero\" }").contains("must be an integer"));
    assert!(load_error("process { command git code=1 }").contains("unknown property `code`"));
    assert!(load_error("clock now=1.5").contains("only integers"));
    assert!(load_error("seed -1").contains("out of range"));
    assert!(load_error("fs { file \"/a\" \"x\" }\nfs {").contains("line 2"));
    assert!(load_error("fs { file \"/tmp\" \"x\" }").contains("cannot create `/tmp`"));
}

#[test]
fn expectations_pass_when_the_world_matches() {
    let mut sim = Sim::from_world(r#"fs { dir "/out" }"#).unwrap();
    write_file(&mut sim, "/out/report.txt", "total 6\n").unwrap();
    print(&mut sim, b"done\n").unwrap();
    sim.perform(Sleep(Duration::from_secs(3)));
    sim.on_command("git", |_| Ok(Default::default()));
    let mut status = Command::new("git");
    status.arg("status");
    sim.perform(RunCommand(status)).unwrap();

    sim.assert_world(
        r#"
        clock now=3
        fs {
            dir "/out" {
                file report.txt "total 6\n"
            }
            absent "/out/scratch"
        }
        stdio { stdout "done\n" }
        process { ran git status }
        "#,
    );
    sim.assert_world("fs { file \"/out/report.txt\" \"total 6\\n\" }"); // a small expectation
}

#[test]
fn every_failure_is_reported_with_its_line() {
    let mut sim = Sim::new();
    write_file(&mut sim, "/tmp/out", "total 5\n").unwrap();
    print(&mut sim, b"oops\n").unwrap();
    sim.on_command("git", |_| Ok(Default::default()));
    sim.perform(RunCommand(Command::new("git"))).unwrap();
    sim.perform(RunCommand(Command::new("git"))).unwrap();

    let mismatch = sim
        .check_world(
            "fs {\n  file \"/tmp/out\" \"total 6\\n\"\n  absent \"/tmp\"\n  dir \"/tmp/missing\"\n}\nstdio { stdout \"done\\n\" }\nprocess { ran git status }\nclock now=9\n",
        )
        .unwrap_err();
    let lines: Vec<usize> = mismatch.failures.iter().map(|f| f.line).collect();
    assert_eq!(lines, [2, 3, 4, 6, 7, 7, 8]);
    let text = mismatch.to_string();
    for part in [
        "line 2: `/tmp/out`: expected \"total 6\\n\", found \"total 5\\n\"",
        "line 3: `/tmp`: expected nothing there, but it exists",
        "line 4: `/tmp/missing`: expected a directory, but nothing is there",
        "line 6: stdout: expected \"done\\n\", found \"oops\\n\"",
        "line 7: command 1: expected `git status`, but it ran `git`",
        "line 7: more commands ran than expected: `git`",
        "line 8: clock: expected 9s since the start, found 0ns",
    ] {
        assert!(text.contains(part), "missing {part:?} in:\n{text}");
    }
}

/// Every block can be checked, as a subset of the world: the network's
/// hosts too.
#[cfg(feature = "net")]
#[test]
fn network_expectations_are_a_subset() {
    let mut sim = Sim::new();
    sim.add_host("example.com", std::net::IpAddr::from([93, 184, 216, 34]));
    sim.check_world(r#"net { host example.com "93.184.216.34" }"#)
        .unwrap();
    let mismatch = sim
        .check_world(r#"net { host example.com "1.2.3.4" }"#)
        .unwrap_err();
    assert_eq!(
        mismatch.failures[0].message,
        r#"expected `host example.com "1.2.3.4"`, found `host example.com "93.184.216.34"`"#
    );
}

#[test]
fn invalid_expectations_are_reported_too() {
    let sim = Sim::new();
    let mismatch = sim.check_world("env { set USER ada }").unwrap_err();
    assert!(
        mismatch
            .to_string()
            .contains("invalid expectations (column 7): unknown `set` in `env` expectations"),
        "{mismatch}"
    );
    let mismatch = sim.check_world("fs { fiel \"/a\" \"x\" }").unwrap_err();
    assert!(
        mismatch.to_string().contains("did you mean `file`?"),
        "{mismatch}"
    );
}

#[test]
#[should_panic(expected = "line 1: `/nope`: expected a file, but nothing is there")]
fn assert_world_panics_with_the_failures() {
    Sim::new().assert_world("fs { file \"/nope\" \"x\" }");
}

/// The examples in the module docs, verbatim.
#[test]
fn the_documented_examples_load() {
    let src = include_str!("../src/world_file.rs");
    let blocks: Vec<String> = src
        .split("//! ```kdl\n")
        .skip(1)
        .map(|rest| {
            rest.split("//! ```\n")
                .next()
                .unwrap()
                .lines()
                .map(|l| {
                    l.strip_prefix("//! ")
                        .or(l.strip_prefix("//!"))
                        .unwrap_or(l)
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .collect();
    assert_eq!(blocks.len(), 2, "a world and its expectations");
    let sim = Sim::from_world(&blocks[0]).unwrap();
    assert_eq!(
        sim.file("/home/ada/projects/seam.md").unwrap(),
        b"sans-effect"
    );
    // The expectations parse; this world does not meet them.
    let mismatch = sim.check_world(&blocks[1]).unwrap_err();
    assert!(
        mismatch
            .failures
            .iter()
            .all(|f| !f.message.starts_with("invalid")),
        "{mismatch}"
    );
}

#[test]
fn env_expectations_check_what_a_core_changed() {
    use seam_core::Perform;
    use seam_core::ops::env::{RemoveVar, SetVar};

    let mut sim = Sim::from_world("env { var MODE start; var LOCK yes }").unwrap();
    sim.perform(SetVar::new("MODE", "done")).unwrap();
    sim.perform(RemoveVar::new("LOCK")).unwrap();
    sim.assert_world("env { var MODE done; unset LOCK; unset NEVER }");

    let mismatch = sim
        .check_world("env {\n    var MODE start\n    unset MODE\n    var LOCK yes\n}")
        .unwrap_err();
    let failures: Vec<_> = mismatch
        .failures
        .iter()
        .map(|f| (f.line, f.message.as_str()))
        .collect();
    assert_eq!(
        failures,
        [
            (2, "env: expected MODE \"start\", found \"done\""),
            (3, "env: expected MODE unset, found \"done\""),
            (4, "env: expected LOCK \"yes\", found unset"),
        ]
    );
}

// ---- states of a program's own, each reading its own blocks ----

use seam_core::{Effect, Operation, effects as fx};
use seam_test::sim::{SimState, Simulate, Simulated, States};
use seam_test::world_file::{Node, WorldError};

struct Bind(String, String);
impl Operation for Bind {
    type Outcome = ();
}
impl Effect for Bind {
    type Key = Bind;
}

/// A fake mount table, with a `mounts` block of its own.
#[derive(Default)]
struct Mounts(Vec<(String, String)>);

fn pair(node: &Node) -> Result<(String, String), WorldError> {
    node.arity(2, Some(2), "a source and a target")?;
    Ok((
        node.args()[0].string("the source")?.to_owned(),
        node.args()[1].string("the target")?.to_owned(),
    ))
}

impl SimState for Mounts {
    const BLOCKS: &'static [&'static str] = &["mounts"];

    fn load(&mut self, block: &Node) -> Result<(), WorldError> {
        for node in block.block()? {
            match node.name() {
                "bind" => self.0.push(pair(node)?),
                _ => return Err(node.unknown("`mounts`", &["bind"])),
            }
        }
        Ok(())
    }

    fn to_world(&self) -> Vec<Node> {
        let mut block = Node::new("mounts");
        for (from, to) in &self.0 {
            block = block.with_child(Node::new("bind").with_arg(from).with_arg(to));
        }
        vec![block]
    }
}

impl Simulated for Bind {
    type State = Mounts;
}
impl Simulate<Bind> for Bind {
    fn run(states: &mut States, op: Bind) {
        states.state::<Mounts>().0.push((op.0, op.1));
    }
}

#[cfg(all(feature = "fs", feature = "env", feature = "io-std"))]
#[test]
fn one_world_file_sets_up_and_checks_every_state() {
    let mut world = seam_test::world![fx::Var, fx::View, fx::OpenStdout, Bind];
    world
        .load_world(
            r#"
            env { var USER ada }
            fs { file "/etc/binds" "x" }
            mounts { bind "/srv/a" "/mnt/a" }
            "#,
        )
        .unwrap();
    assert_eq!(world.file("/etc/binds").unwrap(), b"x");
    world.state(|m: &mut Mounts| assert_eq!(m.0.len(), 1));

    world.perform(Bind("/srv/b".into(), "/mnt/b".into()));
    world
        .check_world(
            r#"
            fs { file "/etc/binds" "x" }
            mounts { bind "/srv/a" "/mnt/a"; bind "/srv/b" "/mnt/b" }
            "#,
        )
        .unwrap();

    // `Mounts` has no `check` of its own: the default is a subset of its
    // `to_world`.
    let mismatch = world
        .check_world("mounts { bind \"/srv/c\" \"/mnt/c\" }")
        .unwrap_err();
    assert_eq!(
        mismatch.failures[0].message,
        "expected `bind \"/srv/c\" \"/mnt/c\"`, found nothing"
    );
}

#[cfg(feature = "fs")]
#[test]
fn a_block_no_state_reads_is_an_error() {
    let mut world = seam_test::world![fx::View];
    let err = world.load_world("mounts { }").unwrap_err();
    assert_eq!(
        err.message,
        "unknown `mounts` in the world (expected one of: `fs`)"
    );
    let mut world = seam_test::world![fx::View, Bind];
    let err = world.load_world("mount { }").unwrap_err();
    assert!(err.message.contains("did you mean `mounts`?"), "{err}");
}

/// Another state that reads `mounts`.
#[derive(Default)]
struct Rival;
impl SimState for Rival {
    const BLOCKS: &'static [&'static str] = &["mounts"];
    fn load(&mut self, _: &Node) -> Result<(), WorldError> {
        Ok(())
    }
    fn to_world(&self) -> Vec<Node> {
        Vec::new()
    }
}

struct Contest;
impl Operation for Contest {
    type Outcome = ();
}
impl Effect for Contest {
    type Key = Contest;
}
impl Simulated for Contest {
    type State = Rival;
}

#[test]
fn two_states_reading_one_block_is_an_error() {
    let mut world = seam_test::world![Bind, Contest];
    let err = world.load_world("mounts { }").unwrap_err();
    assert_eq!(err.message, "two parts of the world read `mounts`");
}

/// The whole simulated world, written out, loads back to the same world.
#[cfg(all(
    feature = "fs",
    feature = "env",
    feature = "net",
    feature = "process",
    feature = "random",
    feature = "signal",
    feature = "io-std",
    feature = "time",
    feature = "rt"
))]
#[test]
fn a_sim_round_trips_through_to_world() {
    let mut sim = Sim::from_world(
        r#"
        seed 7
        parallelism 4
        clock now=3 wall=1000
        env { var USER ada; args app "-v"; cwd "/home/ada" }
        fs { dir "/home/ada" { file notes.txt "buy milk" }; file "/bin.dat" hex="00ff" }
        net { host example.com "93.184.216.34" }
        process { command git status exit=0 stdout="clean\n" }
        stdio { stdin "y\n" }
        signal { ctrl-c 2 }
        "#,
    )
    .unwrap();
    sim.perform(seam_core::ops::process::RunCommand({
        let mut cmd = std::process::Command::new("git");
        cmd.arg("status");
        cmd
    }))
    .unwrap();
    let text = sim.to_world();
    let mut copy = Sim::new();
    copy.load_world(&text).unwrap();
    assert_eq!(copy.to_world(), text);
    copy.check_world(r#"process { ran git status }"#).unwrap();
}
