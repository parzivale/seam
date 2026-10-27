# Testing

A simulated *world* implements seam's operations without touching the
real one. `seam::test::Sim` is the world of every built-in effect, and
`seam::test::world![..]` builds one of just the effects listed (yours
among them). A test sets the world up, hands it to the core in place of a
real backend, and checks what the core left behind. Its only real effect is
allocation.

## Determinism

Everything in a `Sim` is decided by its seed and the calls made on it. The
same seed and the same calls give the same random numbers, the same clock
readings and the same results, in the same order, on every machine.

```rust
# #[cfg(all(feature = "test", feature = "random"))] {
use seam::ops::random::random_u64;
use seam::test::Sim;

let a = random_u64(&mut Sim::with_seed(7));
let b = random_u64(&mut Sim::with_seed(7));
assert_eq!(a, b);
# }
```

## Worlds, and what they are made of

Each effect is simulated on its own key, against a *state* of the world:
`Now` and `Sleep` against a clock, the filesystem's operations against an
in-memory tree, and so on. A world holds one state of each type, so
effects that share one (`Now` and `Sleep`) see the same value.

| State | Its blocks | Its effects |
|---|---|---|
| `Clock` | `clock` | `Now`, `Sleep`, `WallClock` |
| `Random` | `seed` | `FillRandom` |
| `Env` | `env` | `Var`, `SetVar`, `RemoveVar`, `Args`, `CurrentDir`, `TempDir` |
| `Files` | `fs` | `Metadata`, `CreateDir`, `RemoveFile`, `RemoveDir`, `Rename`, `CopyFile`, `Open`, `CreateAtomic`, `View`, `ReadDir` |
| `Net` | `net` | `ResolveInto`, `TcpBind`, `TcpConnect`, `UdpBind` |
| `Stdio` | `stdio` | `OpenStdin`, `OpenStdout`, `OpenStderr` |
| `Processes` | `process` | `RunCommand`, `SpawnChild`, `Exit` |
| `Signals` | `signal` | `CtrlC` |
| `Tasks` | `parallelism` | `AvailableParallelism` |

`Sim` has all of them. A world of only some effects is a precise grant: a
core given `world![fx::Now, fx::Sleep]` cannot touch a file, and if it
tries, it fails to compile. `world.state(|clock: &mut Clock| ..)` reaches
a state directly.

## Arranging and asserting

Every world has plain methods, outside the capability system, for the
test's own use. Each group needs its seam feature.

| Group | Arrange | Assert |
|---|---|---|
| time | `advance`, `set_wall_clock` | inspect `Now`, `WallClock` |
| random | `with_seed`, `set_seed` | |
| fs | `add_file`, `add_dir` | `file`, `exists` |
| env | `set_var`, `remove_var`, `set_args`, `set_current_dir`, `set_temp_dir` | inspect `Var` |
| net | `add_host` | connect to what the core bound |
| io-std | `feed_stdin` | `stdout`, `stderr`, `take_stdout`, `take_stderr` |
| process | `on_command`, `on_command_with_stdin`, `script_command` | `invocations` |
| signal | `raise_ctrl_c` | |
| rt | `set_parallelism` | |

The setters return `&mut World`, so they chain. `Sim::run(core)` runs a
closure on the world and returns it, for one-line tests:

```rust
# #[cfg(all(feature = "test", feature = "fs"))] {
use seam::ops::fs::write_file;
use seam::test::Sim;

let sim = Sim::new().run(|sim| write_file(sim, "/tmp/out", "ok").unwrap());
assert_eq!(sim.file("/tmp/out").unwrap(), b"ok");
# }
```

A fresh world's clock reads `Instant::EPOCH`, its wall clock the Unix
epoch, its current directory is `/`, and its filesystem holds `/` and
`/tmp`.

## What the simulation does

- **Time.** A virtual monotonic clock and wall clock. `Sleep` advances both
  and returns at once.
- **Files.** An in-memory tree. Errors match a real system (`NotFound`,
  `IsADirectory`, `NotADirectory`, `DirectoryNotEmpty`, ...), handles
  survive renames as on Unix, an atomic file's temporary file is visible
  until its scope ends, and directories list in name order. No symlinks or
  permissions.
- **Network.** In-memory TCP and UDP, with a hosts table for name
  resolution.
- **Environment and stdio.** Configured variables, arguments and
  directories; scripted stdin, captured stdout and stderr.
- **Processes.** Commands are answered by scripted handlers and logged; a
  command with no handler fails with `NotFound`. `Exit` unwinds instead of
  ending the test, and `catch_exit` turns it back into a code.
- **Signals.** `raise_ctrl_c` queues a Ctrl-C for `CtrlC` to take.

```rust
# #[cfg(all(feature = "test", feature = "process"))] {
use std::process::Command;
use seam::prelude::*;
use seam::ops::process::{Exit, Output, RunCommand};
use seam::test::{Sim, catch_exit};

let mut sim = Sim::new();
sim.on_command("git", |_cmd| {
    Ok(Output { code: Some(0), stdout: b"clean\n".to_vec(), stderr: Vec::new() })
});
let out = sim.perform(RunCommand(Command::new("git"))).unwrap();
assert_eq!(out.stdout, b"clean\n");
assert_eq!(sim.invocations()[0].program, "git");

assert_eq!(catch_exit(|| match sim.perform(Exit(3)) {}), Err(3));
# }
```

## Nothing waits

Used directly, a `Sim` never blocks. An operation that would wait on a real
system (reading an empty socket, accepting with no pending connection,
waiting for a Ctrl-C that was never raised) fails with
`io::ErrorKind::WouldBlock`, and a timed one expires at once. For cores
whose tasks or threads wait on each other, run them on the simulated
runtime or threads, where those operations suspend until another task makes
them possible: see [the async guide](crate::_docs::guide::async_and_concurrency).

## World files

A world file describes the starting world, what it should look like
afterwards, or a snapshot of it, in KDL. Each block goes to the state that
reads it:

```rust
# #[cfg(all(feature = "test", feature = "fs", feature = "env", feature = "io-std"))] {
use seam::test::Sim;

let mut sim = Sim::from_world(r#"
    env { var USER ada }
    fs { dir "/home/ada" { file notes.txt "buy milk" } }
"#).unwrap();
seam::ops::fs::write_file(&mut sim, "/home/ada/done.txt", "milk").unwrap();
sim.assert_world(r#"
    fs {
        file "/home/ada/done.txt" "milk"
        file "/home/ada/notes.txt" "buy milk"
    }
"#);
# }
```

`check_world` returns every failed expectation with its line, and
`assert_world` panics with them. `to_world` writes the whole world out in
the same format, for a snapshot or a diff. See
[the format](crate::_docs::world_files) for every directive.

## Simulating your own effects

A program's own effect is simulated the same way as seam's: two sister
traits on its key (`Simulated`, naming its state, and `Simulate<O>`,
running an operation), and a state that reads and writes its own
world-file blocks. List it in a world beside seam's effects, and one world
file sets up and checks both. A simulated effect can also be built from
others: its handler performs seam's simulated effects through the
`States` it is given. See [effects of your own](crate::_docs::guide::your_own_effects).

## Checking a fake against the real thing

Effect laws (writing a file then reading it gives back what was written;
`put` then `get` returns the value) are checked as tests. Writing the test
once, generically, and running it on a real backend in a temporary
directory and on the `Sim` checks both, and keeps the simulation honest:

```rust,ignore
fn write_then_read<C, F, W>(c: &mut C, path: &str)
where
    C: for<'a> Lend<View<'a, F>> + for<'a> Provide<Open<'a, WriteOnly, W>>,
    F: for<'a> Inspect<ReadAt<'a>>,
    W: for<'a> Perform<Write<'a>>,
{
    write_file(c, path, "hello").unwrap();
    assert_eq!(read_to_string(c, path).unwrap(), "hello");
}

write_then_read(&mut Sim::new(), "/tmp/law");
write_then_read(&mut Std, temp_dir.join("law").to_str().unwrap());
```

## Keeping tests fast

Simulated time costs nothing, so a test of a retry loop with an hour of
backoff runs instantly, and a test of a race sweeps dozens of seeds in
milliseconds. Prefer many small seeded runs over one long one: a failure
then names the seed that reproduces it.
