# seam

Sans-effect programming for Rust.

```toml
[dependencies]
seam = { package = "seam-rs", version = "0.1", features = ["fs", "std"] }
```

The crates are published as `seam-rs`, `seam-rs-core`, `seam-rs-std`,
`seam-rs-tokio` and `seam-rs-test`; in code they are still `seam`,
`seam_core`, and so on.

Sans-IO pulls I/O out of protocol logic. `seam` pulls out **every** effect:
I/O, time, randomness, shared state. The core of your program is written
against capability bounds and can be unit tested, or run, with no effects at
all. Real implementations or test fakes are plugged in at the edge, the seam.

A function's bounds are an audit of what it can do:

```rust
use seam::prelude::*;
use seam::ops::random::RandomU64;
use seam::ops::time::Sleep;
use core::time::Duration;

/// Can sleep and draw random numbers. Nothing else.
fn jitter<C: Perform<RandomU64> + Perform<Sleep>>(c: &mut C) {
    let ms = c.perform(RandomU64) % 100;
    c.perform(Sleep(Duration::from_millis(ms)));
}
```

## Crates

| Crate | What it is |
|---|---|
| [`seam-core`](crates/seam-core) (`seam-rs-core`) | The traits (`Operation`, `Query`, `Inspect`, `Perform`, `Provide`/`Acquire` and their async forms, `block_on`), the operation and acquisition types, and a `prelude`. `no_std`. |
| [`seam-std`](crates/seam-std) (`seam-rs-std`) | The std backend: a `Std` root capability and newtype handles (`File`, `TcpStream`, ...) that expose no std I/O traits. |
| [`seam-tokio`](crates/seam-tokio) (`seam-rs-tokio`) | The Tokio backend: a `Tokio` root capability implementing the async and `Send` capability traits. |
| [`seam-test`](crates/seam-test) (`seam-rs-test`) | The pure test backend: a `Sim` world with a virtual clock, seeded randomness, in-memory filesystem and network, scripted commands and captured stdio. Its only real effect is allocation. |
| [`seam`](crates/seam) (`seam-rs`) | Re-exports everything, behind feature flags. `no_std` by default. |

## Features

`seam` has no default features: you get the traits and `Push`, with no std.

Features come in two independent kinds. Capability groups choose which
operations exist; backends choose where effects really happen. **No
capability group turns on a backend**: that is always the end user's
choice. A library can depend on `seam` with just the groups its core needs,
and only the binary (or its tests) picks a backend.

| Capability group | Gives you |
|---|---|
| `time`, `random` | `Sleep`, `Now`, `RandomU64`, `FillRandom`. No std needed. |
| `rt` | `Spawn`, `SpawnSet`, `SpawnWith`, `Join`, `Detach`, `YieldNow`. No std or alloc needed. |
| `alloc` | `Spawn::boxed` and `SpawnWith::boxed`, for tasks of unnameable type. |
| `fs`, `net`, `process`, `env`, `io-std`, `signal` | Those operation types. Links std for their types. |
| `full` | Every capability group. Still no backend. |

| Backend | Gives you |
|---|---|
| `std` | The std backend, `seam::stdlib`. |
| `tokio` | The Tokio backend, `seam::tokio`. |
| `test` | The pure test backend, `seam::test`. No std on its own. |
| `rt-multi-thread`, `test-util` | Tokio runtime options, named as in Tokio. Only with `tokio`. |

A backend implements exactly the capability groups that are enabled, so
`features = ["tokio", "rt", "net", "time"]` gives you Tokio-backed
networking and clocks and nothing else.

## Testing a core

`seam-test`'s `Sim` stands in for a real backend. Set up the world, run the
core, assert on what it left behind:

```rust
use seam::Perform;
use seam::ops::fs::WriteFile;
use seam::test::Sim;

let mut sim = Sim::with_seed(7);
sim.add_dir("/data");
sim.perform(WriteFile::new("/data/out", "ok")).unwrap(); // your core here
assert_eq!(sim.file("/data/out").unwrap(), b"ok");
```

Used directly, nothing in the simulation waits: `Sleep` advances the
virtual clock, and an operation that would block fails with `WouldBlock`.
The same seed and calls always give the same results.

For concurrent cores, `seam::test::rt::Runtime` runs async code over a `Sim`
on a deterministic executor:

```rust
let rt = Runtime::with_seed(sim, seed);
rt.block_on(|sim| async move { serve(&mut sim.clone(), addr).await /* ... */ });
```

- Blocking operations (reads, accepts, receives, `CtrlC`) suspend until
  another task makes them possible.
- `Sleep` waits on a virtual clock that jumps to the next timer when every
  task is waiting, so hours of timeouts take no time.
- The seed picks which ready task runs next. Sweep seeds to explore
  interleavings and replay a failure from its seed. The tests include a
  lost-update race that only some seeds expose.
- When nothing can ever make progress, `block_on` reports a deadlock
  instead of hanging.
- Tasks (`Spawn`, `SpawnSet`, `SpawnWith`) work as on Tokio, so the
  `echo_server` example's whole accept loop is tested with concurrent
  clients under 16 schedules.

## Resources and cancellation

A core never owns a file, socket or task. It acquires one for the length of
a closure, which borrows it and gets the capability back:

```rust
c.acquire(CreateAtomic::new("settings.conf"), |c, file| {
    file.perform(WriteAll(b"..."))?; // an error, a panic, or (with
    Ok(())                           // acquire_async) cancellation here...
})?;                                 // ...leaves the old file untouched
```

When the body returns `Ok` the resource is committed: a file is synced, an
atomic file renamed into place, a stream shut down cleanly, a task waited
for. Otherwise it is rolled back, and a task is aborted. Providers make
dropping a resource equal rolling it back, so cancelling at any `.await`,
even while opening or closing, never commits half a result. Atomic files on
Tokio are tested by cancelling after every possible number of polls.

A resource reaches a task only through the provider: `SpawnWith` has a
listener accept a connection into a new task that owns it, running a named
`TaskBody`. `SpawnSet` opens a set of tasks to spawn into and collect from,
joined or aborted together. None of the task API allocates in seam itself;
`alloc` only adds boxed conveniences (`Spawn::boxed`, `SpawnWith::boxed`).
Temporary files left by a process that was killed outright are cleaned up
with `SweepAtomic`.

## Keeping the core honest

The newtype handles stop effects leaking out of the backends, but nothing in
the type system stops a core crate from calling `std::fs::read` directly.
[`templates/clippy.toml`](templates/clippy.toml) bans the std and Tokio
effect APIs (and shared-state types); copy it into each crate that should stay
sans-effect.

## Examples

Real programs in [`crates/seam/examples`](crates/seam/examples), each with
tests that run its core in the simulation:

| Example | Shows |
|---|---|
| [`guess`](crates/seam/examples/guess.rs) | randomness and stdio; seeded, scripted tests, and a test of a playing strategy |
| [`backup`](crates/seam/examples/backup.rs) | filesystem, clock and arguments; bundling many capabilities into one trait; an atomic manifest marks complete snapshots, and crashed runs are cleaned up |
| [`echo_server`](crates/seam/examples/echo_server.rs) | an async `Send` core on Tokio that spawns a task per connection; its handler is tested on the in-memory network with no runtime |
| [`custom_ops`](crates/seam/examples/custom_ops.rs) | defining your own operations; building a capability out of other capabilities; atomic writes in a scope |

```text
cargo run  -p seam-rs --example guess --features full,std,tokio,rt,test
cargo test -p seam-rs --examples --features full,std,tokio,rt,test
```

## Design

See [DESIGN.md](DESIGN.md). API documentation: `cargo doc --open --all-features`.
