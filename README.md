# seam

Sans-effect programming for Rust.

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
| [`seam-core`](crates/seam-core) | The traits (`Operation`, `Query`, `Inspect`, `Perform`, `Finalize`, `Scoped`, async variants, `block_on`), the operation types, and a `prelude`. `no_std`. |
| [`seam-std`](crates/seam-std) | The std backend: a `Std` root capability and newtype handles (`File`, `TcpStream`, ...) that expose no std I/O traits. |
| [`seam-tokio`](crates/seam-tokio) | The Tokio backend: a `Tokio` root capability implementing the async and `Send` capability traits. |
| [`seam-test`](crates/seam-test) | The pure test backend: a `Sim` world with a virtual clock, seeded randomness, in-memory filesystem and network, scripted commands and captured stdio. Its only real effect is allocation. |
| [`seam`](crates/seam) | Re-exports everything, behind feature flags. `no_std` by default. |

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
| `rt` | `Spawn`, `Join`, `Abort`, `IsFinished`, `YieldNow`. No std needed. |
| `alloc` | `BoxFuture` and `Spawn::boxed`, for tasks of unnameable type. |
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

Nothing in the simulation waits: `Sleep` advances the virtual clock, and an
operation that would block (an empty socket, no pending connection) fails
with `WouldBlock`. The same seed and calls always give the same results.

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
| [`backup`](crates/seam/examples/backup.rs) | filesystem, clock and arguments; bundling many capabilities into one trait |
| [`echo_server`](crates/seam/examples/echo_server.rs) | an async `Send` core on Tokio that spawns a task per connection; its handler is tested on the in-memory network with no runtime |
| [`custom_ops`](crates/seam/examples/custom_ops.rs) | defining your own operations; building a capability out of other capabilities |

```text
cargo run  -p seam --example guess --features full,std,tokio,rt,test
cargo test -p seam --examples --features full,std,tokio,rt,test
```

## Design

See [DESIGN.md](DESIGN.md). API documentation: `cargo doc --open --all-features`.
