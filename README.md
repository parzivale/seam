# seam

Sans-effect programming for Rust.

Sans-IO pulls I/O out of protocol logic. `seam` pulls out **every** effect:
I/O, time, randomness, shared state. The core of your program is written
against capability bounds and can be unit tested, or run, with no effects at
all. Real implementations or test fakes are plugged in at the edge, the seam.

A function's bounds are an audit of what it can do:

```rust
use seam::Perform;
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
| [`seam-core`](crates/seam-core) | The traits (`Operation`, `Query`, `Inspect`, `Perform`, `Finalize`, `Scoped`, async variants, `block_on`) and the operation types. `no_std`. |
| [`seam-std`](crates/seam-std) | The std backend: a `Std` root capability and newtype handles (`File`, `TcpStream`, ...) that expose no std I/O traits. |
| [`seam-tokio`](crates/seam-tokio) | The Tokio backend: a `Tokio` root capability implementing the async and `Send` capability traits. |
| [`seam-test`](crates/seam-test) | The pure test backend: a `Sim` world with a virtual clock, seeded randomness, in-memory filesystem and network, scripted commands and captured stdio. Its only real effect is allocation. |
| [`seam`](crates/seam) | Re-exports everything, behind feature flags. `no_std` by default. |

## Features

`seam` has no default features: you get the traits and `Push`, with no std.

| Feature | Gives you |
|---|---|
| `time`, `random` | `Sleep`, `RandomU64`, `FillRandom`. No std needed. |
| `fs`, `net`, `process`, `env`, `io-std` | Those operation types. Pulls in std and the std backend. |
| `std` | The std backend, `seam::stdlib`. |
| `tokio` | The Tokio backend, `seam::tokio`. |
| `test` | The pure test backend, `seam::test`. No std on its own. |
| `signal` | `CtrlC`, on the Tokio and test backends. |
| `rt`, `rt-multi-thread`, `test-util` | Tokio-only, named as in Tokio. |
| `full` | Everything. |

A backend implements exactly the capability groups that are enabled, so
`features = ["tokio", "net", "time"]` gives you Tokio-backed networking and
clocks and nothing else.

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

## Design

See [DESIGN.md](DESIGN.md).
